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

use crate::agent::context::AgentExecutionContext;
use crate::agent::lane::channels::task::{LaneUplinks, UplinkChannels};
use crate::agent::lane::channels::update::{LaneUpdate, UpdateError};
use crate::agent::lane::channels::uplink::spawn::{SpawnerUplinkFactory, UplinkErrorReport};
use crate::agent::lane::channels::uplink::{UplinkAction, UplinkError, UplinkStateMachine};
use crate::agent::lane::channels::{AgentExecutionConfig, LaneMessageHandler, TaggedAction};
use crate::agent::Eff;
use crate::plane::error::ResolutionError;
use crate::routing::{RoutingAddr, ServerRouter, TaggedEnvelope};
use futures::future::{join, join3, ready, BoxFuture};
use futures::stream::once;
use futures::stream::{BoxStream, FusedStream};
use futures::{FutureExt, Stream, StreamExt};
use pin_utils::pin_mut;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;
use swim_common::form::{Form, FormErr};
use swim_common::model::Value;
use swim_common::routing::RoutingError;
use swim_common::sink::item::ItemSink;
use swim_common::topic::MpscTopic;
use swim_common::warp::envelope::Envelope;
use swim_common::warp::path::RelativePath;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Sender;
use url::Url;

const INIT: i32 = 42;

#[derive(Debug)]
struct Message(i32);

impl Form for Message {
    fn as_value(&self) -> Value {
        Value::Int32Value(self.0)
    }

    fn try_from_value(value: &Value) -> Result<Self, FormErr> {
        i32::try_from_value(value).map(|n| Message(n))
    }
}

//A minimal suite of fake uplink and router implementations which which to test the spawner.

struct TestHandler(mpsc::Sender<i32>, i32);
struct TestStateMachine(i32);
struct TestUpdater(mpsc::Sender<i32>);
struct TestRouter(mpsc::Sender<TaggedEnvelope>);

struct TestSender {
    addr: RoutingAddr,
    inner: mpsc::Sender<TaggedEnvelope>,
}

impl<'a> ItemSink<'a, Envelope> for TestSender {
    type Error = RoutingError;
    type SendFuture = BoxFuture<'a, Result<(), Self::Error>>;

    fn send_item(&'a mut self, value: Envelope) -> Self::SendFuture {
        let tagged = TaggedEnvelope(self.addr, value);
        async move {
            self.inner
                .send(tagged)
                .await
                .map_err(|_| RoutingError::RouterDropped)
        }
        .boxed()
    }
}

impl ServerRouter for TestRouter {
    type Sender = TestSender;

    fn get_sender(&mut self, addr: RoutingAddr) -> BoxFuture<Result<Self::Sender, RoutingError>> {
        ready(Ok(TestSender {
            addr,
            inner: self.0.clone(),
        }))
        .boxed()
    }

    fn resolve(
        &mut self,
        _host: Option<Url>,
        _route: String,
    ) -> BoxFuture<'static, Result<RoutingAddr, ResolutionError>> {
        panic!("Unexpected resolution attempt.")
    }
}

impl LaneMessageHandler for TestHandler {
    type Event = i32;
    type Uplink = TestStateMachine;
    type Update = TestUpdater;

    fn make_uplink(&self, _addr: RoutingAddr) -> Self::Uplink {
        TestStateMachine(self.1)
    }

    fn make_update(&self) -> Self::Update {
        TestUpdater(self.0.clone())
    }
}

impl From<Message> for Value {
    fn from(msg: Message) -> Self {
        Value::Int32Value(msg.0)
    }
}

impl UplinkStateMachine<i32> for TestStateMachine {
    type Msg = Message;

    fn message_for(&self, event: i32) -> Result<Option<Self::Msg>, UplinkError> {
        if event >= 0 {
            Ok(Some(Message(event)))
        } else {
            Err(UplinkError::InconsistentForm(FormErr::Malformatted))
        }
    }

    fn sync_lane<'a, Updates>(
        &'a self,
        _updates: &'a mut Updates,
    ) -> BoxStream<'a, Result<Self::Msg, UplinkError>>
    where
        Updates: FusedStream<Item = i32> + Send + Unpin + 'a,
    {
        let TestStateMachine(n) = self;
        once(ready(Ok(Message(*n)))).boxed()
    }
}

impl LaneUpdate for TestUpdater {
    type Msg = Message;

    fn run_update<Messages, Err>(
        self,
        messages: Messages,
    ) -> BoxFuture<'static, Result<(), UpdateError>>
    where
        Messages: Stream<Item = Result<(RoutingAddr, Self::Msg), Err>> + Send + 'static,
        Err: Send,
        UpdateError: From<Err>,
    {
        let TestUpdater(mut tx) = self;

        async move {
            pin_mut!(messages);
            while let Some(Ok((_, Message(n)))) = messages.next().await {
                if tx.send(n).await.is_err() {
                    break;
                }
            }
            Ok(())
        }
        .boxed()
    }
}

fn default_buffer() -> NonZeroUsize {
    NonZeroUsize::new(5).unwrap()
}

fn yield_after() -> NonZeroUsize {
    NonZeroUsize::new(256).unwrap()
}

fn route() -> RelativePath {
    RelativePath::new("node", "lane")
}

#[derive(Clone)]
struct UplinkSpawnerInputs {
    action_tx: mpsc::Sender<TaggedAction>,
    event_tx: mpsc::Sender<i32>,
}

impl UplinkSpawnerInputs {
    async fn action(&mut self, addr: RoutingAddr, action: UplinkAction) {
        assert!(self
            .action_tx
            .send(TaggedAction(addr, action))
            .await
            .is_ok())
    }

    async fn generate_event(&mut self, event: i32) {
        assert!(self.event_tx.send(event).await.is_ok())
    }
}

impl UplinkSpawnerOutputs {
    async fn take_router_events(&mut self, n: usize) -> Vec<TaggedEnvelope> {
        tokio::time::timeout(
            Duration::from_secs(1),
            (&mut self.router_rx).take(n).collect::<Vec<_>>(),
        )
        .await
        .expect("Timeout awaiting outputs.")
    }

    fn split(self, expected: HashSet<RoutingAddr>) -> (UplinkSpawnerSplitOutputs, Eff) {
        let UplinkSpawnerOutputs {
            _update_rx,
            mut router_rx,
        } = self;
        let mut txs = HashMap::new();
        let mut rxs = HashMap::new();
        for addr in expected.iter() {
            let (tx, rx) = mpsc::channel(5);
            txs.insert(*addr, tx);
            rxs.insert(*addr, rx);
        }
        let task = async move {
            while let Some(TaggedEnvelope(addr, envelope)) = router_rx.next().await {
                if let Some(tx) = txs.get_mut(&addr) {
                    assert!(tx.send(envelope).await.is_ok());
                } else {
                    panic!("Unexpected address: {}", addr);
                }
            }
        };
        (
            UplinkSpawnerSplitOutputs {
                _update_rx,
                router_rxs: rxs,
            },
            task.boxed(),
        )
    }
}

struct UplinkSpawnerOutputs {
    _update_rx: mpsc::Receiver<i32>,
    router_rx: mpsc::Receiver<TaggedEnvelope>,
}

struct UplinkSpawnerSplitOutputs {
    _update_rx: mpsc::Receiver<i32>,
    router_rxs: HashMap<RoutingAddr, mpsc::Receiver<Envelope>>,
}

struct RouterChannel(mpsc::Receiver<Envelope>);

impl RouterChannel {
    async fn take_router_events(&mut self, n: usize) -> Vec<Envelope> {
        tokio::time::timeout(
            Duration::from_secs(1),
            (&mut self.0).take(n).collect::<Vec<_>>(),
        )
        .await
        .expect("Timeout awaiting outputs.")
    }
}

impl UplinkSpawnerSplitOutputs {
    pub fn take_addr(&mut self, addr: RoutingAddr) -> RouterChannel {
        RouterChannel(self.router_rxs.remove(&addr).unwrap())
    }
}

fn make_config() -> AgentExecutionConfig {
    AgentExecutionConfig::with(default_buffer(), 1, 1, Duration::from_secs(5))
}

struct TestContext(mpsc::Sender<TaggedEnvelope>, Sender<Eff>);

impl AgentExecutionContext for TestContext {
    type Router = TestRouter;

    fn router_handle(&self) -> Self::Router {
        TestRouter(self.0.clone())
    }

    fn spawner(&self) -> Sender<Eff> {
        self.1.clone()
    }
}

/// Create a spawner connected to a complete test harness.
fn make_test_harness() -> (
    UplinkSpawnerInputs,
    UplinkSpawnerOutputs,
    BoxFuture<'static, Vec<UplinkErrorReport>>,
) {
    let (tx_up, rx_up) = mpsc::channel(5);
    let (tx_event, rx_event) = mpsc::channel(5);
    let (tx_act, rx_act) = mpsc::channel(5);
    let (tx_router, rx_router) = mpsc::channel(5);

    let (spawn_tx, spawn_rx) = mpsc::channel(5);
    let spawn_task = spawn_rx.for_each_concurrent(None, |task| task);

    let (error_tx, error_rx) = mpsc::channel(5);
    let error_task = error_rx.collect::<Vec<_>>();

    let handler = Arc::new(TestHandler(tx_up, INIT));
    let (topic, _rec) = MpscTopic::new(rx_event, default_buffer(), yield_after());
    let factory = SpawnerUplinkFactory(make_config());

    let channels = UplinkChannels::new(topic, rx_act, error_tx);

    let context = TestContext(tx_router, spawn_tx);

    let spawner_task = factory.make_task(handler, channels, route(), &context);

    let errs = join3(spawn_task, spawner_task, error_task)
        .map(|(_, _, errs)| errs)
        .boxed();

    (
        UplinkSpawnerInputs {
            event_tx: tx_event,
            action_tx: tx_act,
        },
        UplinkSpawnerOutputs {
            _update_rx: rx_up,
            router_rx: rx_router,
        },
        errs,
    )
}

#[tokio::test]
async fn link_to_lane() {
    let (mut inputs, mut outputs, spawn_task) = make_test_harness();

    let addr = RoutingAddr::remote(1);

    let io_task = async move {
        inputs.action(addr, UplinkAction::Link).await;

        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, Envelope::linked("node", "lane"))]
        );

        drop(inputs);

        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, Envelope::unlinked("node", "lane"))]
        );
    };

    let (_, errs) = join(io_task, spawn_task).await;
    assert!(errs.is_empty());
}

#[tokio::test]
async fn immediate_unlink() {
    let (mut inputs, mut outputs, spawn_task) = make_test_harness();

    let addr = RoutingAddr::remote(1);

    let io_task = async move {
        inputs.action(addr, UplinkAction::Unlink).await;

        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, Envelope::unlinked("node", "lane"))]
        );
    };

    let (_, errs) = join(io_task, spawn_task).await;
    assert!(errs.is_empty());
}

fn event_envelope(n: i32) -> Envelope {
    Envelope::make_event("node", "lane", Some(Value::Int32Value(n)))
}

#[tokio::test]
async fn receive_event() {
    let (mut inputs, mut outputs, spawn_task) = make_test_harness();

    let addr = RoutingAddr::remote(1);

    let io_task = async move {
        inputs.action(addr, UplinkAction::Link).await;
        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, Envelope::linked("node", "lane"))]
        );
        inputs.generate_event(13).await;
        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, event_envelope(13))]
        );

        drop(inputs);

        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, Envelope::unlinked("node", "lane"))]
        );
    };

    let (_, errs) = join(io_task, spawn_task).await;
    assert!(errs.is_empty());
}

#[tokio::test]
async fn sync_with_lane() {
    let (mut inputs, mut outputs, spawn_task) = make_test_harness();

    let addr = RoutingAddr::remote(1);

    let io_task = async move {
        inputs.action(addr, UplinkAction::Sync).await;

        assert_eq!(
            outputs.take_router_events(3).await,
            vec![
                TaggedEnvelope(addr, Envelope::linked("node", "lane")),
                TaggedEnvelope(addr, event_envelope(INIT)),
                TaggedEnvelope(addr, Envelope::synced("node", "lane"))
            ]
        );

        drop(inputs);

        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, Envelope::unlinked("node", "lane"))]
        );
    };

    let (_, errs) = join(io_task, spawn_task).await;
    assert!(errs.is_empty());
}

#[tokio::test]
async fn receive_event_after_sync() {
    let (mut inputs, mut outputs, spawn_task) = make_test_harness();

    let addr = RoutingAddr::remote(1);

    let io_task = async move {
        inputs.action(addr, UplinkAction::Sync).await;

        assert_eq!(
            outputs.take_router_events(3).await,
            vec![
                TaggedEnvelope(addr, Envelope::linked("node", "lane")),
                TaggedEnvelope(addr, event_envelope(INIT)),
                TaggedEnvelope(addr, Envelope::synced("node", "lane"))
            ]
        );

        inputs.generate_event(13).await;
        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, event_envelope(13))]
        );

        drop(inputs);

        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, Envelope::unlinked("node", "lane"))]
        );
    };

    let (_, errs) = join(io_task, spawn_task).await;
    assert!(errs.is_empty());
}

#[tokio::test]
async fn relink_for_same_addr() {
    let (mut inputs, mut outputs, spawn_task) = make_test_harness();

    let addr = RoutingAddr::remote(1);

    let io_task = async move {
        inputs.action(addr, UplinkAction::Link).await;
        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, Envelope::linked("node", "lane"))]
        );

        inputs.action(addr, UplinkAction::Unlink).await;
        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, Envelope::unlinked("node", "lane"))]
        );

        inputs.action(addr, UplinkAction::Link).await;
        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, Envelope::linked("node", "lane"))]
        );

        drop(inputs);

        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, Envelope::unlinked("node", "lane"))]
        );
    };

    let (_, errs) = join(io_task, spawn_task).await;
    assert!(errs.is_empty());
}

#[tokio::test]
async fn sync_lane_twice() {
    let (inputs, outputs, spawn_task) = make_test_harness();

    let addr1 = RoutingAddr::remote(1);
    let addr2 = RoutingAddr::remote(2);

    let mut addrs = HashSet::new();
    addrs.insert(addr1);
    addrs.insert(addr2);

    let (mut split_outputs, split_task) = outputs.split(addrs);

    let mut inputs1 = inputs.clone();
    let mut outputs1 = split_outputs.take_addr(addr1);
    let mut inputs2 = inputs;
    let mut outputs2 = split_outputs.take_addr(addr2);

    let io_task1 = async move {
        inputs1.action(addr1, UplinkAction::Sync).await;

        assert_eq!(
            outputs1.take_router_events(3).await,
            vec![
                Envelope::linked("node", "lane"),
                event_envelope(INIT),
                Envelope::synced("node", "lane")
            ]
        );

        drop(inputs1);

        assert_eq!(
            outputs1.take_router_events(1).await,
            vec![Envelope::unlinked("node", "lane")]
        );
    };

    let io_task2 = async move {
        inputs2.action(addr2, UplinkAction::Sync).await;

        assert_eq!(
            outputs2.take_router_events(3).await,
            vec![
                Envelope::linked("node", "lane"),
                event_envelope(INIT),
                Envelope::synced("node", "lane")
            ]
        );

        drop(inputs2);

        assert_eq!(
            outputs2.take_router_events(1).await,
            vec![Envelope::unlinked("node", "lane")]
        );
    };

    let (_, _, errs) = join3(join(io_task1, io_task2), split_task, spawn_task).await;
    assert!(errs.is_empty());
}

#[tokio::test]
async fn uplink_failure() {
    let (mut inputs, mut outputs, spawn_task) = make_test_harness();

    let addr = RoutingAddr::remote(1);

    let io_task = async move {
        inputs.action(addr, UplinkAction::Link).await;
        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, Envelope::linked("node", "lane"))]
        );
        inputs.generate_event(-1).await;
        assert_eq!(
            outputs.take_router_events(1).await,
            vec![TaggedEnvelope(addr, Envelope::unlinked("node", "lane"))]
        );

        drop(inputs);
    };

    let (_, errs) = join(io_task, spawn_task).await;
    assert!(
        matches!(errs.as_slice(), [UplinkErrorReport { error: UplinkError::InconsistentForm(FormErr::Malformatted), addr: a }] if *a == addr)
    );
}
