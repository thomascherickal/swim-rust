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
use crate::downlink::any::AnyDownlink;
use crate::downlink::raw::{DownlinkTask, DownlinkTaskHandle};
use crate::downlink::topic::{DownlinkReceiver, DownlinkTopic, MakeReceiver};
use crate::downlink::{
    raw, Command, Downlink, DownlinkError, DownlinkInternals, Event, Message, StateMachine,
    StoppedFuture,
};
use crate::router::RoutingError;
use common::sink::item::{ItemSender, ItemSink, MpscSend};
use common::topic::{BroadcastReceiver, BroadcastSender, BroadcastTopic, Topic, TopicError};
use futures::future::Ready;
use futures::Stream;
use futures_util::stream::StreamExt;
use std::fmt::{Debug, Formatter};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Weak};
use tokio::sync::{mpsc, watch};
use tracing::trace_span;
use tracing_futures::Instrument;
use utilities::future::{SwimFutureExt, TransformedFuture};
/// A downlink where subscribers consume via a shared queue that will start dropping (the oldest)
/// records if any fall behind.
#[derive(Debug)]
pub struct BufferedDownlink<Act, Upd> {
    input: mpsc::Sender<Act>,
    topic: BroadcastTopic<Event<Upd>>,
    internal: Arc<Internal<Act, Upd>>,
}

struct Internal<Act, Upd> {
    input: mpsc::Sender<Act>,
    topic: BroadcastTopic<Event<Upd>>,
    task: DownlinkTaskHandle,
}

/// A weak handle on a buffered downlink. Holding this will not keep the downlink running nor prevent
/// its sender and topic from being dropped.
#[derive(Debug)]
pub struct WeakBufferedDownlink<Act, Upd>(Weak<Internal<Act, Upd>>);

impl<Act, Upd> WeakBufferedDownlink<Act, Upd> {
    /// Attempt to upgrade this weak handle to a strong one.
    pub fn upgrade(&self) -> Option<BufferedDownlink<Act, Upd>> {
        self.0.upgrade().map(|internal| BufferedDownlink {
            input: internal.input.clone(),
            topic: internal.topic.clone(),
            internal,
        })
    }
}

impl<Act, Upd> DownlinkInternals for Internal<Act, Upd>
where
    Act: Send,
    Upd: Send + Sync,
{
    fn task_handle(&self) -> &DownlinkTaskHandle {
        &self.task
    }
}

impl<Act, Upd> Debug for Internal<Act, Upd> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Buffered Downlink Internals>")
    }
}

impl<Act, Upd> Clone for BufferedDownlink<Act, Upd> {
    fn clone(&self) -> Self {
        BufferedDownlink {
            input: self.input.clone(),
            topic: self.topic.clone(),
            internal: self.internal.clone(),
        }
    }
}

impl<Act, Upd> BufferedDownlink<Act, Upd> {
    pub fn any_dl(self) -> AnyDownlink<Act, Upd> {
        AnyDownlink::Buffered(self)
    }

    pub fn same_downlink(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.internal, &other.internal)
    }

    /// Downgrade this handle to a weak handle.
    pub fn downgrade(&self) -> WeakBufferedDownlink<Act, Upd> {
        WeakBufferedDownlink(Arc::downgrade(&self.internal))
    }

    /// Determine if the downlink is still running.
    pub fn is_running(&self) -> bool {
        !self.internal.task.is_complete()
    }

    /// Get a future that will complete when the downlink stops running.
    pub fn await_stopped(&self) -> StoppedFuture {
        self.internal.task.await_stopped()
    }
}

pub type BufferedTopicReceiver<T> = BroadcastReceiver<Event<T>>;
pub type BufferedReceiver<T> = DownlinkReceiver<BroadcastReceiver<Event<T>>>;

impl<Act, Upd> BufferedDownlink<Act, Upd>
where
    Act: Send + 'static,
    Upd: Clone + Send + Sync + 'static,
{
    pub(in crate::downlink) fn assemble<F>(
        raw_factory: F,
        buffer_size: usize,
    ) -> (BufferedDownlink<Act, Upd>, BufferedReceiver<Upd>)
    where
        F: FnOnce(BroadcastSender<Event<Upd>>) -> (mpsc::Sender<Act>, DownlinkTaskHandle),
    {
        let (topic, sender, first) = BroadcastTopic::new(buffer_size);
        let (input, task) = raw_factory(sender);
        let internal = Internal {
            input: input.clone(),
            topic: topic.clone(),
            task,
        };
        let internal_ptr = Arc::new(internal);
        let first_receiver = DownlinkReceiver::new(first, internal_ptr.clone());
        (
            BufferedDownlink {
                input,
                topic,
                internal: internal_ptr,
            },
            first_receiver,
        )
    }
}

impl<Act, Upd> Topic<Event<Upd>> for BufferedDownlink<Act, Upd>
where
    Act: Send + 'static,
    Upd: Clone + Send + Sync + 'static,
{
    type Receiver = BufferedReceiver<Upd>;
    type Fut =
        TransformedFuture<Ready<Result<BufferedTopicReceiver<Upd>, TopicError>>, MakeReceiver>;

    fn subscribe(&mut self) -> Self::Fut {
        self.topic
            .subscribe()
            .transform(MakeReceiver::new(self.internal.clone()))
    }
}

impl<'a, Act, Upd> ItemSink<'a, Act> for BufferedDownlink<Act, Upd>
where
    Act: Send + 'static,
{
    type Error = DownlinkError;
    type SendFuture = MpscSend<'a, Act, DownlinkError>;

    fn send_item(&'a mut self, value: Act) -> Self::SendFuture {
        MpscSend::new(&mut self.input, value)
    }
}

impl<Act, Upd> Downlink<Act, Event<Upd>> for BufferedDownlink<Act, Upd>
where
    Act: Send + 'static,
    Upd: Clone + Send + Sync + 'static,
{
    type DlTopic = DownlinkTopic<BroadcastTopic<Event<Upd>>>;
    type DlSink = raw::Sender<mpsc::Sender<Act>>;

    fn split(self) -> (Self::DlTopic, Self::DlSink) {
        let BufferedDownlink {
            input,
            topic,
            internal,
        } = self;
        let sender = raw::Sender::new(input, internal.clone());
        let dl_topic = DownlinkTopic::new(topic, internal);
        (dl_topic, sender)
    }
}

pub(in crate::downlink) fn make_downlink<M, A, State, Machine, Updates, Commands>(
    machine: Machine,
    update_stream: Updates,
    cmd_sink: Commands,
    buffer_size: usize,
    queue_size: usize,
    on_invalid: OnInvalidMessage,
) -> (
    BufferedDownlink<A, Machine::Ev>,
    BufferedReceiver<Machine::Ev>,
)
where
    M: Send + 'static,
    A: Send + 'static,
    State: Send + 'static,
    Machine: StateMachine<State, M, A> + Send + 'static,
    Machine::Ev: Clone + Send + Sync + 'static,
    Machine::Cmd: Send + 'static,
    Updates: Stream<Item = Message<M>> + Send + 'static,
    Commands: ItemSender<Command<Machine::Cmd>, RoutingError> + Send + 'static,
{
    let fac = move |event_tx: BroadcastSender<Event<Machine::Ev>>| {
        let (act_tx, act_rx) = mpsc::channel::<A>(buffer_size);

        let event_sink = event_tx.map_err_into();

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
            raw::make_operation_stream(update_stream),
            act_rx.fuse(),
            machine,
        );

        let join_handle = tokio::task::spawn(lane_task.instrument(trace_span!("downlink task")));

        let dl_task = raw::DownlinkTaskHandle::new(join_handle, stopped_rx, completed);

        (act_tx, dl_task)
    };

    BufferedDownlink::assemble(fac, queue_size)
}
