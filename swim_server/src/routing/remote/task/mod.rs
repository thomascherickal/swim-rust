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

#[cfg(test)]
mod tests;

use crate::routing::error::{ConnectionError, ResolutionError, RouterError};
use crate::routing::remote::config::ConnectionConfig;
use crate::routing::remote::router::RemoteRouter;
use crate::routing::remote::RoutingRequest;
use crate::routing::ws::{CloseReason, JoinedStreamSink, SelectorResult, WsStreamSelector};
use crate::routing::{
    ConnectionDropped, Route, RoutingAddr, ServerRouter, ServerRouterFactory, TaggedEnvelope,
};
use futures::future::BoxFuture;
use futures::{select_biased, FutureExt, StreamExt};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::str::FromStr;
use std::time::Duration;
use swim_common::model::parser::{self, ParseFailure};
use swim_common::warp::envelope::{Envelope, EnvelopeParseErr};
use swim_common::warp::path::RelativePath;
use swim_common::ws::protocol::WsMessage;
use tokio::sync::mpsc;
use tokio::time::{sleep, Instant};
use utilities::errors::Recoverable;
use utilities::future::retryable::strategy::RetryStrategy;
use utilities::sync::trigger;
use utilities::task::Spawner;
use utilities::uri::{BadRelativeUri, RelativeUri};

/// A task that manages reading from and writing to a web-sockets channel.
pub struct ConnectionTask<Str, Router> {
    ws_stream: Str,
    messages: mpsc::Receiver<TaggedEnvelope>,
    router: Router,
    stop_signal: trigger::Receiver,
    activity_timeout: Duration,
    retry_strategy: RetryStrategy,
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
        Completion::Failed(ConnectionError::Warp(err.to_string()))
    }
}

impl From<EnvelopeParseErr> for Completion {
    fn from(err: EnvelopeParseErr) -> Self {
        Completion::Failed(ConnectionError::Warp(err.to_string()))
    }
}

impl<Str, Router> ConnectionTask<Str, Router>
where
    Str: JoinedStreamSink<WsMessage, ConnectionError> + Unpin,
    Router: ServerRouter,
{
    /// Create a new task.
    ///
    /// #Arguments
    ///
    /// * `ws_stream` - The joined sink/stream that implements the web sockets protocol.
    /// * `router` - Router to route incoming messages to the appropriate destination.
    /// * `messages`- Stream of messages to be sent into the sink.
    /// * `stop_signal` - Signals to the task that it should stop.
    /// * `activity_timeout` - If the task neither sends nor receives a message within this period
    /// it will stop itself.
    /// * `retry_strategy` - Retry strategy when attempting to route incoming messages.
    pub fn new(
        ws_stream: Str,
        router: Router,
        messages: mpsc::Receiver<TaggedEnvelope>,
        stop_signal: trigger::Receiver,
        activity_timeout: Duration,
        retry_strategy: RetryStrategy,
    ) -> Self {
        assert!(activity_timeout > ZERO);
        ConnectionTask {
            ws_stream,
            messages,
            router,
            stop_signal,
            activity_timeout,
            retry_strategy,
        }
    }

    pub async fn run(self) -> ConnectionDropped {
        let ConnectionTask {
            mut ws_stream,
            messages,
            mut router,
            stop_signal,
            activity_timeout,
            retry_strategy,
        } = self;
        let outgoing_payloads = messages
            .map(|TaggedEnvelope(_, envelope)| WsMessage::Text(envelope.into_value().to_string()));

        let selector = WsStreamSelector::new(&mut ws_stream, outgoing_payloads);

        let mut stop_fused = stop_signal.fuse();
        let mut select_stream = selector.fuse();
        let mut timeout = sleep(activity_timeout);

        let mut resolved: HashMap<RelativePath, Route> = HashMap::new();

        let completion = loop {
            timeout.reset(
                Instant::now()
                    .checked_add(activity_timeout)
                    .expect("Timer overflow."),
            );
            let next: Option<Result<SelectorResult<WsMessage>, ConnectionError>> = select_biased! {
                _ = stop_fused => {
                    break Completion::StoppedLocally;
                },
                _ = (&mut timeout).fuse() => {
                    break Completion::TimedOut;
                }
                event = select_stream.next() => event,
            };

            if let Some(event) = next {
                match event {
                    Ok(SelectorResult::Read(msg)) => match msg {
                        WsMessage::Text(msg) => match read_envelope(&msg) {
                            Ok(envelope) => {
                                if let Err(_err) = dispatch_envelope(
                                    &mut router,
                                    &mut resolved,
                                    envelope,
                                    retry_strategy,
                                    sleep,
                                )
                                .await
                                {
                                    //TODO Log error.
                                }
                            }
                            Err(c) => {
                                break c;
                            }
                        },
                        _ => {}
                    },
                    Err(err) => {
                        break Completion::Failed(err);
                    }
                    _ => {}
                }
            } else {
                break Completion::StoppedRemotely;
            }
        };

        if let Some(reason) = match &completion {
            Completion::StoppedLocally => Some(CloseReason::GoingAway),
            Completion::Failed(ConnectionError::Warp(err)) => {
                Some(CloseReason::ProtocolError(err.clone()))
            }
            _ => None,
        } {
            if let Err(_err) = ws_stream.close(Some(reason)).await {
                //TODO Log close error.
            }
        }

        match completion {
            Completion::Failed(err) => ConnectionDropped::Failed(err),
            Completion::TimedOut => ConnectionDropped::TimedOut(activity_timeout),
            Completion::StoppedRemotely => {
                ConnectionDropped::Failed(ConnectionError::ClosedRemotely)
            }
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
            DispatchError::Unresolvable(ResolutionError::Unresolvable(_)) => false,
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

async fn dispatch_envelope<Router, F, D>(
    router: &mut Router,
    resolved: &mut HashMap<RelativePath, Route>,
    mut envelope: Envelope,
    mut retry_strategy: RetryStrategy,
    delay_fn: F,
) -> Result<(), DispatchError>
where
    Router: ServerRouter,
    F: Fn(Duration) -> D,
    D: Future<Output = ()>,
{
    loop {
        let result = try_dispatch_envelope(router, resolved, envelope).await;
        match result {
            Err((env, err)) if !err.is_fatal() => {
                match retry_strategy.next() {
                    Some(Some(dur)) => {
                        delay_fn(dur).await;
                    }
                    None => {
                        break Err(err);
                    }
                    _ => {}
                }
                envelope = env;
            }
            Err((_, err)) => {
                break Err(err);
            }
            _ => {
                break Ok(());
            }
        }
    }
}

async fn try_dispatch_envelope<Router>(
    router: &mut Router,
    resolved: &mut HashMap<RelativePath, Route>,
    envelope: Envelope,
) -> Result<(), (Envelope, DispatchError)>
where
    Router: ServerRouter,
{
    if let Some(target) = envelope.header.relative_path().as_ref() {
        let Route { sender, .. } = if let Some(route) = resolved.get_mut(target) {
            route
        } else {
            let route = get_route(router, target).await;
            match route {
                Ok(route) => match resolved.entry(target.clone()) {
                    Entry::Occupied(_) => unreachable!(),
                    Entry::Vacant(entry) => entry.insert(route),
                },
                Err(err) => {
                    return Err((envelope, err));
                }
            }
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

async fn get_route<Router>(
    router: &mut Router,
    target: &RelativePath,
) -> Result<Route, DispatchError>
where
    Router: ServerRouter,
{
    let target_addr = router
        .lookup(None, RelativeUri::from_str(&target.node.as_str())?)
        .await?;
    Ok(router.resolve_sender(target_addr).await?)
}

/// Factory to create and spawn new connection tasks.
pub struct TaskFactory<RouterFac> {
    request_tx: mpsc::Sender<RoutingRequest>,
    stop_trigger: trigger::Receiver,
    configuration: ConnectionConfig,
    delegate_router: RouterFac,
}

impl<RouterFac> TaskFactory<RouterFac> {
    pub fn new(
        request_tx: mpsc::Sender<RoutingRequest>,
        stop_trigger: trigger::Receiver,
        configuration: ConnectionConfig,
        delegate_router: RouterFac,
    ) -> Self {
        TaskFactory {
            request_tx,
            stop_trigger,
            configuration,
            delegate_router,
        }
    }
}
impl<RouterFac> TaskFactory<RouterFac>
where
    RouterFac: ServerRouterFactory + 'static,
{
    pub fn spawn_connection_task<Str, Sp>(
        &self,
        ws_stream: Str,
        tag: RoutingAddr,
        spawner: &Sp,
    ) -> mpsc::Sender<TaggedEnvelope>
    where
        Str: JoinedStreamSink<WsMessage, ConnectionError> + Send + Unpin + 'static,
        Sp: Spawner<BoxFuture<'static, (RoutingAddr, ConnectionDropped)>>,
    {
        let TaskFactory {
            request_tx,
            stop_trigger,
            configuration,
            delegate_router,
        } = self;
        let (msg_tx, msg_rx) = mpsc::channel(configuration.channel_buffer_size.get());
        let task = ConnectionTask::new(
            ws_stream,
            RemoteRouter::new(tag, delegate_router.create_for(tag), request_tx.clone()),
            msg_rx,
            stop_trigger.clone(),
            configuration.activity_timeout,
            configuration.connection_retries,
        );

        spawner.add(
            async move {
                let result = task.run().await;
                (tag, result)
            }
            .boxed(),
        );
        msg_tx
    }
}
