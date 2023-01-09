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

use std::borrow::BorrowMut;
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::{ready, Future};
use std::io;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::BytesMut;
use futures_util::future::{BoxFuture, Either};
use futures_util::stream::Empty;
use futures_util::FutureExt;
use ratchet::{Message, NegotiatedExtension, NoExt, PayloadType, Role, WebSocket, WebSocketConfig};
use tokio::io::{duplex, AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio::time::timeout;
use tokio_util::codec::Encoder;
use uuid::Uuid;

use swim_api::downlink::{Downlink, DownlinkConfig, DownlinkKind};
use swim_api::error::DownlinkTaskError;
use swim_downlink::lifecycle::{BasicValueDownlinkLifecycle, ValueDownlinkLifecycle};
use swim_downlink::{DownlinkTask, ValueDownlinkModel};
use swim_model::address::{Address, RelativeAddress};
use swim_model::path::{AbsolutePath, RelativePath};
use swim_model::{Text, Value};
use swim_recon::parser::{parse_recognize, Span};
use swim_recon::printer::print_recon;
use swim_remote::AttachClient;
use swim_runtime::compat::{RawRequestMessageEncoder, RequestMessage};
use swim_runtime::downlink::{DownlinkOptions, DownlinkRuntimeConfig};
use swim_runtime::error::{ConnectionError, IoError};
use swim_runtime::remote::net::dns::{BoxDnsResolver, DnsResolver};
use swim_runtime::remote::table::SchemeHostPort;
use swim_runtime::remote::{ExternalConnections, Listener, Scheme, SchemeSocketAddr};
use swim_runtime::routing::RoutingAddr;
use swim_runtime::ws::{WsConnections, WsOpenFuture};
use swim_utilities::algebra::non_zero_usize;
use swim_utilities::io::byte_channel::{byte_channel, ByteReader, ByteWriter};
use swim_utilities::trigger;
use swim_utilities::trigger::{promise, Sender};
use swim_warp::envelope::Envelope;

use crate::runtime::error::DownlinkErrorKind;
use crate::runtime::transport::TransportHandle;
use crate::{start_runtime, DownlinkRuntimeError, RawHandle, Transport};

#[derive(Debug)]
struct Inner {
    addrs: HashMap<SchemeHostPort, SchemeSocketAddr>,
    sockets: HashMap<SocketAddr, DuplexStream>,
}

impl Inner {
    fn new<R, S>(resolver: R, sockets: S) -> Inner
    where
        R: IntoIterator<Item = (SchemeHostPort, SchemeSocketAddr)>,
        S: IntoIterator<Item = (SocketAddr, DuplexStream)>,
    {
        Inner {
            addrs: HashMap::from_iter(resolver),
            sockets: HashMap::from_iter(sockets),
        }
    }
}

#[derive(Debug, Clone)]
struct MockExternalConnections {
    inner: Arc<Mutex<Inner>>,
}

impl MockExternalConnections {
    fn new<R, S>(resolver: R, sockets: S) -> MockExternalConnections
    where
        R: IntoIterator<Item = (SchemeHostPort, SchemeSocketAddr)>,
        S: IntoIterator<Item = (SocketAddr, DuplexStream)>,
    {
        MockExternalConnections {
            inner: Arc::new(Mutex::new(Inner::new(resolver, sockets))),
        }
    }
}

struct MockListener;
impl Listener for MockListener {
    type Socket = DuplexStream;
    type AcceptStream = Empty<io::Result<(DuplexStream, SchemeSocketAddr)>>;

    fn into_stream(self) -> Self::AcceptStream {
        panic!("Unexpected listener invocation")
    }
}

impl ExternalConnections for MockExternalConnections {
    type Socket = DuplexStream;
    type ListenerType = MockListener;

    fn bind(
        &self,
        _addr: SocketAddr,
    ) -> BoxFuture<'static, io::Result<(SocketAddr, Self::ListenerType)>> {
        panic!("Unexpected bind invocation")
    }

    fn try_open(&self, addr: SocketAddr) -> BoxFuture<'static, io::Result<Self::Socket>> {
        let result = self
            .inner
            .lock()
            .unwrap()
            .sockets
            .remove(&addr)
            .ok_or(ErrorKind::NotFound.into());
        ready(result).boxed()
    }

    fn dns_resolver(&self) -> BoxDnsResolver {
        Box::new(self.clone())
    }

    fn lookup(
        &self,
        host: SchemeHostPort,
    ) -> BoxFuture<'static, io::Result<Vec<SchemeSocketAddr>>> {
        self.resolve(host).boxed()
    }
}

impl DnsResolver for MockExternalConnections {
    type ResolveFuture = BoxFuture<'static, io::Result<Vec<SchemeSocketAddr>>>;

    fn resolve(&self, host: SchemeHostPort) -> Self::ResolveFuture {
        let result = match self.inner.lock().unwrap().addrs.get(&host) {
            Some(sock) => Ok(vec![sock.clone()]),
            None => Err(io::ErrorKind::NotFound.into()),
        };
        ready(result).boxed()
    }
}

enum WsAction {
    Open,
    Fail(ConnectionError),
}

struct MockWs {
    states: HashMap<String, WsAction>,
}

impl MockWs {
    fn new<S>(states: S) -> MockWs
    where
        S: IntoIterator<Item = (String, WsAction)>,
    {
        MockWs {
            states: HashMap::from_iter(states),
        }
    }
}

impl WsConnections<DuplexStream> for MockWs {
    type Ext = NoExt;
    type Error = ConnectionError;

    fn open_connection(
        &self,
        socket: DuplexStream,
        addr: String,
    ) -> WsOpenFuture<DuplexStream, Self::Ext, Self::Error> {
        let result = match self.states.get(&addr) {
            Some(WsAction::Open) => Ok(WebSocket::from_upgraded(
                WebSocketConfig::default(),
                socket,
                NegotiatedExtension::from(NoExt),
                BytesMut::default(),
                Role::Client,
            )),
            Some(WsAction::Fail(e)) => Err(e.clone()),
            None => Err(ConnectionError::Io(IoError::new(ErrorKind::NotFound, None))),
        };
        ready(result).boxed()
    }

    fn accept_connection(
        &self,
        _socket: DuplexStream,
    ) -> WsOpenFuture<DuplexStream, Self::Ext, Self::Error> {
        panic!("Unexpected accept connection invocation")
    }
}

#[tokio::test]
async fn transport_opens_connection_ok() {
    let peer = SchemeHostPort::new(Scheme::Ws, "127.0.0.1".to_string(), 9001);
    let sock: SocketAddr = "127.0.0.1:9001".parse().unwrap();
    let (client, server) = duplex(128);
    let ext = MockExternalConnections::new(
        [(
            peer.clone(),
            SchemeSocketAddr::new(Scheme::Ws, sock.clone()),
        )],
        [("127.0.0.1:9001".parse().unwrap(), client)],
    );
    let ws = MockWs::new([("127.0.0.1".to_string(), WsAction::Open)]);
    let transport = Transport::new(ext, ws, non_zero_usize!(128));

    let (transport_tx, transport_rx) = mpsc::channel(128);
    let _transport_task = tokio::spawn(transport.run(transport_rx));

    let handle = TransportHandle::new(transport_tx);

    let addrs = handle.resolve(peer).await.expect("Failed to resolve peer");
    assert_eq!(addrs, vec![sock]);

    let (opened_sock, attach) = handle
        .connection_for("127.0.0.1".to_string(), vec![sock])
        .await
        .expect("Failed to open connection");
    assert_eq!(opened_sock, sock);

    let (byte_tx1, _byte_rx1) = byte_channel(non_zero_usize!(128));
    let (mut byte_tx2, byte_rx2) = byte_channel(non_zero_usize!(128));
    let (open_tx, open_rx) = oneshot::channel();

    attach
        .send(AttachClient::AttachDownlink {
            downlink_id: Uuid::nil(),
            path: RelativeAddress::new(Text::new("node"), Text::new("lane")),
            sender: byte_tx1,
            receiver: byte_rx2,
            done: open_tx,
        })
        .await
        .expect("Failed to attach downlink");
    open_rx.await.unwrap().expect("Failed to open downlink");

    let mut buf = BytesMut::new();

    RawRequestMessageEncoder
        .encode(
            RequestMessage::link(RoutingAddr::client(0), RelativePath::new("node", "lane")),
            &mut buf,
        )
        .unwrap();

    byte_tx2.write_all(&mut buf).await.unwrap();

    buf.clear();

    let mut ws_server = WebSocket::from_upgraded(
        WebSocketConfig::default(),
        server,
        NegotiatedExtension::from(NoExt),
        buf,
        Role::Server,
    );

    let mut buf = BytesMut::new();
    let message = ws_server.read(&mut buf).await.unwrap();
    assert_eq!(message, Message::Text);
    let link_message = std::str::from_utf8(buf.as_ref()).unwrap();
    assert_eq!(link_message, "@link(node:node,lane:lane)");

    let (opened_sock, attach_2) = handle
        .connection_for("127.0.0.1".to_string(), vec![sock])
        .await
        .expect("Failed to open connection");
    assert_eq!(opened_sock, sock);
    assert!(attach.same_channel(&attach_2));
}

#[tokio::test]
async fn transport_opens_connection_err() {
    let peer = SchemeHostPort::new(Scheme::Ws, "127.0.0.1".to_string(), 9001);
    let sock: SocketAddr = "127.0.0.1:9001".parse().unwrap();
    let (client, _server) = duplex(128);
    let ext = MockExternalConnections::new(
        [(
            peer.clone(),
            SchemeSocketAddr::new(Scheme::Ws, sock.clone()),
        )],
        [("127.0.0.1:9001".parse().unwrap(), client)],
    );
    let err = ConnectionError::Io(IoError::new(ErrorKind::NotFound, None));
    let ws = MockWs::new([("127.0.0.1".to_string(), WsAction::Fail(err.clone()))]);
    let transport = Transport::new(ext, ws, non_zero_usize!(128));

    let (transport_tx, transport_rx) = mpsc::channel(128);
    let _transport_task = tokio::spawn(transport.run(transport_rx));

    let handle = TransportHandle::new(transport_tx);

    let addrs = handle.resolve(peer).await.expect("Failed to resolve peer");
    assert_eq!(addrs, vec![sock]);

    let actual_err = handle
        .connection_for("127.0.0.1".to_string(), vec![sock])
        .await
        .expect_err("Expected connection to fail");
    assert!(actual_err.is(DownlinkErrorKind::Connection));
    let cause = actual_err
        .downcast_ref::<ConnectionError>()
        .expect("Expected a connection error");
    assert_eq!(cause, &err);
}

struct TrackingDownlink<LC> {
    spawned: Arc<Notify>,
    stopped: Arc<Notify>,
    inner: DownlinkTask<ValueDownlinkModel<i32, LC>>,
}

impl<LC> TrackingDownlink<LC> {
    fn new(
        spawned: Arc<Notify>,
        stopped: Arc<Notify>,
        inner: ValueDownlinkModel<i32, LC>,
    ) -> TrackingDownlink<LC> {
        TrackingDownlink {
            spawned,
            stopped,
            inner: DownlinkTask::new(inner),
        }
    }
}

impl<LC> Downlink for TrackingDownlink<LC>
where
    LC: ValueDownlinkLifecycle<i32> + 'static,
{
    fn kind(&self) -> DownlinkKind {
        DownlinkKind::Value
    }

    fn run(
        self,
        path: Address<Text>,
        config: DownlinkConfig,
        input: ByteReader,
        output: ByteWriter,
    ) -> BoxFuture<'static, Result<(), DownlinkTaskError>> {
        let TrackingDownlink {
            spawned,
            stopped,
            inner,
        } = self;
        let task = async move {
            spawned.notify_one();
            let result = inner.run(path, config, input, output).await;
            stopped.notify_one();
            result
        };
        Box::pin(task)
    }

    fn run_boxed(
        self: Box<Self>,
        path: Address<Text>,
        config: DownlinkConfig,
        input: ByteReader,
        output: ByteWriter,
    ) -> BoxFuture<'static, Result<(), DownlinkTaskError>> {
        (*self).run(path, config, input, output)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TestMessage<T> {
    Linked,
    Synced(T),
    Event(T),
    Set(Option<Arc<T>>, T),
    Unlinked,
}

fn default_lifecycle<T>(tx: mpsc::UnboundedSender<TestMessage<T>>) -> impl ValueDownlinkLifecycle<T>
where
    T: Clone + Send + Sync + 'static,
{
    BasicValueDownlinkLifecycle::<T>::default()
        .with(tx)
        .on_linked_blocking(|tx| {
            assert!(tx.send(TestMessage::Linked).is_ok());
        })
        .on_synced_blocking(|tx, v| {
            assert!(tx.send(TestMessage::Synced(v.clone())).is_ok());
        })
        .on_event_blocking(|tx, v| {
            assert!(tx.send(TestMessage::Event(v.clone())).is_ok());
        })
        .on_set_blocking(|tx, before, after| {
            assert!(tx
                .send(TestMessage::Set(before.cloned(), after.clone()))
                .is_ok());
        })
        .on_unlinked_blocking(|tx| {
            assert!(tx.send(TestMessage::Unlinked).is_ok());
        })
}

struct Lane<'l> {
    node: String,
    lane: String,
    server: RefCell<&'l mut Server>,
}

impl<'l> Lane<'l> {
    async fn read(&mut self) -> Envelope {
        let Lane { server, .. } = self;
        let Server { buf, transport } = server.get_mut();

        match transport.borrow_mut().read(buf).await.unwrap() {
            Message::Text => {}
            m => panic!("Unexpected message type: {:?}", m),
        }
        let read = String::from_utf8(buf.to_vec()).unwrap();
        buf.clear();

        parse_recognize::<Envelope>(Span::new(&read), false).unwrap()
    }

    async fn write(&mut self, env: Envelope) {
        let Lane { server, .. } = self;
        let Server { transport, .. } = server.get_mut();

        let response = print_recon(&env);
        transport
            .write(format!("{}", response), PayloadType::Text)
            .await
            .unwrap();
    }

    async fn await_link(&mut self) {
        match self.read().await {
            Envelope::Link {
                node_uri, lane_uri, ..
            } => {
                assert_eq!(node_uri, self.node);
                assert_eq!(lane_uri, self.lane);
                self.write(Envelope::Linked {
                    node_uri: node_uri.clone(),
                    lane_uri: lane_uri.clone(),
                    rate: None,
                    prio: None,
                    body: None,
                })
                .await;
            }
            e => panic!("Unexpected envelope {:?}", e),
        }
    }

    async fn await_sync<V: Into<Value>>(&mut self, val: V) {
        match self.read().await {
            Envelope::Sync {
                node_uri, lane_uri, ..
            } => {
                assert_eq!(node_uri, self.node);
                assert_eq!(lane_uri, self.lane);
                self.write(Envelope::Event {
                    node_uri: node_uri.clone(),
                    lane_uri: lane_uri.clone(),
                    body: Some(val.into()),
                })
                .await;
                self.write(Envelope::Synced {
                    node_uri: node_uri.clone(),
                    lane_uri: lane_uri.clone(),
                    body: None,
                })
                .await;
            }
            e => panic!("Unexpected envelope {:?}", e),
        }
    }

    async fn await_command(&mut self, expected: i32) {
        match self.read().await {
            Envelope::Command {
                node_uri,
                lane_uri,
                body: Some(val),
            } => {
                assert_eq!(node_uri, self.node);
                assert_eq!(lane_uri, self.lane);
                assert_eq!(val, Value::Int32Value(expected));
            }
            e => panic!("Unexpected envelope {:?}", e),
        }
    }

    async fn send_unlinked(&mut self) {
        self.write(Envelope::Unlinked {
            node_uri: self.node.clone().into(),
            lane_uri: self.lane.clone().into(),
            body: None,
        })
        .await;
    }

    async fn send_event<V: Into<Value>>(&mut self, val: V) {
        self.write(Envelope::Event {
            node_uri: self.node.clone().into(),
            lane_uri: self.lane.clone().into(),
            body: Some(val.into()),
        })
        .await;
    }

    async fn await_closed(&mut self) {
        let Lane { server, .. } = self;
        let Server { buf, transport } = server.get_mut();

        match transport.borrow_mut().read(buf).await.unwrap() {
            Message::Close(_) => {}
            m => panic!("Unexpected message type: {:?}", m),
        }
    }
}

struct Server {
    buf: BytesMut,
    transport: WebSocket<DuplexStream, NoExt>,
}

impl Server {
    fn lane_for<N, L>(&mut self, node: N, lane: L) -> Lane<'_>
    where
        N: ToString,
        L: ToString,
    {
        Lane {
            node: node.to_string(),
            lane: lane.to_string(),
            server: RefCell::new(self),
        }
    }
}

impl Server {
    fn new(transport: DuplexStream) -> Server {
        Server {
            buf: BytesMut::new(),
            transport: WebSocket::from_upgraded(
                WebSocketConfig::default(),
                transport,
                NegotiatedExtension::from(NoExt),
                BytesMut::default(),
                Role::Server,
            ),
        }
    }
}

struct DownlinkContext {
    handle: RawHandle,
    spawned: Arc<Notify>,
    stopped: Arc<Notify>,
    set_tx: mpsc::Sender<i32>,
    get_rx: watch::Receiver<Arc<i32>>,
    server: Server,
    promise: promise::Receiver<Result<(), DownlinkRuntimeError>>,
    stop_tx: trigger::Sender,
}

fn start() -> (RawHandle, Sender, Server) {
    let peer = SchemeHostPort::new(Scheme::Ws, "127.0.0.1".to_string(), 80);
    let sock: SocketAddr = "127.0.0.1:80".parse().unwrap();
    let (client, server) = duplex(128);
    let ext = MockExternalConnections::new(
        [(
            peer.clone(),
            SchemeSocketAddr::new(Scheme::Ws, sock.clone()),
        )],
        [("127.0.0.1:80".parse().unwrap(), client)],
    );
    let ws = MockWs::new([("127.0.0.1".to_string(), WsAction::Open)]);

    let (handle, stop) = start_runtime(Transport::new(ext, ws, non_zero_usize!(128)));
    (handle, stop, Server::new(server))
}

async fn test_downlink<LC, F, Fut>(lifecycle: LC, test: F)
where
    LC: ValueDownlinkLifecycle<i32> + Send + Sync + 'static,
    F: FnOnce(DownlinkContext) -> Fut,
    Fut: Future,
{
    let (handle, stop_tx, server) = start();
    let TrackingContext {
        spawned,
        stopped,
        set_tx,
        get_rx,
        promise,
    } = tracking_downlink(&handle, lifecycle, DownlinkRuntimeConfig::default()).await;

    let context = DownlinkContext {
        handle,
        spawned,
        stopped,
        set_tx,
        get_rx,
        server,
        promise,
        stop_tx,
    };
    assert!(timeout(Duration::from_secs(5), test(context)).await.is_ok());
}

#[tokio::test]
async fn spawns_downlink() {
    let (msg_tx, _msg_rx) = unbounded_channel();
    test_downlink(default_lifecycle(msg_tx), |ctx| async move {
        ctx.spawned.notified().await;
    })
    .await;
}

#[tokio::test]
async fn stops_on_disconnect() {
    let (msg_tx, _msg_rx) = unbounded_channel();
    test_downlink(default_lifecycle(msg_tx), |ctx| async move {
        let DownlinkContext {
            handle: _raw,
            stop_tx: _stop_tx,
            spawned,
            stopped,
            server,
            promise,
            ..
        } = ctx;
        spawned.notified().await;
        drop(server);
        stopped.notified().await;

        assert!(promise.await.is_ok());
    })
    .await;
}

#[tokio::test]
async fn lifecycle() {
    let (msg_tx, mut msg_rx) = unbounded_channel();
    test_downlink(default_lifecycle(msg_tx), |ctx| async move {
        let DownlinkContext {
            handle: _raw,
            spawned,
            stopped,
            set_tx,
            get_rx,
            mut server,
            promise,
            stop_tx,
        } = ctx;
        spawned.notified().await;

        let mut lane = server.lane_for("node", "lane");

        lane.await_link().await;
        assert_eq!(msg_rx.recv().await.unwrap(), TestMessage::Linked);

        lane.await_sync(7).await;
        assert_eq!(msg_rx.recv().await.unwrap(), TestMessage::Synced(7));
        {
            let state = get_rx.borrow();
            assert_eq!(*state.as_ref(), 7);
        }

        set_tx.send(13).await.unwrap();
        lane.await_command(13).await;

        lane.send_unlinked().await;
        assert_eq!(msg_rx.recv().await.unwrap(), TestMessage::Unlinked);

        assert!(stop_tx.trigger());
        lane.await_closed().await;

        assert_eq!(msg_rx.recv().now_or_never().unwrap(), None);
        stopped.notified().await;
        assert!(promise.await.unwrap().is_ok());
    })
    .await;
}

async fn tracking_downlink<LC>(
    handle: &RawHandle,
    lifecycle: LC,
    config: DownlinkRuntimeConfig,
) -> TrackingContext
where
    LC: ValueDownlinkLifecycle<i32> + Send + Sync + 'static,
{
    let spawned = Arc::new(Notify::new());
    let stopped = Arc::new(Notify::new());

    let (set_tx, set_rx) = mpsc::channel(128);
    let (get_tx, get_rx) = watch::channel(Arc::new(0));

    let downlink = TrackingDownlink::new(
        spawned.clone(),
        stopped.clone(),
        ValueDownlinkModel::new(set_rx, get_tx, lifecycle),
    );

    let promise = handle
        .run_downlink(
            AbsolutePath::new(
                "ws://127.0.0.1".parse().unwrap(),
                "node".into(),
                "lane".into(),
            ),
            config,
            Default::default(),
            DownlinkOptions::SYNC,
            downlink,
        )
        .await
        .expect("Failed to spawn downlink open request");

    TrackingContext {
        spawned,
        stopped,
        set_tx,
        get_rx,
        promise,
    }
}

struct TrackingContext {
    spawned: Arc<Notify>,
    stopped: Arc<Notify>,
    set_tx: mpsc::Sender<i32>,
    get_rx: watch::Receiver<Arc<i32>>,
    promise: promise::Receiver<Result<(), DownlinkRuntimeError>>,
}

struct State {
    fail_after_first: bool,
    tx: mpsc::UnboundedSender<TestMessage<i32>>,
    expected_events: Vec<i32>,
}

impl State {
    pub fn new(fail_after_first: bool, tx: mpsc::UnboundedSender<TestMessage<i32>>) -> State {
        State {
            fail_after_first,
            tx,
            expected_events: vec![1, 2, 3],
        }
    }

    fn make_lifecycle(
        tx: mpsc::UnboundedSender<TestMessage<i32>>,
        fail_after_first: bool,
    ) -> impl ValueDownlinkLifecycle<i32> {
        BasicValueDownlinkLifecycle::default()
            .with(State::new(fail_after_first, tx))
            .on_linked_blocking(|state| {
                assert!(state.tx.send(TestMessage::Linked).is_ok());
            })
            .on_synced_blocking(|state, v| {
                assert!(state.tx.send(TestMessage::Synced(*v)).is_ok());
            })
            .on_event_blocking(|state, v| {
                if state.fail_after_first && state.expected_events.len() != 3 {
                    panic!()
                } else if state.expected_events.len() != 0 {
                    let i = state.expected_events.remove(0);
                    assert_eq!(*v, i);
                    assert!(state.tx.send(TestMessage::Event(i)).is_ok());
                } else {
                    panic!()
                }
            })
            .on_set_blocking(|state, before, after| {
                assert!(state
                    .tx
                    .send(TestMessage::Set(before.cloned(), after.clone()))
                    .is_ok());
            })
            .on_unlinked_blocking(|state| {
                assert!(state.tx.send(TestMessage::Unlinked).is_ok());
            })
    }
}

pub struct Bias<L, R> {
    towards: L,
    other: R,
}

fn bias<L, R>(towards: L, other: R) -> impl Future<Output = Either<L::Output, R::Output>>
where
    L: Future + Unpin,
    R: Future + Unpin,
{
    Bias { towards, other }
}

impl<L, R> Future for Bias<L, R>
where
    L: Future + Unpin,
    R: Future + Unpin,
{
    type Output = Either<L::Output, R::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Poll::Ready(l) = Pin::new(&mut self.towards).poll(cx) {
            Poll::Ready(Either::Left(l))
        } else {
            Pin::new(&mut self.other).poll(cx).map(Either::Right)
        }
    }
}

struct ClosableDownlink<LC> {
    trigger: trigger::Receiver,
    stopped: Arc<Notify>,
    inner: DownlinkTask<ValueDownlinkModel<i32, LC>>,
}

impl<LC> ClosableDownlink<LC> {
    pub fn new(
        trigger: trigger::Receiver,
        stopped: Arc<Notify>,
        inner: ValueDownlinkModel<i32, LC>,
    ) -> ClosableDownlink<LC> {
        ClosableDownlink {
            trigger,
            stopped,
            inner: DownlinkTask::new(inner),
        }
    }
}

impl<LC> Downlink for ClosableDownlink<LC>
where
    LC: ValueDownlinkLifecycle<i32> + 'static,
{
    fn kind(&self) -> DownlinkKind {
        DownlinkKind::Value
    }

    fn run(
        self,
        path: Address<Text>,
        config: DownlinkConfig,
        input: ByteReader,
        output: ByteWriter,
    ) -> BoxFuture<'static, Result<(), DownlinkTaskError>> {
        let ClosableDownlink {
            stopped,
            trigger,
            inner,
        } = self;
        let task = async move {
            match bias(trigger, inner.run(path, config, input, output)).await {
                Either::Left(Ok(())) => {
                    stopped.notify_one();
                    Ok(())
                }
                Either::Left(Err(e)) => {
                    panic!("{:?}", e)
                }
                Either::Right(out) => {
                    panic!("Downlink completed before trigger. Output: {:?}", out)
                }
            }
        };
        Box::pin(task)
    }

    fn run_boxed(
        self: Box<Self>,
        path: Address<Text>,
        config: DownlinkConfig,
        input: ByteReader,
        output: ByteWriter,
    ) -> BoxFuture<'static, Result<(), DownlinkTaskError>> {
        (self).run(path, config, input, output)
    }
}

struct ClosableContext {
    stopped: Arc<Notify>,
    shutdown: trigger::Sender,
    _set_tx: mpsc::Sender<i32>,
    get_rx: watch::Receiver<Arc<i32>>,
    promise: promise::Receiver<Result<(), DownlinkRuntimeError>>,
}

async fn closable_downlink<LC>(
    handle: &RawHandle,
    lifecycle: LC,
    config: DownlinkRuntimeConfig,
) -> ClosableContext
where
    LC: ValueDownlinkLifecycle<i32> + Send + Sync + 'static,
{
    let stopped = Arc::new(Notify::new());

    let (set_tx, set_rx) = mpsc::channel(128);
    let (get_tx, get_rx) = watch::channel(Arc::new(0));

    let (shutdown_tx, shutdown_rx) = trigger::trigger();

    let downlink = ClosableDownlink::new(
        shutdown_rx,
        stopped.clone(),
        ValueDownlinkModel::new(set_rx, get_tx, lifecycle),
    );

    let promise = handle
        .run_downlink(
            AbsolutePath::new(
                "ws://127.0.0.1".parse().unwrap(),
                "node".into(),
                "lane".into(),
            ),
            config,
            Default::default(),
            DownlinkOptions::SYNC,
            downlink,
        )
        .await
        .expect("Failed to spawn downlink open request");

    ClosableContext {
        stopped,
        shutdown: shutdown_tx,
        _set_tx: set_tx,
        get_rx,
        promise,
    }
}

/// Tests that disjoint runtime configurations start different runtimes for the same host
#[tokio::test]
async fn different_configurations() {
    let (handle, _stop_tx, mut server) = start();

    let (succeeding_tx, succeeding_rx) = mpsc::unbounded_channel();
    let succeeding_lifecycle = State::make_lifecycle(succeeding_tx, false);

    let (failing_tx, failing_rx) = mpsc::unbounded_channel();
    let failing_lifecycle = State::make_lifecycle(failing_tx, true);

    let succeeding_context = tracking_downlink(
        &handle,
        succeeding_lifecycle,
        DownlinkRuntimeConfig {
            empty_timeout: Duration::from_secs(30),
            ..Default::default()
        },
    )
    .await;
    let trigger_context = closable_downlink(
        &handle,
        failing_lifecycle,
        DownlinkRuntimeConfig {
            empty_timeout: Duration::from_secs(2),
            ..Default::default()
        },
    )
    .await;

    let mut receivers = (succeeding_rx, failing_rx);

    let mut lane = server.lane_for("node", "lane");
    lane.await_link().await;

    type Channel = mpsc::UnboundedReceiver<TestMessage<i32>>;
    async fn op_receivers<'f, Fun, Fut>(pair: &'f mut (Channel, Channel), f: Fun)
    where
        Fun: Fn(&'f mut Channel) -> Fut,
        Fut: Future<Output = ()> + 'f,
    {
        f(&mut pair.0).await;
        f(&mut pair.1).await;
    }

    op_receivers(&mut receivers, |rx| async move {
        assert_eq!(rx.recv().await.unwrap(), TestMessage::Linked);
    })
    .await;

    lane.await_sync(7).await;

    op_receivers(&mut receivers, |rx| async move {
        assert_eq!(rx.recv().await.unwrap(), TestMessage::Synced(7));
    })
    .await;

    {
        let state = succeeding_context.get_rx.borrow();
        assert_eq!(*state.as_ref(), 7);
    }
    {
        let state = trigger_context.get_rx.borrow();
        assert_eq!(*state.as_ref(), 7);
    }

    lane.send_event(1).await;

    op_receivers(&mut receivers, |rx| async move {
        assert_eq!(rx.recv().await.unwrap(), TestMessage::Event(1));
    })
    .await;

    op_receivers(&mut receivers, |rx| async move {
        assert_eq!(
            rx.recv().await.unwrap(),
            TestMessage::Set(Some(Arc::new(7)), 1)
        );
    })
    .await;

    assert!(trigger_context.shutdown.trigger());
    trigger_context.stopped.notified().await;

    assert!(receivers.0.recv().now_or_never().is_none());
    assert!(succeeding_context
        .stopped
        .notified()
        .now_or_never()
        .is_none());
    assert!(succeeding_context.promise.now_or_never().is_none());

    assert!(trigger_context.promise.await.is_ok());
}

#[tokio::test]
async fn failed_handshake() {
    let peer = SchemeHostPort::new(Scheme::Ws, "127.0.0.1".to_string(), 80);
    let sock: SocketAddr = "127.0.0.1:80".parse().unwrap();
    let (client, _server) = duplex(128);
    let ext = MockExternalConnections::new(
        [(
            peer.clone(),
            SchemeSocketAddr::new(Scheme::Ws, sock.clone()),
        )],
        [("127.0.0.1:80".parse().unwrap(), client)],
    );
    let err = ConnectionError::Io(IoError::new(ErrorKind::NotFound, None));
    let ws = MockWs::new([("127.0.0.1".to_string(), WsAction::Fail(err.clone()))]);

    let (handle, _stop) = start_runtime(Transport::new(ext, ws, non_zero_usize!(128)));

    let spawned = Arc::new(Notify::new());
    let stopped = Arc::new(Notify::new());

    let (_set_tx, set_rx) = mpsc::channel(128);
    let (get_tx, _get_rx) = watch::channel(Arc::new(0));

    let downlink = TrackingDownlink::new(
        spawned.clone(),
        stopped.clone(),
        ValueDownlinkModel::new(set_rx, get_tx, BasicValueDownlinkLifecycle::default()),
    );

    let promise = handle
        .run_downlink(
            AbsolutePath::new(
                "ws://127.0.0.1".parse().unwrap(),
                "node".into(),
                "lane".into(),
            ),
            Default::default(),
            Default::default(),
            DownlinkOptions::SYNC,
            downlink,
        )
        .await;
    assert!(promise.is_err());
    let actual_err = promise.unwrap_err();
    assert!(actual_err.is(DownlinkErrorKind::WebsocketNegotiationFailed));
    let cause = actual_err
        .downcast_ref::<ConnectionError>()
        .expect("Incorrect error");
    assert_eq!(cause, &err);
}
