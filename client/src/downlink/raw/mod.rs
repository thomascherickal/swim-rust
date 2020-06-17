// Copyright 2015-2020 SWIM.AI inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::configuration::downlink::OnInvalidMessage;
use crate::downlink::{
    Command, DownlinkError, DownlinkInternals, DownlinkState, DroppedError, Event, Message,
    Operation, Response, StateMachine, StoppedFuture,
};
use crate::router::RoutingError;
use common::sink::item::{self, ItemSender, ItemSink, MpscSend};
use futures::stream::FusedStream;
use futures::task::{Context, Poll};
use futures::{Stream, StreamExt};
use futures_util::future::ready;
use futures_util::select_biased;
use futures_util::stream::once;
use pin_utils::pin_mut;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use swim_runtime::task::{spawn, TaskHandle};
use tokio::sync::{mpsc, watch};
use tracing::{instrument, trace};

#[cfg(test)]
pub mod tests;

#[derive(Debug, Clone)]
pub struct Sender<S> {
    /// A sink for local actions (sets, insertions, etc.)
    set_sink: S,
    /// The task running the downlink.
    task: Arc<dyn DownlinkInternals>,
}

impl<S> Sender<S> {
    pub(in crate::downlink) fn new(set_sink: S, task: Arc<dyn DownlinkInternals>) -> Sender<S> {
        Sender { set_sink, task }
    }

    pub fn same_sender(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.task, &other.task)
    }

    pub fn is_running(&self) -> bool {
        !self.task.task_handle().is_complete()
    }
}

impl<T> Sender<mpsc::Sender<T>> {
    pub async fn send(&mut self, value: T) -> Result<(), mpsc::error::SendError<T>> {
        self.set_sink.send(value).await
    }
}

impl<'a, T> ItemSink<'a, T> for Sender<mpsc::Sender<T>>
where
    T: Send + 'static,
{
    type Error = DownlinkError;
    type SendFuture = MpscSend<'a, T, DownlinkError>;

    fn send_item(&'a mut self, value: T) -> Self::SendFuture {
        let send: MpscSend<'a, T, DownlinkError> = MpscSend::new(&mut self.set_sink, value);
        send
    }
}

impl<T> Sender<watch::Sender<T>> {
    pub fn send(&mut self, value: T) -> Result<(), watch::error::SendError<T>> {
        self.set_sink.broadcast(value)
    }
}

pub struct Receiver<R> {
    /// A stream of events generated by the downlink.
    pub(in crate::downlink) event_stream: R,
    _task: Arc<dyn DownlinkInternals>,
}

impl<R: Unpin> Unpin for Receiver<R> {}

impl<R: Stream + Unpin> Stream for Receiver<R> {
    type Item = R::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().event_stream.poll_next_unpin(cx)
    }
}

/// Type containing the components of a running downlink.
pub struct RawDownlink<S, R> {
    pub(in crate::downlink) sender: S,
    pub(in crate::downlink) receiver: R,
    pub(in crate::downlink) task: DownlinkTaskHandle,
}

impl<S, R> RawDownlink<S, R> {
    //Private as downlinks should only be created by methods in this module and its children.
    pub(in crate::downlink) fn new(
        set_sink: S,
        event_stream: R,
        task: DownlinkTaskHandle,
    ) -> RawDownlink<S, R> {
        RawDownlink {
            receiver: event_stream,
            sender: set_sink,
            task,
        }
    }

    pub fn split(self) -> (Sender<S>, Receiver<R>) {
        let RawDownlink {
            sender,
            receiver,
            task,
        } = self;
        let send_part = Sender {
            set_sink: sender,
            task: Arc::new(task),
        };
        let receive_part = Receiver {
            event_stream: receiver,
            _task: send_part.task.clone(),
        };
        (send_part, receive_part)
    }
}

/// Asynchronously create a new downlink from a stream of input events, writing to a sink of
/// commands.
pub(in crate::downlink) fn create_downlink<M, A, State, Machine, Updates, Commands>(
    machine: Machine,
    update_stream: Updates,
    cmd_sink: Commands,
    buffer_size: NonZeroUsize,
    yield_after: NonZeroUsize,
    on_invalid: OnInvalidMessage,
) -> RawDownlink<mpsc::Sender<A>, mpsc::Receiver<Event<Machine::Ev>>>
where
    M: Send + 'static,
    A: Send + 'static,
    State: Send + 'static,
    Machine: StateMachine<State, M, A> + Send + 'static,
    Machine::Ev: Send + 'static,
    Machine::Cmd: Send + 'static,
    Updates: Stream<Item = Result<Message<M>, RoutingError>> + Send + 'static,
    Commands: ItemSender<Command<Machine::Cmd>, RoutingError> + Send + 'static,
{
    let (act_tx, act_rx) = mpsc::channel::<A>(buffer_size.get());
    let (event_tx, event_rx) = mpsc::channel::<Event<Machine::Ev>>(buffer_size.get());

    let event_sink = item::for_mpsc_sender::<_, DroppedError>(event_tx);

    let (stopped_tx, stopped_rx) = watch::channel(None);

    let completed = Arc::new(AtomicBool::new(false));

    // The task that maintains the internal state of the lane.
    let task = DownlinkTask::new(
        cmd_sink,
        event_sink,
        completed.clone(),
        stopped_tx,
        on_invalid,
    );

    let lane_task = task.run(
        make_operation_stream(update_stream),
        act_rx.fuse(),
        machine,
        yield_after,
    );

    let join_handle = spawn(lane_task);

    let dl_task = DownlinkTaskHandle {
        join_handle,
        stop_await: stopped_rx,
        completed,
    };

    RawDownlink::new(act_tx, event_rx, dl_task)
}

#[derive(Debug)]
pub(in crate::downlink) struct DownlinkTaskHandle {
    join_handle: TaskHandle<Result<(), DownlinkError>>,
    stop_await: watch::Receiver<Option<Result<(), DownlinkError>>>,
    completed: Arc<AtomicBool>,
}

impl DownlinkTaskHandle {
    pub(in crate::downlink) fn new(
        join_handle: TaskHandle<Result<(), DownlinkError>>,
        stop_await: watch::Receiver<Option<Result<(), DownlinkError>>>,
        completed: Arc<AtomicBool>,
    ) -> DownlinkTaskHandle {
        DownlinkTaskHandle {
            join_handle,
            stop_await,
            completed,
        }
    }

    /// Read the flag indicating whether the downlink is still running.
    pub fn is_complete(&self) -> bool {
        self.completed.load(Ordering::Acquire)
    }

    /// Get a future that will complete when the downlink stops running.
    pub fn await_stopped(&self) -> StoppedFuture {
        StoppedFuture(self.stop_await.clone())
    }
}

/// A task that consumes the operations applied to the downlink, updates the state and
/// forwards events and commands to a pair of output sinks.
pub(in crate::downlink) struct DownlinkTask<Commands, Events> {
    cmd_sink: Commands,
    ev_sink: Events,
    completed: Arc<AtomicBool>,
    stop_event: watch::Sender<Option<Result<(), DownlinkError>>>,
    on_invalid: OnInvalidMessage,
}

enum TaskInput<M, A> {
    Op(Operation<M, A>),
    ActTerminated,
    Terminated,
}

impl<M, A> TaskInput<M, A> {
    fn from_action(maybe_action: Option<A>) -> TaskInput<M, A> {
        match maybe_action {
            Some(a) => TaskInput::Op(Operation::Action(a)),
            _ => TaskInput::ActTerminated,
        }
    }

    fn from_operation(maybe_action: Option<Operation<M, A>>) -> TaskInput<M, A> {
        match maybe_action {
            Some(op) => TaskInput::Op(op),
            _ => TaskInput::Terminated,
        }
    }
}

impl<Commands, Events> DownlinkTask<Commands, Events> {
    pub(in crate::downlink) fn new(
        cmd_sink: Commands,
        ev_sink: Events,
        completed: Arc<AtomicBool>,
        stop_event: watch::Sender<Option<Result<(), DownlinkError>>>,
        on_invalid: OnInvalidMessage,
    ) -> Self {
        DownlinkTask {
            cmd_sink,
            ev_sink,
            completed,
            stop_event,
            on_invalid,
        }
    }

    #[instrument(skip(self, ops, acts, state_machine))]
    pub(in crate::downlink) async fn run<M, A, Ops, Acts, State, Machine>(
        self,
        ops: Ops,
        acts: Acts,
        state_machine: Machine,
        yield_after: NonZeroUsize,
    ) -> Result<(), DownlinkError>
    where
        Machine: StateMachine<State, M, A>,
        Ops: FusedStream<Item = Operation<M, A>> + Send + 'static,
        Acts: FusedStream<Item = A> + Send + 'static,
        Commands: ItemSender<Command<Machine::Cmd>, RoutingError>,
        Events: ItemSender<Event<Machine::Ev>, DroppedError>,
    {
        let DownlinkTask {
            mut cmd_sink,
            mut ev_sink,
            completed,
            stop_event,
            on_invalid,
        } = self;

        let mut dl_state = DownlinkState::Unlinked;
        let mut model = state_machine.init_state();
        let yield_mod = yield_after.get();

        pin_mut!(ops);
        pin_mut!(acts);

        let mut ops_str: Pin<&mut Ops> = ops;
        let mut act_str: Pin<&mut Acts> = acts;

        let mut act_terminated = false;
        let mut events_terminated = false;
        let mut read_act = false;

        let mut iteration_count: usize = 0;

        trace!("Running downlink task");

        let result: Result<(), DownlinkError> = loop {
            let next_op: TaskInput<M, A> =
                if dl_state == state_machine.dl_start_state() && !act_terminated {
                    if read_act {
                        read_act = false;
                        let input = select_biased! {
                            act_op = act_str.next() => TaskInput::from_action(act_op),
                            upd_op = ops_str.next() => TaskInput::from_operation(upd_op),
                        };
                        input
                    } else {
                        read_act = true;
                        let input = select_biased! {
                            upd_op = ops_str.next() => TaskInput::from_operation(upd_op),
                            act_op = act_str.next() => TaskInput::from_action(act_op),
                        };
                        input
                    }
                } else {
                    TaskInput::from_operation(ops_str.next().await)
                };

            match next_op {
                TaskInput::Op(op) => {
                    let Response {
                        event,
                        command,
                        error,
                        terminate,
                    } = match state_machine.handle_operation(&mut dl_state, &mut model, op) {
                        Ok(r) => r,
                        Err(e) => match e {
                            e @ DownlinkError::TaskPanic(_) => {
                                break Err(e);
                            }
                            _ => match on_invalid {
                                OnInvalidMessage::Ignore => {
                                    continue;
                                }
                                OnInvalidMessage::Terminate => {
                                    break Err(e);
                                }
                            },
                        },
                    };
                    let result = match (event, command) {
                        (Some(event), Some(cmd)) => {
                            if !events_terminated && ev_sink.send_item(event).await.is_err() {
                                events_terminated = true;
                            }
                            cmd_sink.send_item(cmd).await
                        }
                        (Some(event), _) => {
                            if !events_terminated && ev_sink.send_item(event).await.is_err() {
                                events_terminated = true;
                            }
                            Ok(())
                        }
                        (_, Some(command)) => cmd_sink.send_item(command).await,
                        _ => Ok(()),
                    };

                    if error.map(|e| e.is_fatal()).unwrap_or(false) {
                        break Err(DownlinkError::TransitionError);
                    } else if terminate || result.is_err() {
                        break result.map_err(Into::into);
                    } else if act_terminated && events_terminated {
                        break Ok(());
                    }
                }
                TaskInput::ActTerminated => {
                    act_terminated = true;
                    if events_terminated {
                        break Ok(());
                    }
                }
                TaskInput::Terminated => {
                    break Ok(());
                }
            }

            iteration_count += 1;
            if iteration_count % yield_mod == 0 {
                tokio::task::yield_now().await;
            }
        };
        completed.store(true, Ordering::Release);
        let _ = stop_event.broadcast(Some(result.clone()));
        result
    }
}

/// Combines together updates received from the Warp connection  and the stop signal
/// into a single stream.
pub(in crate::downlink) fn make_operation_stream<M, A, Upd>(
    updates: Upd,
) -> impl FusedStream<Item = Operation<M, A>> + Send + 'static
where
    M: Send + 'static,
    A: Send + 'static,
    Upd: Stream<Item = Result<Message<M>, RoutingError>> + Send + 'static,
{
    let upd_operations = updates.map(|e| match e {
        Ok(l) => Operation::Message(l),
        Err(e) => Operation::Error(e),
    });

    let init = once(ready(Operation::Start));

    init.chain(upd_operations).fuse()
}
