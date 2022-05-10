// Copyright 2015-2021 Swim Inc.
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

use std::{
    num::NonZeroUsize,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::{
    future::{join3, select, Either},
    ready,
    stream::SelectAll,
    Future, SinkExt, Stream, StreamExt,
};
use swim_api::{
    agent::{LaneConfig, UplinkKind},
    error::FrameIoError,
    protocol::{
        agent::{LaneRequest, LaneRequestDecoder},
        map::{MapMessage, MapMessageDecoder, MapOperationDecoder},
        WithLenRecognizerDecoder,
    },
};
use swim_form::structural::read::recognizer::primitive::I32Recognizer;
use swim_model::{path::RelativePath, Text};
use swim_utilities::{
    algebra::non_zero_usize,
    io::byte_channel::{byte_channel, ByteReader, ByteWriter},
    trigger,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::codec::{FramedRead, FramedWrite};
use uuid::Uuid;

use crate::{
    agent::{
        task::{
            read_task, timeout_coord, LaneEndpoint, ReadTaskRegistration, RwCoorindationMessage,
            WriteTaskMessage,
        },
        AgentRuntimeConfig,
    },
    compat::{RawRequestMessageEncoder, RequestMessage},
    routing::RoutingAddr,
};

struct FakeAgent {
    initial: Vec<LaneEndpoint<ByteReader>>,
    coord: mpsc::Receiver<WriteTaskMessage>,
    stopping: trigger::Receiver,
    event_tx: mpsc::UnboundedSender<Event>,
}

type ValueDecoder = LaneRequestDecoder<WithLenRecognizerDecoder<I32Recognizer>>;
type MapDecoder = LaneRequestDecoder<MapMessageDecoder<MapOperationDecoder<Text, i32>>>;

enum LaneReader {
    Value {
        name: Text,
        read: FramedRead<ByteReader, ValueDecoder>,
    },
    Map {
        name: Text,
        read: FramedRead<ByteReader, MapDecoder>,
    },
}

impl LaneReader {
    fn new(endpoint: LaneEndpoint<ByteReader>) -> Self {
        let LaneEndpoint { name, kind, io } = endpoint;
        match kind {
            UplinkKind::Value => LaneReader::Value {
                name,
                read: FramedRead::new(
                    io,
                    LaneRequestDecoder::new(WithLenRecognizerDecoder::new(I32Recognizer)),
                ),
            },
            UplinkKind::Map => LaneReader::Map {
                name,
                read: FramedRead::new(io, LaneRequestDecoder::new(Default::default())),
            },
        }
    }
}

impl Stream for LaneReader {
    type Item = (
        Text,
        Result<Either<LaneRequest<i32>, LaneRequest<MapMessage<Text, i32>>>, FrameIoError>,
    );

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(match self.get_mut() {
            LaneReader::Value { name, read } => {
                let maybe_result = ready!(read.poll_next_unpin(cx));
                maybe_result.map(|r| (name.clone(), r.map(Either::Left).map_err(Into::into)))
            }
            LaneReader::Map { name, read } => {
                let maybe_result = ready!(read.poll_next_unpin(cx));
                maybe_result.map(|r| (name.clone(), r.map(Either::Right)))
            }
        })
    }
}

#[derive(Debug, Clone)]
enum Event {
    Sync {
        name: Text,
        id: Uuid,
    },
    ValueCommand {
        name: Text,
        n: i32,
    },
    MapCommand {
        name: Text,
        cmd: MapMessage<Text, i32>,
    },
    FrameError {
        name: Text,
    },
    Coord(RwCoorindationMessage),
}

impl FakeAgent {
    fn new(
        initial: Vec<LaneEndpoint<ByteReader>>,
        coord: mpsc::Receiver<WriteTaskMessage>,
        stopping: trigger::Receiver,
        event_tx: mpsc::UnboundedSender<Event>,
    ) -> Self {
        FakeAgent {
            initial,
            coord,
            stopping,
            event_tx,
        }
    }

    async fn run(self) -> Vec<Event> {
        let FakeAgent {
            initial,
            coord,
            stopping,
            event_tx,
        } = self;

        let mut lanes = SelectAll::new();
        for endpoint in initial {
            lanes.push(LaneReader::new(endpoint));
        }

        let mut coord_stream = ReceiverStream::new(coord).take_until(stopping);

        let mut events = vec![];

        loop {
            let event = match select(lanes.next(), coord_stream.next()).await {
                Either::Left((Some((name, Ok(Either::Left(LaneRequest::Sync(id))))), _))
                | Either::Left((Some((name, Ok(Either::Right(LaneRequest::Sync(id))))), _)) => {
                    Event::Sync { name, id }
                }
                Either::Left((Some((name, Ok(Either::Left(LaneRequest::Command(n))))), _)) => {
                    Event::ValueCommand { name, n }
                }
                Either::Left((Some((name, Ok(Either::Right(LaneRequest::Command(msg))))), _)) => {
                    Event::MapCommand { name, cmd: msg }
                }
                Either::Left((Some((name, Err(_))), _)) => Event::FrameError { name },
                Either::Right((Some(WriteTaskMessage::Coord(coord)), _)) => Event::Coord(coord),
                _ => {
                    break;
                }
            };
            events.push(event.clone());
            let _ = event_tx.send(event);
        }
        events
    }
}

struct TestContext {
    stop_sender: trigger::Sender,
    reg_tx: mpsc::Sender<ReadTaskRegistration>,
    vote2: timeout_coord::Voter,
    vote_rx: timeout_coord::Receiver,
    event_rx: mpsc::UnboundedReceiver<Event>,
}

const QUEUE_SIZE: NonZeroUsize = non_zero_usize!(8);
const BUFFER_SIZE: NonZeroUsize = non_zero_usize!(4096);

fn make_config(inactive_timeout: Duration) -> AgentRuntimeConfig {
    AgentRuntimeConfig {
        default_lane_config: LaneConfig {
            input_buffer_size: BUFFER_SIZE,
            output_buffer_size: BUFFER_SIZE,
        },
        attachment_queue_size: non_zero_usize!(8),
        inactive_timeout,
    }
}

const VAL_LANE: &str = "value_lane";
const MAP_LANE: &str = "map_lane";

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

async fn run_test_case<F, Fut>(inactive_timeout: Duration, test_case: F) -> Vec<Event>
where
    F: FnOnce(TestContext) -> Fut,
    Fut: Future<Output = ()> + Send,
{
    let (stop_tx, stop_rx) = trigger::trigger();
    let config = make_config(inactive_timeout);

    let endpoints = vec![
        LaneEndpoint {
            name: Text::new(VAL_LANE),
            kind: UplinkKind::Value,
            io: byte_channel(config.default_lane_config.input_buffer_size),
        },
        LaneEndpoint {
            name: Text::new(MAP_LANE),
            kind: UplinkKind::Map,
            io: byte_channel(config.default_lane_config.input_buffer_size),
        },
    ];

    let (event_tx, event_rx) = mpsc::unbounded_channel();

    let (endpoints_tx, endpoints_rx) = endpoints.into_iter().map(LaneEndpoint::split).unzip();
    let (coord_tx, coord_rx) = mpsc::channel(QUEUE_SIZE.get());
    let (reg_tx, reg_rx) = mpsc::channel(QUEUE_SIZE.get());

    let agent = FakeAgent::new(endpoints_rx, coord_rx, stop_rx.clone(), event_tx);

    let (vote1, vote2, vote_rx) = timeout_coord::timeout_coordinator();

    let read = read_task(config, endpoints_tx, reg_rx, coord_tx, vote1, stop_rx);

    let context = TestContext {
        stop_sender: stop_tx,
        reg_tx,
        vote2,
        vote_rx,
        event_rx,
    };

    let test_task = test_case(context);

    let (events, _, _) = tokio::time::timeout(TEST_TIMEOUT, join3(agent.run(), read, test_task))
        .await
        .expect("Test timeout out");
    events
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn shutdown_no_remotes() {
    let events = run_test_case(DEFAULT_TIMEOUT, |context| async move {
        let TestContext {
            stop_sender,
            reg_tx: _reg_tx,
            vote2: _vote2,
            vote_rx: _vote_rx,
            event_rx: _event_rx,
        } = context;
        stop_sender.trigger();
    })
    .await;
    assert!(events.is_empty());
}

const RID: RoutingAddr = RoutingAddr::remote(0);
const NODE: &str = "node";

struct RemoteSender(FramedWrite<ByteWriter, RawRequestMessageEncoder>);

impl RemoteSender {
    async fn link(&mut self, lane: &str) {
        let RemoteSender(inner) = self;
        let path = RelativePath::new(NODE, lane);
        assert!(inner.send(RequestMessage::link(RID, path)).await.is_ok());
    }

    async fn sync(&mut self, lane: &str) {
        let RemoteSender(inner) = self;
        let path = RelativePath::new(NODE, lane);
        assert!(inner.send(RequestMessage::sync(RID, path)).await.is_ok());
    }

    async fn value_command(&mut self, lane: &str, n: i32) {
        let RemoteSender(inner) = self;
        let path = RelativePath::new(NODE, lane);
        let body = format!("{}", n);
        assert!(inner
            .send(RequestMessage::command(RID, path, body.as_bytes()))
            .await
            .is_ok());
    }

    async fn map_command(&mut self, lane: &str, key: &str, value: i32) {
        let RemoteSender(inner) = self;
        let path = RelativePath::new(NODE, lane);
        let body = format!("@update(key:\"{}\") {}", key, value);
        assert!(inner
            .send(RequestMessage::command(RID, path, body.as_bytes()))
            .await
            .is_ok());
    }
}

async fn attach_remote(reg_tx: &mpsc::Sender<ReadTaskRegistration>) -> RemoteSender {
    let (tx, rx) = byte_channel(BUFFER_SIZE);
    assert!(reg_tx
        .send(ReadTaskRegistration::Remote { reader: rx })
        .await
        .is_ok());
    RemoteSender(FramedWrite::new(tx, Default::default()))
}

#[tokio::test]
async fn attach_remote_and_link() {
    let events = run_test_case(DEFAULT_TIMEOUT, |context| async move {
        let TestContext {
            stop_sender,
            reg_tx,
            vote2: _vote2,
            vote_rx: _vote_rx,
            mut event_rx,
        } = context;
        let mut sender = attach_remote(&reg_tx).await;
        sender.link(VAL_LANE).await;
        let event = event_rx.recv().await;
        match event {
            Some(Event::Coord(RwCoorindationMessage::Link { origin, lane })) => {
                assert_eq!(origin, *RID.uuid());
                assert_eq!(lane, VAL_LANE);
            }
            ow => panic!("Unexpected event: {:?}", ow),
        }
        stop_sender.trigger();
    })
    .await;
    assert_eq!(events.len(), 1);
}

#[tokio::test]
async fn attach_remote_and_sync() {
    let events = run_test_case(DEFAULT_TIMEOUT, |context| async move {
        let TestContext {
            stop_sender,
            reg_tx,
            vote2: _vote2,
            vote_rx: _vote_rx,
            mut event_rx,
        } = context;
        let mut sender = attach_remote(&reg_tx).await;
        sender.sync(VAL_LANE).await;
        let event = event_rx.recv().await;
        match event {
            Some(Event::Sync { name, id }) => {
                assert_eq!(id, *RID.uuid());
                assert_eq!(name, VAL_LANE);
            }
            ow => panic!("Unexpected event: {:?}", ow),
        }
        stop_sender.trigger();
    })
    .await;
    assert_eq!(events.len(), 1);
}

#[tokio::test]
async fn attach_remote_and_value_command() {
    let events = run_test_case(DEFAULT_TIMEOUT, |context| async move {
        let TestContext {
            stop_sender,
            reg_tx,
            vote2: _vote2,
            vote_rx: _vote_rx,
            mut event_rx,
        } = context;
        let mut sender = attach_remote(&reg_tx).await;
        sender.value_command(VAL_LANE, 77).await;
        let event = event_rx.recv().await;
        match event {
            Some(Event::ValueCommand { name, n }) => {
                assert_eq!(name, VAL_LANE);
                assert_eq!(n, 77);
            }
            ow => panic!("Unexpected event: {:?}", ow),
        }
        stop_sender.trigger();
    })
    .await;
    assert_eq!(events.len(), 1);
}

#[tokio::test]
async fn attach_remote_and_map_command() {
    let events = run_test_case(DEFAULT_TIMEOUT, |context| async move {
        let TestContext {
            stop_sender,
            reg_tx,
            vote2: _vote2,
            vote_rx: _vote_rx,
            mut event_rx,
        } = context;
        let mut sender = attach_remote(&reg_tx).await;
        sender.map_command(MAP_LANE, "key", 647).await;
        let event = event_rx.recv().await;
        match event {
            Some(Event::MapCommand {
                name,
                cmd: MapMessage::Update { key, value },
            }) => {
                assert_eq!(name, MAP_LANE);
                assert_eq!(key, "key");
                assert_eq!(value, 647);
            }
            ow => panic!("Unexpected event: {:?}", ow),
        }
        stop_sender.trigger();
    })
    .await;
    assert_eq!(events.len(), 1);
}
