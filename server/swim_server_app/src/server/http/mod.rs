// Copyright 2015-2023 Swim Inc.
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
    marker::PhantomData,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use bytes::{Bytes, BytesMut};
use futures::{
    future::BoxFuture,
    ready,
    stream::{BoxStream, FuturesUnordered},
    Future, FutureExt, Stream, StreamExt,
};
use hyper::{
    body::to_bytes,
    server::conn::http1,
    service::Service,
    upgrade::{Parts, Upgraded},
    Body, Request, Response, StatusCode,
};
use pin_project::pin_project;
use ratchet::{Extension, ExtensionProvider, ProtocolRegistry, WebSocket, WebSocketConfig};
use swim_api::{agent::HttpLaneRequest, net::Scheme};
use swim_http::{Negotiated, SockUnwrap, UpgradeError, UpgradeFuture};
use swim_model::http::HttpRequest;
use swim_remote::{
    net::{Listener, ListenerError, ListenerResult},
    ws::{RatchetError, WebsocketClient, WebsocketServer, WsOpenFuture, PROTOCOLS},
};
use swim_remote::{AgentResolutionError, FindNode, NoSuchAgent};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
    time::{sleep, Sleep},
};

use crate::config::HttpConfig;

use self::resolver::Resolver;

mod resolver;
#[cfg(test)]
mod tests;

pub type WsWithAddr<Ext, Sock> = (WebSocket<Sock, Ext>, Scheme, SocketAddr);
pub type ListenResult<Ext, Sock> = Result<WsWithAddr<Ext, Sock>, ListenerError>;

/// Hyper based web-server that will attempt to negotiate a server websocket over
/// every incoming connection.
///
/// # Arguments
/// * `listener` - Listener providing a stream of incoming connections.
/// * `extension_provider` - Web socket extension provider.
/// * `config` - HTTP server configuration parameters.
pub fn hyper_http_server<Sock, L, Ext>(
    listener: L,
    find: mpsc::Sender<FindNode>,
    extension_provider: Ext,
    config: HttpConfig,
) -> impl Stream<Item = ListenResult<Ext::Extension, Sock>> + Send
where
    Sock: Unpin + Send + Sync + AsyncRead + AsyncWrite + 'static,
    L: Listener<Sock> + Send,
    Ext: ExtensionProvider + Send + Sync + 'static,
    Ext::Extension: Send + Unpin,
{
    let resolver = Resolver::new(find, config.resolver_timeout);
    let state = StreamState::<L::AcceptStream, Sock, Ext, _, _>::new(
        listener.into_stream(),
        extension_provider,
        resolver,
        config,
        |sock, mut svc| async move {
            let result = http1::Builder::new()
                .serve_connection(sock, &mut svc)
                .with_upgrades()
                .await;
            result.map(move |_| svc.into_upgrade_fut())
        },
    );

    futures::stream::unfold(state, |mut state| async move {
        state.next().await.map(move |result| (result, state))
    })
}

type WebsocketParts<Sock, Ext> = (WebSocket<Sock, Ext>, Scheme, SocketAddr);

enum TaskResult<Ext, Sock> {
    ConnectionComplete(Result<Option<UpgradeFutureWithSock<Ext, Sock>>, hyper::Error>),
    UpgradeComplete(Result<Box<WebsocketParts<Sock, Ext>>, hyper::Error>),
}

#[pin_project(project = TaskFutureProj)]
enum TaskFuture<Sock, Ext, Con>
where
    Sock: AsyncRead + AsyncWrite + Unpin + 'static,
    Ext: ExtensionProvider,
    Ext::Extension: Send,
{
    Connection(#[pin] Con),
    Upgrade(UpgradeFutureWithSock<Ext::Extension, Sock>),
}

impl<Sock, Ext, Con> Future for TaskFuture<Sock, Ext, Con>
where
    Sock: AsyncRead + AsyncWrite + Unpin + 'static,
    Ext: ExtensionProvider,
    Ext::Extension: Send + Unpin,
    Con: Future<Output = Result<Option<UpgradeFutureWithSock<Ext::Extension, Sock>>, hyper::Error>>,
{
    type Output = TaskResult<Ext::Extension, Sock>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project() {
            TaskFutureProj::Connection(fut) => {
                Poll::Ready(TaskResult::ConnectionComplete(ready!(fut.poll(cx))))
            }
            TaskFutureProj::Upgrade(fut) => Poll::Ready(TaskResult::UpgradeComplete(
                ready!(fut.poll_unpin(cx)).map(Box::new),
            )),
        }
    }
}

struct StreamState<L, Sock, Ext, Con, FC>
where
    Sock: AsyncRead + AsyncWrite + Unpin + 'static,
    Ext: ExtensionProvider,
    Ext::Extension: Send,
{
    listener_stream: L,
    connection_tasks: FuturesUnordered<TaskFuture<Sock, Ext, Con>>,
    upgrader: Upgrader<Ext>,
    connect_fn: FC,
    max_pending: usize,
    timeout: Pin<Box<Sleep>>,
    timeout_enabled: bool,
}

impl<L, Sock, Ext, Con, FC> StreamState<L, Sock, Ext, Con, FC>
where
    Sock: AsyncRead + AsyncWrite + Unpin + 'static,
    Ext: ExtensionProvider + Send + Sync,
    Ext::Extension: Send,
    FC: Fn(Sock, UpgradeService<Ext, Sock>) -> Con + Copy + Send + 'static,
{
    fn new(
        listener_stream: L,
        extension_provider: Ext,
        resolver: resolver::Resolver,
        config: HttpConfig,
        connect_fn: FC,
    ) -> Self {
        let connection_tasks = FuturesUnordered::new();
        StreamState {
            listener_stream,
            connection_tasks,
            upgrader: Upgrader::new(
                extension_provider,
                resolver,
                config.websockets,
                config.http_request_timeout,
            ),
            connect_fn,
            max_pending: config.max_http_requests.get(),
            timeout: Box::pin(sleep(config.resolver_timeout)),
            timeout_enabled: false,
        }
    }
}

enum Event<Sock, Ext> {
    TaskComplete(TaskResult<Ext, Sock>),
    Incoming(Sock, Scheme, SocketAddr),
    IncomingFailed(ListenerError),
    Timeout,
    Continue,
    Stop,
}

impl<L, Sock, Ext, Con, FC> StreamState<L, Sock, Ext, Con, FC>
where
    Sock: AsyncRead + AsyncWrite + Unpin + 'static,
    L: Stream<Item = ListenerResult<(Sock, Scheme, SocketAddr)>> + Send + Unpin,
    Ext: ExtensionProvider + Send + Sync,
    Ext::Extension: Send + Unpin,
    Con: Future<Output = Result<Option<UpgradeFutureWithSock<Ext::Extension, Sock>>, hyper::Error>>,
    FC: Fn(Sock, UpgradeService<Ext, Sock>) -> Con + Copy + Send + 'static,
{
    async fn next(&mut self) -> Option<ListenResult<Ext::Extension, Sock>> {
        let StreamState {
            listener_stream,
            connection_tasks,
            upgrader,
            connect_fn,
            max_pending,
            timeout,
            timeout_enabled,
        } = self;
        loop {
            let event = if connection_tasks.len() < *max_pending {
                tokio::select! {
                    biased;
                    _ = &mut*timeout, if *timeout_enabled => Event::Timeout,
                    maybe_event = connection_tasks.next(), if !connection_tasks.is_empty() => {
                        if let Some(ev) = maybe_event {
                            Event::TaskComplete(ev)
                        } else {
                            Event::Continue
                        }
                    }
                    maybe_incoming = listener_stream.next() => {
                        match maybe_incoming {
                            Some(Ok((sock, scheme, addr))) => {
                                Event::Incoming(sock, scheme, addr)
                            },
                            Some(Err(err)) => {
                                Event::IncomingFailed(err)
                            },
                            _ => {
                                Event::Stop
                            },
                        }
                    }
                }
            } else {
                tokio::select! {
                    biased;
                    _ = &mut*timeout, if *timeout_enabled => Event::Timeout,
                    some_ev = connection_tasks.next() => {
                        if let Some(ev) = some_ev {
                            Event::TaskComplete(ev)
                        } else {
                            Event::Continue
                        }
                    }
                }
            };
            match event {
                Event::TaskComplete(TaskResult::ConnectionComplete(Err(err))) => {
                    break Some(Err(ListenerError::NegotiationFailed(Box::new(err))));
                }
                Event::TaskComplete(TaskResult::ConnectionComplete(Ok(Some(fut)))) => {
                    connection_tasks.push(TaskFuture::Upgrade(fut));
                }
                Event::TaskComplete(TaskResult::ConnectionComplete(Ok(_))) => {
                    if !*timeout_enabled {
                        if let Some(t) = upgrader.resolver_cleanup() {
                            *timeout_enabled = true;
                            timeout.as_mut().reset(tokio::time::Instant::from_std(t));
                        }
                    }
                    continue;
                }
                Event::TaskComplete(TaskResult::UpgradeComplete(Ok(parts))) => {
                    break Some(Ok(*parts));
                }
                Event::TaskComplete(TaskResult::UpgradeComplete(Err(err))) => {
                    break Some(Err(ListenerError::NegotiationFailed(Box::new(err))));
                }
                Event::Incoming(sock, scheme, addr) => {
                    let svc = upgrader.make_service(scheme, addr);
                    connection_tasks.push(TaskFuture::Connection(connect_fn(sock, svc)));
                    continue;
                }
                Event::IncomingFailed(err) => break Some(Err(err)),
                Event::Continue => continue,
                Event::Stop => break None,
                Event::Timeout => {
                    if let Some(t) = upgrader.resolver_cleanup() {
                        *timeout_enabled = true;
                        timeout.as_mut().reset(tokio::time::Instant::from_std(t));
                    } else {
                        *timeout_enabled = false;
                    }
                }
            }
        }
    }
}

fn perform_upgrade<Ext, Sock, Err>(
    request: Request<Body>,
    config: WebSocketConfig,
    result: Result<Negotiated<'_, Ext>, UpgradeError<Err>>,
    target: &mut Option<UpgradeFutureWithSock<Ext, Sock>>,
    scheme: Scheme,
    addr: SocketAddr,
) -> Result<Response<Body>, hyper::Error>
where
    Sock: AsyncRead + AsyncWrite + Unpin + 'static,
    Ext: Extension + Send,
    Err: std::error::Error + Send,
{
    match result {
        Ok(negotiated) => {
            let (response, upgrade_fut) = swim_http::upgrade(
                request,
                negotiated,
                Some(config),
                ReclaimSock::<Sock>::default(),
            );
            *target = Some(UpgradeFutureWithSock::new(upgrade_fut, scheme, addr));
            Ok(response)
        }
        Err(err) => Ok(swim_http::fail_upgrade(err)),
    }
}

struct Upgrader<Ext: ExtensionProvider> {
    extension_provider: Arc<Ext>,
    resolver: resolver::Resolver,
    config: WebSocketConfig,
    request_timeout: Duration,
}

impl<Ext> Upgrader<Ext>
where
    Ext: ExtensionProvider + Send + Sync,
{
    fn new(
        extension_provider: Ext,
        resolver: resolver::Resolver,
        config: WebSocketConfig,
        request_timeout: Duration,
    ) -> Self {
        Upgrader {
            extension_provider: Arc::new(extension_provider),
            resolver,
            config,
            request_timeout,
        }
    }

    fn resolver_cleanup(&self) -> Option<Instant> {
        self.resolver.check_access_times()
    }

    fn make_service<Sock>(&self, scheme: Scheme, addr: SocketAddr) -> UpgradeService<Ext, Sock>
    where
        Sock: AsyncRead + AsyncWrite + Unpin + 'static,
    {
        let Upgrader {
            extension_provider,
            resolver,
            config,
            request_timeout,
        } = self;
        UpgradeService::new(
            extension_provider.clone(),
            resolver.clone(),
            *config,
            scheme,
            addr,
            *request_timeout,
        )
    }
}

struct UpgradeService<Ext: ExtensionProvider, Sock> {
    extension_provider: Arc<Ext>,
    resolver: resolver::Resolver,
    upgrade_fut: Option<UpgradeFutureWithSock<Ext::Extension, Sock>>,
    config: WebSocketConfig,
    scheme: Scheme,
    addr: SocketAddr,
    request_timeout: Duration,
}

impl<Ext: ExtensionProvider, Sock> UpgradeService<Ext, Sock>
where
    Sock: AsyncRead + AsyncWrite + Unpin + 'static,
{
    fn new(
        extension_provider: Arc<Ext>,
        resolver: resolver::Resolver,
        config: WebSocketConfig,
        scheme: Scheme,
        addr: SocketAddr,
        request_timeout: Duration,
    ) -> Self {
        UpgradeService {
            extension_provider,
            resolver,
            upgrade_fut: None,
            config,
            scheme,
            addr,
            request_timeout,
        }
    }

    fn into_upgrade_fut(self) -> Option<UpgradeFutureWithSock<Ext::Extension, Sock>> {
        self.upgrade_fut
    }
}

impl<Ext, Sock> Service<Request<Body>> for UpgradeService<Ext, Sock>
where
    Sock: AsyncRead + AsyncWrite + Unpin + 'static,
    Ext: ExtensionProvider,
    Ext::Extension: Send,
{
    type Response = Response<Body>;

    type Error = hyper::Error;

    type Future = BoxFuture<'static, Result<Response<Body>, hyper::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        let UpgradeService {
            extension_provider,
            upgrade_fut,
            config,
            scheme,
            addr,
            resolver,
            request_timeout,
        } = self;
        let result =
            swim_http::negotiate_upgrade(&request, &PROTOCOLS, extension_provider.as_ref())
                .transpose();
        if let Some(result) = result {
            let upgrade_result =
                perform_upgrade(request, *config, result, upgrade_fut, *scheme, *addr);
            async move { upgrade_result }.boxed()
        } else {
            serve_request(request, *request_timeout, resolver.clone())
                .map(Ok)
                .boxed()
        }
    }
}

struct ReclaimSock<Sock>(PhantomData<fn(Upgraded) -> Sock>);

impl<Sock> Default for ReclaimSock<Sock> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<Sock> SockUnwrap for ReclaimSock<Sock>
where
    Sock: AsyncRead + AsyncWrite + Unpin + 'static,
{
    type Sock = Sock;

    fn unwrap_sock(&self, upgraded: Upgraded) -> (Self::Sock, BytesMut) {
        let Parts { io, read_buf, .. } = upgraded
            .downcast::<Sock>()
            .expect("Hyper should not alter underlying socket type.");
        (io, BytesMut::from(read_buf.as_ref()))
    }
}

struct UpgradeFutureWithSock<Ext, Sock> {
    inner: UpgradeFuture<Ext, ReclaimSock<Sock>>,
    scheme: Scheme,
    addr: SocketAddr,
}

impl<Ext, Sock> UpgradeFutureWithSock<Ext, Sock> {
    pub fn new(
        inner: UpgradeFuture<Ext, ReclaimSock<Sock>>,
        scheme: Scheme,
        addr: SocketAddr,
    ) -> Self {
        UpgradeFutureWithSock {
            inner,
            scheme,
            addr,
        }
    }
}

impl<Ext, Sock> Future for UpgradeFutureWithSock<Ext, Sock>
where
    Sock: AsyncRead + AsyncWrite + Unpin + 'static,
    Ext: Extension + Unpin,
{
    type Output = Result<(WebSocket<Sock, Ext>, Scheme, SocketAddr), hyper::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let UpgradeFutureWithSock {
            inner,
            scheme,
            addr,
        } = self.get_mut();
        let ws = ready!(inner.poll_unpin(cx))?;
        Poll::Ready(Ok((ws, *scheme, *addr)))
    }
}

/// Implementation of [`WebsocketServer`] and [`WebsocketClient`] that uses [`hyper`] to upgrade
/// HTTP connections to [`ratchet`] web-socket connections.
pub struct HyperWebsockets {
    config: HttpConfig,
}

impl HyperWebsockets {
    /// #Arguments
    ///
    /// * `config` - HTTP server configuration.
    /// will handle concurrently.
    pub fn new(config: HttpConfig) -> Self {
        HyperWebsockets { config }
    }
}

impl WebsocketServer for HyperWebsockets {
    type WsStream<Sock, Ext> =
        BoxStream<'static, Result<(WebSocket<Sock, Ext>, SocketAddr), ListenerError>>;

    fn wrap_listener<Sock, L, Provider>(
        &self,
        listener: L,
        provider: Provider,
        find: mpsc::Sender<FindNode>,
    ) -> Self::WsStream<Sock, Provider::Extension>
    where
        Sock: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
        L: Listener<Sock> + Send + 'static,
        Provider: ExtensionProvider + Send + Sync + Unpin + 'static,
        Provider::Extension: Send + Sync + Unpin + 'static,
    {
        let HyperWebsockets { config } = self;
        hyper_http_server(listener, find, provider, *config)
            .map(|r| r.map(|(ws, _, addr)| (ws, addr)))
            .boxed()
    }
}

impl WebsocketClient for HyperWebsockets {
    fn open_connection<'a, Sock, Provider>(
        &self,
        socket: Sock,
        provider: &'a Provider,
        addr: String,
    ) -> WsOpenFuture<'a, Sock, Provider::Extension, RatchetError>
    where
        Sock: AsyncRead + AsyncWrite + Send + Unpin + 'static,
        Provider: ExtensionProvider + Send + Sync + 'static,
        Provider::Extension: Send + Sync + 'static,
    {
        let HyperWebsockets { config, .. } = self;

        let config = *config;
        Box::pin(async move {
            let subprotocols = ProtocolRegistry::new(PROTOCOLS.iter().copied())?;
            let socket =
                ratchet::subscribe_with(config.websockets, socket, addr, provider, subprotocols)
                    .await?
                    .into_websocket();
            Ok(socket)
        })
    }
}

fn bad_request(msg: String) -> Response<Body> {
    let mut response = Response::default();
    *response.status_mut() = StatusCode::BAD_REQUEST;
    *response.body_mut() = Bytes::from(msg).into();
    response
}

fn error(msg: &'static str) -> Response<Body> {
    let mut response = Response::default();
    *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    *response.body_mut() = Bytes::from_static(msg.as_bytes()).into();
    response
}

fn req_timeout() -> Response<Body> {
    let mut response = Response::default();
    *response.status_mut() = StatusCode::REQUEST_TIMEOUT;
    *response.body_mut() = Bytes::from("The agent failed to respond.".to_string()).into();
    response
}

fn not_found(node: &str) -> Response<Body> {
    let mut response = Response::default();
    *response.status_mut() = StatusCode::NOT_FOUND;
    *response.body_mut() = Bytes::from(format!("No agent at '{}'", node)).into();
    response
}

fn unavailable() -> Response<Body> {
    let mut response = Response::default();
    *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    *response.body_mut() = Bytes::from_static(b"The server is stopping.").into();
    response
}

async fn serve_request(
    request: Request<Body>,
    timeout: Duration,
    resolver: Resolver,
) -> Response<Body> {
    let http_request = match HttpRequest::try_from(request) {
        Ok(req) => req,
        Err(err) => return bad_request(err.to_string()),
    };
    let bytes_request = match http_request.try_transform(to_bytes).await {
        Ok(req) => req,
        Err(err) => return bad_request(err.to_string()),
    };

    let (message, response_rx) = HttpLaneRequest::new(bytes_request);
    if let Err(err) = resolver.send(message).await {
        match err {
            AgentResolutionError::NotFound(NoSuchAgent { node, .. }) => {
                return not_found(node.as_str())
            }
            AgentResolutionError::PlaneStopping => return unavailable(),
        }
    }
    match tokio::time::timeout(timeout, response_rx).await {
        Ok(Ok(response)) => match Response::try_from(response) {
            Ok(res) => res.map(|b| b.into()),
            Err(_) => error("Invalid response."),
        },
        Ok(Err(_)) => error("The agent failed to provide a response."),
        Err(_) => req_timeout(),
    }
}
