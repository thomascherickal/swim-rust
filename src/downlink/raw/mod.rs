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

use super::*;

#[cfg(test)]
mod tests;

pub struct Sender<S> {
    /// A sink for local actions (sets, insertions, etc.)
    pub set_sink: S,
    /// The task running the downlink.
    task: Option<DownlinkTask>,
}

impl<T> Sender<mpsc::Sender<T>> {
    pub async fn send(&mut self, value: T) -> Result<(), mpsc::error::SendError<T>> {
        self.set_sink.send(value).await
    }
}

impl<T> Sender<watch::Sender<T>> {
    pub fn send(&mut self, value: T) -> Result<(), watch::error::SendError<T>> {
        self.set_sink.broadcast(value)
    }
}

impl<S> Sender<S> {
    /// Stop the downlink from running.
    pub async fn stop(mut self) -> Result<(), DownlinkError> {
        match self.task.take() {
            Some(t) => t.stop().await,
            _ => Ok(()),
        }
    }
}

pub struct Receiver<R> {
    /// A stream of events generated by the downlink.
    pub event_stream: R,
}

/// Type containing the components of a running downlink.
pub struct RawDownlink<S, R> {
    pub receiver: Receiver<R>,
    pub sender: Sender<S>,
}

impl<S, R> RawDownlink<S, R> {
    //Private as downlinks should only be created by methods in this module and its children.
    fn new(set_sink: S, event_stream: R, task: Option<DownlinkTask>) -> RawDownlink<S, R> {
        RawDownlink {
            receiver: Receiver { event_stream },
            sender: Sender { set_sink, task },
        }
    }

    /// Stop the downlink from running.
    pub async fn stop(self) -> Result<(), DownlinkError> {
        self.sender.stop().await
    }

    pub fn split(self) -> (Sender<S>, Receiver<R>) {
        let RawDownlink { sender, receiver } = self;
        (sender, receiver)
    }
}

/// Asynchronously create a new downlink from a stream of input events, writing to a sink of
/// commands.
pub(in crate::downlink) fn create_downlink<Err, M, A, State, Updates, Commands>(
    init: State,
    update_stream: Updates,
    cmd_sink: Commands,
    buffer_size: usize,
) -> RawDownlink<mpsc::Sender<A>, mpsc::Receiver<Event<State::Ev>>>
where
    M: Send + 'static,
    A: Send + 'static,
    State: StateMachine<M, A> + Send + 'static,
    State::Ev: Send + 'static,
    State::Cmd: Send + 'static,
    Err: Into<DownlinkError> + Send + 'static,
    Updates: Stream<Item = Message<M>> + Send + 'static,
    Commands: for<'b> ItemSink<'b, Command<State::Cmd>, Error = Err> + Send + 'static,
{
    let model = Model::new(init);
    let (act_tx, act_rx) = mpsc::channel::<A>(buffer_size);
    let (event_tx, event_rx) = mpsc::channel::<Event<State::Ev>>(buffer_size);
    let (stop_tx, stop_rx) = oneshot::channel::<()>();

    let event_sink = item::for_mpsc_sender::<_, DownlinkError>(event_tx);

    // The task that maintains the internal state of the lane.
    let lane_task = make_downlink_task(
        model,
        combine_inputs(update_stream, stop_rx),
        act_rx.fuse(),
        cmd_sink,
        event_sink,
    );

    let join_handle = tokio::task::spawn(lane_task);

    let dl_task = DownlinkTask {
        join_handle,
        stop_trigger: stop_tx,
    };

    RawDownlink::new(act_tx, event_rx, Some(dl_task))
}

struct DownlinkTask {
    join_handle: JoinHandle<Result<(), DownlinkError>>,
    stop_trigger: oneshot::Sender<()>,
}

impl DownlinkTask {
    async fn stop(self) -> Result<(), DownlinkError> {
        match self.stop_trigger.send(()) {
            Ok(_) => match self.join_handle.await {
                Ok(r) => r,
                Err(_) => Err(DownlinkError::TaskPanic),
            },
            Err(_) => match self.join_handle.await {
                Ok(r) => r,
                Err(_) => Err(DownlinkError::TaskPanic),
            },
        }
    }
}

/// A task that consumes the operations applied to the downlink, updates the state and
/// forwards events and commands to a pair of output sinks.
async fn make_downlink_task<State, EC, EE, M, A, Ops, Acts, Commands, Events>(
    mut model: Model<State>,
    ops: Ops,
    acts: Acts,
    mut cmd_sink: Commands,
    mut ev_sink: Events,
) -> Result<(), DownlinkError>
where
    EC: Into<DownlinkError>,
    EE: Into<DownlinkError>,
    State: StateMachine<M, A>,
    Ops: FusedStream<Item = Operation<M, A>> + Send + 'static,
    Acts: FusedStream<Item = A> + Send + 'static,
    Commands: for<'b> ItemSink<'b, Command<State::Cmd>, Error = EC>,
    Events: for<'b> ItemSink<'b, Event<State::Ev>, Error = EE>,
{
    pin_mut!(ops);
    pin_mut!(acts);
    let mut ops_str: Pin<&mut Ops> = ops;
    let mut act_str: Pin<&mut Acts> = acts;

    let mut read_act = false;

    loop {
        let next_op = if model.state == DownlinkState::Synced {
            if read_act {
                read_act = false;
                select_biased! {
                    act_op = act_str.next() => act_op.map(Operation::Action),
                    upd_op = ops_str.next() => upd_op,
                }
            } else {
                read_act = true;
                select_biased! {
                    upd_op = ops_str.next() => upd_op,
                    act_op = act_str.next() => act_op.map(Operation::Action),
                }
            }
        } else {
            ops_str.next().await
        };

        if let Some(op) = next_op {
            let Response {
                event,
                command,
                error,
                terminate,
            } = StateMachine::handle_operation(&mut model, op);
            let result = match (event, command) {
                (Some(ev), Some(cmd)) => match ev_sink.send_item(ev).await {
                    Ok(()) => cmd_sink.send_item(cmd).await.map_err(|e| e.into()),
                    Err(e) => Err(e.into()),
                },
                (Some(event), _) => ev_sink.send_item(event).await.map_err(|e| e.into()),
                (_, Some(command)) => cmd_sink.send_item(command).await.map_err(|e| e.into()),
                _ => Ok(()),
            };

            if error.is_some() {
                break Err(DownlinkError::TransitionError); //TODO Handle this properly.
            } else if terminate || result.is_err() {
                break result;
            }
        } else {
            break Err(DownlinkError::OperationStreamEnded);
        }
    }
}

/// Combines together updates received from the Warp connection  and the stop signal
/// into a single stream.
fn combine_inputs<M, A, Upd>(
    updates: Upd,
    stop: oneshot::Receiver<()>,
) -> impl FusedStream<Item = Operation<M, A>> + Send + 'static
where
    M: Send + 'static,
    A: Send + 'static,
    Upd: Stream<Item = Message<M>> + Send + 'static,
{
    let upd_operations = updates.map(Operation::Message);
    let close_operations = stream::once(stop).map(|_| Operation::Close);

    let init = stream::once(future::ready(Operation::Start));

    init.chain(stream::select(close_operations, upd_operations))
}
