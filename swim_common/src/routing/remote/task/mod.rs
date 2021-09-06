// Copyright 2015-2021 SWIM.AI inc.
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

#[cfg(test)]
mod tests;

use crate::model::parser::{self, ParseFailure};
use crate::routing::error::{
    CloseError, CloseErrorKind, ConnectionError, ProtocolError, ProtocolErrorKind, ResolutionError,
    ResolutionErrorKind,
};
use crate::routing::remote::config::RemoteConnectionsConfig;
use crate::routing::remote::router::RemoteRouter;
use crate::routing::remote::{BidirectionalReceiverRequest, RemoteRoutingRequest};
use crate::routing::ws::selector::{SelectorResult, WsStreamSelector};
use crate::routing::ws::{CloseCode, CloseReason, JoinedStreamSink, WsMessage};
use crate::routing::RouterError;
use crate::routing::{
    ConnectionDropped, Route, Router, RouterFactory, RoutingAddr, TaggedEnvelope, TaggedSender,
};
use crate::warp::envelope::{Envelope, EnvelopeHeader, EnvelopeParseErr, OutgoingHeader};
use crate::warp::path::RelativePath;
use either::Either;
use futures::future::join_all;
use futures::future::{join, BoxFuture};
use futures::{select_biased, stream, FutureExt, Sink, Stream, StreamExt};
use pin_utils::pin_mut;
use slab::Slab;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{sleep, Instant};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{event, Level};
use utilities::errors::Recoverable;
use utilities::future::retryable::strategy::RetryStrategy;
use utilities::sync::trigger;
use utilities::task::Spawner;
use utilities::uri::{BadRelativeUri, RelativeUri};

/// A task that manages reading from and writing to a web-sockets channel.
pub struct ConnectionTask<Str, Router> {
    tag: RoutingAddr,
    ws_stream: Str,
    messages: mpsc::Receiver<TaggedEnvelope>,
    message_injector: mpsc::Sender<TaggedEnvelope>,
    router: Router,
    bidirectional_request_rx: mpsc::Receiver<BidirectionalReceiverRequest>,
    stop_signal: trigger::Receiver,
    config: RemoteConnectionsConfig,
}

const ZERO: Duration = Duration::from_secs(0);

/// Possible ways in which the task can end.
#[derive(Debug)]
enum Completion {
    Failed(ConnectionError),
    TimedOut,
    StoppedRemotely,
    StoppedLocally,
}

impl From<ParseFailure> for Completion {
    fn from(err: ParseFailure) -> Self {
        Completion::Failed(ConnectionError::Protocol(ProtocolError::new(
            ProtocolErrorKind::Warp,
            Some(err.to_string()),
        )))
    }
}

impl From<EnvelopeParseErr> for Completion {
    fn from(err: EnvelopeParseErr) -> Self {
        Completion::Failed(ConnectionError::Protocol(ProtocolError::new(
            ProtocolErrorKind::Warp,
            Some(err.to_string()),
        )))
    }
}

const IGNORING_MESSAGE: &str = "Ignoring unexpected message.";
const ERROR_ON_CLOSE: &str = "Error whilst closing connection.";
const BIDIRECTIONAL_RECEIVER_ERROR: &str = "Error whilst sending a bidirectional receiver.";

impl<Str, R> ConnectionTask<Str, R>
where
    Str: JoinedStreamSink<WsMessage, ConnectionError> + Unpin,
    R: Router,
{
    /// Create a new task.
    ///
    /// #Arguments
    ///
    /// * `tag`  - The routing address of the connection.
    /// * `ws_stream` - The joined sink/stream that implements the web sockets protocol.
    /// * `router` - Router to route incoming messages to the appropriate destination.
    /// * `messages_tx` - Allows messages to be injected into the outgoing stream.
    /// * `messages_rx`- Stream of messages to be sent into the sink.
    /// * `bidirectional_request_rx` - Stream of bidirectional requests.
    /// * `stop_signal` - Signals to the task that it should stop.
    /// * `config` - Configuration for the connection task.
    /// runtime.
    pub fn new(
        tag: RoutingAddr,
        ws_stream: Str,
        router: R,
        (messages_tx, messages_rx): (mpsc::Sender<TaggedEnvelope>, mpsc::Receiver<TaggedEnvelope>),
        bidirectional_request_rx: mpsc::Receiver<BidirectionalReceiverRequest>,
        stop_signal: trigger::Receiver,
        config: RemoteConnectionsConfig,
    ) -> Self {
        assert!(config.activity_timeout > ZERO);
        ConnectionTask {
            tag,
            ws_stream,
            messages: messages_rx,
            message_injector: messages_tx,
            router,
            bidirectional_request_rx,
            stop_signal,
            config,
        }
    }

    pub async fn run(self) -> ConnectionDropped {
        let ConnectionTask {
            tag,
            mut ws_stream,
            messages,
            message_injector,
            mut router,
            bidirectional_request_rx,
            stop_signal,
            config,
        } = self;

        let outgoing_payloads = ReceiverStream::new(messages).map(Into::into);
        let mut bidirectional_request_rx = ReceiverStream::new(bidirectional_request_rx).fuse();
        let mut bidirectional_connections = Slab::new();

        let mut selector = WsStreamSelector::new(
            &mut ws_stream,
            outgoing_payloads,
            config.write_timeout,
            |dur| ConnectionError::WriteTimeout(*dur),
        );

        let mut stop_fused = stop_signal.fuse();
        let timeout = sleep(config.activity_timeout);
        pin_mut!(timeout);

        let mut resolved: HashMap<RelativePath, Route> = HashMap::new();
        let yield_mod = config.yield_after.get();
        let mut iteration_count: usize = 0;

        let completion = loop {
            timeout.as_mut().reset(
                Instant::now()
                    .checked_add(config.activity_timeout)
                    .expect("Timer overflow."),
            );
            let next: Option<
                Either<
                    BidirectionalReceiverRequest,
                    Result<SelectorResult<WsMessage>, ConnectionError>,
                >,
            > = select_biased! {
                _ = stop_fused => {
                    break Completion::StoppedLocally;
                },
                _ = (&mut timeout).fuse() => {
                    break Completion::TimedOut;
                }
                conn_request = bidirectional_request_rx.next() => conn_request.map(Either::Left),
                event = selector.select_rw() => event.map(Either::Right),
            };

            if let Some(event) = next {
                // disable the linter here as there are to-dos
                #[allow(clippy::collapsible_if)]
                match event {
                    Either::Left(receiver_request) => {
                        let (tx, rx) = mpsc::channel(config.channel_buffer_size.get());
                        bidirectional_connections.insert(TaggedSender::new(tag, tx));

                        if receiver_request.send(rx).is_err() {
                            event!(Level::WARN, BIDIRECTIONAL_RECEIVER_ERROR);
                        }
                    }
                    Either::Right(Ok(SelectorResult::Read(msg))) => match msg {
                        WsMessage::Text(msg) => match read_envelope(&msg) {
                            Ok(envelope) => {
                                let (done_tx, done_rx) = trigger::trigger();

                                let write_task = write_to_socket_only(
                                    &mut selector,
                                    done_rx,
                                    yield_mod,
                                    &mut iteration_count,
                                );

                                let dispatch_task = async {
                                    let dispatch_result = dispatch_envelope(
                                        &mut router,
                                        &mut bidirectional_connections,
                                        &mut resolved,
                                        envelope,
                                        config.connection_retries,
                                        sleep,
                                    )
                                    .await;
                                    if let Err((env, _)) = dispatch_result {
                                        handle_not_found(env, &message_injector).await;
                                    }
                                }
                                .then(move |_| async {
                                    done_tx.trigger();
                                });

                                let (_, write_result) = join(dispatch_task, write_task).await;

                                if let Err(err) = write_result {
                                    break Completion::Failed(err);
                                }
                            }
                            Err(c) => {
                                break c;
                            }
                        },
                        message => {
                            event!(Level::WARN, IGNORING_MESSAGE, ?message);
                        }
                    },
                    Either::Right(Err(err)) => {
                        break Completion::Failed(err);
                    }
                    Either::Right(Ok(SelectorResult::Written)) => {}
                }

                iteration_count += 1;
                if iteration_count % yield_mod == 0 {
                    tokio::task::yield_now().await;
                }
            } else {
                break Completion::StoppedRemotely;
            }
        };

        if let Some(reason) = match &completion {
            Completion::StoppedLocally => Some(CloseReason::new(
                CloseCode::GoingAway,
                "Stopped locally".to_string(),
            )),
            Completion::Failed(ConnectionError::Protocol(e))
                if e.kind() == ProtocolErrorKind::Warp =>
            {
                Some(CloseReason::new(
                    CloseCode::ProtocolError,
                    e.cause().clone().unwrap_or_else(|| "WARP error".into()),
                ))
            }
            _ => None,
        } {
            if let Err(error) = ws_stream.close(Some(reason)).await {
                event!(Level::ERROR, ERROR_ON_CLOSE, ?error);
            }
        }

        match completion {
            Completion::Failed(err) => ConnectionDropped::Failed(err),
            Completion::TimedOut => ConnectionDropped::TimedOut(config.activity_timeout),
            Completion::StoppedRemotely => ConnectionDropped::Failed(ConnectionError::Closed(
                CloseError::new(CloseErrorKind::ClosedRemotely, None),
            )),
            _ => ConnectionDropped::Closed,
        }
    }
}

fn read_envelope(msg: &str) -> Result<Envelope, Completion> {
    Ok(Envelope::try_from(parser::parse_single(msg)?)?)
}

/// Error type indicating a failure to route an incoming message.
#[derive(Debug)]
enum DispatchError {
    BadNodeUri(BadRelativeUri),
    Unresolvable(ResolutionError),
    RoutingProblem(RouterError),
    Dropped(ConnectionDropped),
}

impl Display for DispatchError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchError::BadNodeUri(err) => write!(f, "Invalid relative URI: '{}'", err),
            DispatchError::Unresolvable(err) => {
                write!(f, "Could not resolve a router endpoint: '{}'", err)
            }
            DispatchError::RoutingProblem(err) => {
                write!(f, "Could not find a router endpoint: '{}'", err)
            }
            DispatchError::Dropped(err) => write!(f, "The routing channel was dropped: '{}'", err),
        }
    }
}

impl Error for DispatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            DispatchError::BadNodeUri(err) => Some(err),
            DispatchError::Unresolvable(err) => Some(err),
            DispatchError::RoutingProblem(err) => Some(err),
            _ => None,
        }
    }
}

impl Recoverable for DispatchError {
    fn is_fatal(&self) -> bool {
        match self {
            DispatchError::RoutingProblem(err) => err.is_fatal(),
            DispatchError::Dropped(reason) => !reason.is_recoverable(),
            DispatchError::Unresolvable(e) if e.kind() == ResolutionErrorKind::Unresolvable => {
                false
            }
            _ => true,
        }
    }
}

impl From<BadRelativeUri> for DispatchError {
    fn from(err: BadRelativeUri) -> Self {
        DispatchError::BadNodeUri(err)
    }
}

impl From<ResolutionError> for DispatchError {
    fn from(err: ResolutionError) -> Self {
        DispatchError::Unresolvable(err)
    }
}

impl From<RouterError> for DispatchError {
    fn from(err: RouterError) -> Self {
        DispatchError::RoutingProblem(err)
    }
}

async fn dispatch_envelope<R, F, D>(
    router: &mut R,
    bidirectional_connections: &mut Slab<TaggedSender>,
    resolved: &mut HashMap<RelativePath, Route>,
    mut envelope: Envelope,
    mut retry_strategy: RetryStrategy,
    delay_fn: F,
) -> Result<(), (Envelope, DispatchError)>
where
    R: Router,
    F: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    loop {
        let result =
            try_dispatch_envelope(router, bidirectional_connections, resolved, envelope).await;
        match result {
            Err((env, err)) if !err.is_fatal() => {
                match retry_strategy.next() {
                    Some(Some(dur)) => {
                        delay_fn(dur).await;
                    }
                    None => {
                        break Err((env, err));
                    }
                    _ => {}
                }
                envelope = env;
            }
            Err((env, err)) => {
                break Err((env, err));
            }
            _ => {
                break Ok(());
            }
        }
    }
}

async fn try_dispatch_envelope<R>(
    router: &mut R,
    bidirectional_connections: &mut Slab<TaggedSender>,
    resolved: &mut HashMap<RelativePath, Route>,
    envelope: Envelope,
) -> Result<(), (Envelope, DispatchError)>
where
    R: Router,
{
    if envelope.header.is_response() {
        let mut futures = vec![];

        for (idx, conn) in bidirectional_connections.iter_mut() {
            let envelope = envelope.clone();

            futures.push(async move {
                let result = conn.send_item(envelope).await;
                (idx, result)
            });
        }

        let results = join_all(futures).await;

        for result in results {
            if let (idx, Err(_)) = result {
                bidirectional_connections.remove(idx);
            }
        }

        Ok(())
    } else if let Some(target) = envelope.header.relative_path().as_ref() {
        let Route { sender, .. } = if let Some(route) = resolved.get_mut(target) {
            if route.sender.inner.is_closed() {
                resolved.remove(target);
                insert_new_route(router, resolved, target)
                    .await
                    .map_err(|err| (envelope.clone(), err))?
            } else {
                route
            }
        } else {
            insert_new_route(router, resolved, target)
                .await
                .map_err(|err| (envelope.clone(), err))?
        };
        if let Err(err) = sender.send_item(envelope).await {
            if let Some(Route { on_drop, .. }) = resolved.remove(target) {
                let reason = on_drop
                    .await
                    .map(|reason| (*reason).clone())
                    .unwrap_or(ConnectionDropped::Unknown);
                let (_, env) = err.split();
                Err((env, DispatchError::Dropped(reason)))
            } else {
                unreachable!();
            }
        } else {
            Ok(())
        }
    } else {
        panic!("Authentication envelopes not yet supported.");
    }
}

#[allow(clippy::needless_lifetimes)]
async fn insert_new_route<'a, R>(
    router: &mut R,
    resolved: &'a mut HashMap<RelativePath, Route>,
    target: &RelativePath,
) -> Result<&'a mut Route, DispatchError>
where
    R: Router,
{
    let route = get_route(router, target).await;

    match route {
        Ok(route) => match resolved.entry(target.clone()) {
            Entry::Occupied(_) => unreachable!(),
            Entry::Vacant(entry) => Ok(entry.insert(route)),
        },
        Err(err) => Err(err),
    }
}

async fn get_route<R>(router: &mut R, target: &RelativePath) -> Result<Route, DispatchError>
where
    R: Router,
{
    let target_addr = router
        .lookup(None, RelativeUri::from_str(target.node.as_str())?)
        .await?;
    Ok(router.resolve_sender(target_addr).await?)
}

/// Factory to create and spawn new connection tasks.
pub struct TaskFactory<DelegateRouterFac> {
    request_tx: mpsc::Sender<RemoteRoutingRequest>,
    stop_trigger: trigger::Receiver,
    configuration: RemoteConnectionsConfig,
    delegate_router_fac: DelegateRouterFac,
}

impl<DelegateRouterFac> TaskFactory<DelegateRouterFac> {
    pub fn new(
        request_tx: mpsc::Sender<RemoteRoutingRequest>,
        stop_trigger: trigger::Receiver,
        configuration: RemoteConnectionsConfig,
        delegate_router_fac: DelegateRouterFac,
    ) -> Self {
        TaskFactory {
            request_tx,
            stop_trigger,
            configuration,
            delegate_router_fac,
        }
    }
}

impl<DelegateRouterFac> TaskFactory<DelegateRouterFac>
where
    DelegateRouterFac: RouterFactory + 'static,
{
    pub fn spawn_connection_task<Str, Sp>(
        &self,
        ws_stream: Str,
        tag: RoutingAddr,
        spawner: &Sp,
    ) -> (
        mpsc::Sender<TaggedEnvelope>,
        mpsc::Sender<BidirectionalReceiverRequest>,
    )
    where
        Str: JoinedStreamSink<WsMessage, ConnectionError> + Send + Unpin + 'static,
        Sp: Spawner<BoxFuture<'static, (RoutingAddr, ConnectionDropped)>>,
    {
        let TaskFactory {
            request_tx,
            stop_trigger,
            configuration,
            delegate_router_fac,
        } = self;
        let (msg_tx, msg_rx) = mpsc::channel(configuration.channel_buffer_size.get());
        let (bidirectional_request_tx, bidirectional_request_rx) =
            mpsc::channel(configuration.channel_buffer_size.get());

        let task = ConnectionTask::new(
            tag,
            ws_stream,
            RemoteRouter::new(tag, delegate_router_fac.create_for(tag), request_tx.clone()),
            (msg_tx.clone(), msg_rx),
            bidirectional_request_rx,
            stop_trigger.clone(),
            *configuration,
        );

        spawner.add(
            async move {
                let result = task.run().await;
                (tag, result)
            }
            .boxed(),
        );
        (msg_tx, bidirectional_request_tx)
    }
}

//Get the target path only for link and sync messages (for creating the "not found" response).
fn link_or_sync(env: Envelope) -> Option<RelativePath> {
    match env.header {
        EnvelopeHeader::OutgoingLink(OutgoingHeader::Link(_), path) => Some(path),
        EnvelopeHeader::OutgoingLink(OutgoingHeader::Sync(_), path) => Some(path),
        _ => None,
    }
}

// Dummy origing for not found messages.
const NOT_FOUND_ADDR: RoutingAddr = RoutingAddr::plane(0);

// For a link or sync message that cannot be routed, send back a "not found" message.
async fn handle_not_found(env: Envelope, sender: &mpsc::Sender<TaggedEnvelope>) {
    if let Some(RelativePath { node, lane }) = link_or_sync(env) {
        let not_found = Envelope::node_not_found(node, lane);
        //An error here means the web socket connection has failed and will produce an error
        //the next time it is polled so it is fine to discard this error.
        let _ = sender.send(TaggedEnvelope(NOT_FOUND_ADDR, not_found)).await;
    }
}

// Continue polling the selector but only to write messsages. This ensures that the task cannot
// block whilst waiting to dispatch an incoming message. (Where an imcoming message generates
// one or more outgoing messages on the same socket this can lead to a deadlock).
async fn write_to_socket_only<S, M, T, E>(
    selector: &mut WsStreamSelector<S, M, T, E>,
    done: trigger::Receiver,
    yield_mod: usize,
    iteration_count: &mut usize,
) -> Result<(), E>
where
    M: Stream<Item = T> + Unpin,
    S: Sink<T, Error = E>,
    S: Stream<Item = Result<T, E>> + Unpin,
{
    let write_stream = stream::unfold(
        (selector, iteration_count),
        |(selector, iteration_count)| async {
            let write_result = selector.select_w().await;
            match write_result {
                Some(Ok(_)) => {
                    *iteration_count += 1;
                    if *iteration_count % yield_mod == 0 {
                        tokio::task::yield_now().await;
                    }
                    Some((Ok(()), (selector, iteration_count)))
                }
                Some(Err(e)) => Some((Err(e), (selector, iteration_count))),
                _ => None,
            }
        },
    )
    .take_until(done);

    pin_mut!(write_stream);

    while let Some(result) = write_stream.next().await {
        if result.is_err() {
            return result;
        }
    }
    Ok(())
}
