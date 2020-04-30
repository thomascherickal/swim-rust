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

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::pin::Pin;

use futures::future::Ready;
use futures::stream;
use futures::task::{Context, Poll};
use futures::{Future, Stream};
use tokio::stream::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::SendError;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use common::request::request_future::{RequestError, RequestFuture, Sequenced};
use common::sink::item::map_err::SenderErrInto;
use common::sink::item::ItemSender;
use common::warp::envelope::Envelope;
use common::warp::path::AbsolutePath;

use crate::connections::factory::tungstenite::TungsteniteWsFactory;
use crate::connections::{ConnectionError, ConnectionPool, ConnectionSender};
use crate::router::command::{CommandSender, CommandTask};
use crate::router::incoming::{IncomingSubscriberReqSender, IncomingTask, IncomingTaskReqSender};
use crate::router::outgoing::{OutgoingTask, OutgoingTaskReqSender};

use crate::configuration::router::RouterParams;
use tokio_tungstenite::tungstenite::protocol::Message;

pub mod command;
pub mod incoming;
pub mod outgoing;

#[cfg(test)]
mod tests;

pub trait Router: Send {
    type ConnectionStream: Stream<Item = RouterEvent> + Send + 'static;
    type ConnectionSink: ItemSender<Envelope, RoutingError> + Send + Clone + Sync + 'static;
    type GeneralSink: ItemSender<(String, Envelope), RoutingError> + Send + 'static;

    type ConnectionFut: Future<Output = Result<(Self::ConnectionSink, Self::ConnectionStream), RequestError>>
        + Send;
    type GeneralFut: Future<Output = Self::GeneralSink> + Send;

    fn connection_for(&mut self, target: &AbsolutePath) -> Self::ConnectionFut;

    fn general_sink(&mut self) -> Self::GeneralFut;
}

pub type CloseRequestSender = mpsc::Sender<()>;
pub type CloseRequestReceiver = mpsc::Receiver<()>;

pub struct SwimRouter {
    configuration: RouterParams,
    outgoing_task_request_tx: OutgoingTaskReqSender,
    incoming_subscribe_request_tx: IncomingSubscriberReqSender,
    connection_request_handler: JoinHandle<Result<(), RoutingError>>,
    connections_task_close_request_tx: CloseRequestSender,
    outgoing_task_handler: JoinHandle<Result<(), RoutingError>>,
    outgoing_task_close_request_tx: CloseRequestSender,
    incoming_task_handler: JoinHandle<Result<(), RoutingError>>,
    incoming_task_close_request_tx: CloseRequestSender,
    command_tx: CommandSender,
    command_task_handler: JoinHandle<Result<(), RoutingError>>,
    command_task_close_request_tx: CloseRequestSender,
}

impl SwimRouter {
    pub async fn new(configuration: RouterParams) -> SwimRouter {
        let buffer_size = configuration.buffer_size().get();

        let connection_pool =
            ConnectionPool::new(buffer_size, TungsteniteWsFactory::new(buffer_size).await);

        let (
            incoming_task,
            incoming_task_request_tx,
            incoming_subscribe_request_tx,
            incoming_task_close_request_tx,
        ) = IncomingTask::new(buffer_size);

        let (connection_request_task, connection_request_tx, connections_task_close_request_tx) =
            ConnectionRequestTask::new(
                connection_pool,
                incoming_task_request_tx.clone(),
                configuration,
            );

        let (outgoing_task, outgoing_task_request_tx, outgoing_task_close_request_tx) =
            OutgoingTask::new(
                connection_request_tx.clone(),
                incoming_task_request_tx,
                configuration,
            );

        let (command_task, command_tx, command_task_close_request_tx) =
            CommandTask::new(connection_request_tx, configuration);

        let connection_request_handler = tokio::spawn(connection_request_task.run());
        let outgoing_task_handler = tokio::spawn(outgoing_task.run());
        let incoming_task_handler = tokio::spawn(incoming_task.run());
        let command_task_handler = tokio::spawn(command_task.run());

        SwimRouter {
            configuration,
            outgoing_task_request_tx,
            incoming_subscribe_request_tx,
            connection_request_handler,
            connections_task_close_request_tx,
            outgoing_task_handler,
            outgoing_task_close_request_tx,
            incoming_task_handler,
            incoming_task_close_request_tx,
            command_tx,
            command_task_handler,
            command_task_close_request_tx,
        }
    }

    pub async fn close(mut self) {
        self.outgoing_task_close_request_tx.send(()).await.unwrap();
        let _ = self.outgoing_task_handler.await.unwrap();

        self.command_task_close_request_tx.send(()).await.unwrap();
        let _ = self.command_task_handler.await.unwrap();

        self.connections_task_close_request_tx
            .send(())
            .await
            .unwrap();
        let _ = self.connection_request_handler.await.unwrap();

        self.incoming_task_close_request_tx.send(()).await.unwrap();
        let _ = self.incoming_task_handler.await.unwrap();
    }

    pub fn send_command(
        &mut self,
        target: &AbsolutePath,
        message: String,
    ) -> Result<(), RoutingError> {
        let AbsolutePath { host, node, lane } = target;

        self.command_tx
            .try_send((
                AbsolutePath {
                    host: host.clone(),
                    node: node.clone(),
                    lane: lane.clone(),
                },
                message,
            ))
            .map_err(|_| RoutingError::ConnectionError)?;

        Ok(())
    }
}

type Host = String;

pub enum ConnectionResponse {
    Success((Host, mpsc::Receiver<Message>)),
    Failure(Host),
}

pub type ConnReqSender = mpsc::Sender<(
    Host,
    oneshot::Sender<Result<ConnectionSender, RoutingError>>,
    bool, // Whether or not to recreate the connection
)>;
type ConnReqReceiver = mpsc::Receiver<(
    Host,
    oneshot::Sender<Result<ConnectionSender, RoutingError>>,
    bool, // Whether or not to recreate the connection
)>;

struct ConnectionRequestTask {
    connection_pool: ConnectionPool,
    connection_request_rx: ConnReqReceiver,
    close_request_rx: CloseRequestReceiver,
    incoming_task_request_tx: IncomingTaskReqSender,
}

impl ConnectionRequestTask {
    fn new(
        connection_pool: ConnectionPool,
        incoming_task_request_tx: IncomingTaskReqSender,
        config: RouterParams,
    ) -> (Self, ConnReqSender, CloseRequestSender) {
        let (connection_request_tx, connection_request_rx) =
            mpsc::channel(config.buffer_size().get());
        let (close_request_tx, close_request_rx) = mpsc::channel(config.buffer_size().get());

        (
            ConnectionRequestTask {
                connection_pool,
                connection_request_rx,
                close_request_rx,
                incoming_task_request_tx,
            },
            connection_request_tx,
            close_request_tx,
        )
    }

    async fn run(self) -> Result<(), RoutingError> {
        let ConnectionRequestTask {
            mut connection_pool,
            connection_request_rx,
            close_request_rx,
            mut incoming_task_request_tx,
        } = self;

        let mut rx = combine_router_streams(connection_request_rx, close_request_rx);

        while let ConnectionRequestType::NewConnection {
            host,
            connection_tx,
            recreate,
        } = rx.next().await.ok_or(RoutingError::ConnectionError)?
        {
            let host_url = url::Url::parse(&host).map_err(|_| RoutingError::ConnectionError)?;

            let connection = if recreate {
                connection_pool
                    .recreate_connection(host_url.clone())
                    .map_err(|_| RoutingError::ConnectionError)?
                    .await
            } else {
                connection_pool
                    .request_connection(host_url.clone())
                    .map_err(|_| RoutingError::ConnectionError)?
                    .await
            }
            .map_err(|_| RoutingError::ConnectionError)?;

            match connection {
                Ok((connection_sender, connection_receiver)) => {
                    connection_tx
                        .send(Ok(connection_sender))
                        .map_err(|_| RoutingError::ConnectionError)?;

                    if let Some(receiver) = connection_receiver {
                        incoming_task_request_tx
                            .send(ConnectionResponse::Success((host, receiver)))
                            .await
                            .map_err(|_| RoutingError::ConnectionError)?
                    }
                }

                Err(e) => {
                    // Need to return an error to the envelope routing task so that it can cancel
                    // the active request and not attempt it again the request again. Some errors
                    // are transient and they may resolve themselves after waiting
                    match e {
                        // Transient error that may be recoverable
                        ConnectionError::Transient => {
                            let _ = connection_tx.send(Err(RoutingError::Transient));
                        }
                        // Permanent, unrecoverable error
                        _ => {
                            let _ = connection_tx.send(Err(RoutingError::ConnectionError));
                            let _ = incoming_task_request_tx
                                .send(ConnectionResponse::Failure(host))
                                .await;
                        }
                    }
                }
            }
        }
        connection_pool.close().await;
        Ok(())
    }
}

enum ConnectionRequestType {
    NewConnection {
        host: Host,
        connection_tx: oneshot::Sender<Result<ConnectionSender, RoutingError>>,
        recreate: bool,
    },
    Close,
}

fn combine_router_streams(
    connection_requests_rx: ConnReqReceiver,
    close_requests_rx: CloseRequestReceiver,
) -> impl stream::Stream<Item = ConnectionRequestType> + Send + 'static {
    let connection_requests =
        connection_requests_rx.map(|r| ConnectionRequestType::NewConnection {
            host: r.0,
            connection_tx: r.1,
            recreate: r.2,
        });
    let close_request = close_requests_rx.map(|_| ConnectionRequestType::Close);
    stream::select(connection_requests, close_request)
}

#[derive(Debug, Clone, PartialEq)]
pub enum RouterEvent {
    Envelope(Envelope),
    ConnectionClosed,
    Unreachable,
    Stopping,
}

type SwimRouterConnection = (
    SenderErrInto<mpsc::Sender<Envelope>, RoutingError>,
    mpsc::Receiver<RouterEvent>,
);

pub struct ConnectionFuture {
    outgoing_rx: oneshot::Receiver<mpsc::Sender<Envelope>>,
    incoming_rx: Option<mpsc::Receiver<RouterEvent>>,
}

impl ConnectionFuture {
    fn new(
        outgoing_rx: oneshot::Receiver<mpsc::Sender<Envelope>>,
        incoming_rx: mpsc::Receiver<RouterEvent>,
    ) -> ConnectionFuture {
        ConnectionFuture {
            outgoing_rx,
            incoming_rx: Some(incoming_rx),
        }
    }
}

impl Future for ConnectionFuture {
    type Output = Result<SwimRouterConnection, RoutingError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let ConnectionFuture {
            outgoing_rx,
            incoming_rx,
        } = &mut self.get_mut();

        oneshot::Receiver::poll(Pin::new(outgoing_rx), cx).map(|r| match r {
            Ok(outgoing_rx) => Ok((
                outgoing_rx.map_err_into::<RoutingError>(),
                incoming_rx.take().ok_or(RoutingError::ConnectionError)?,
            )),
            _ => Err(RoutingError::ConnectionError),
        })
    }
}

type SwimRouterConnectionFut = Sequenced<
    Sequenced<RequestFuture<OutgoingRequest>, RequestFuture<(Host, IncomingRequest)>>,
    ConnectionFuture,
>;

pub fn connect(
    outgoing_task_request_tx: OutgoingTaskReqSender,
    incoming_subscriber_request_tx: IncomingSubscriberReqSender,
    target: AbsolutePath,
    buffer_size: usize,
) -> SwimRouterConnectionFut {
    let (outgoing_tx, outgoing_rx) = oneshot::channel();
    let (incoming_tx, incoming_rx) = mpsc::channel(buffer_size);

    let AbsolutePath { host, node, lane } = target;

    let outgoing_request = RequestFuture::new(
        outgoing_task_request_tx,
        OutgoingRequest::new(host.clone(), outgoing_tx),
    );
    let incoming_request = RequestFuture::new(
        incoming_subscriber_request_tx,
        (host, IncomingRequest::new(node, lane, incoming_tx)),
    );

    let request_futures = Sequenced::new(outgoing_request, incoming_request);

    Sequenced::new(
        request_futures,
        ConnectionFuture::new(outgoing_rx, incoming_rx),
    )
}

impl Router for SwimRouter {
    type ConnectionStream = mpsc::Receiver<RouterEvent>;
    type ConnectionSink = SenderErrInto<mpsc::Sender<Envelope>, RoutingError>;
    type GeneralSink = SenderErrInto<mpsc::Sender<(String, Envelope)>, RoutingError>;
    type ConnectionFut = SwimRouterConnectionFut;
    type GeneralFut = Ready<Self::GeneralSink>;

    fn connection_for(&mut self, target: &AbsolutePath) -> Self::ConnectionFut {
        connect(
            self.outgoing_task_request_tx.clone(),
            self.incoming_subscribe_request_tx.clone(),
            target.clone(),
            self.configuration.buffer_size().get(),
        )
    }

    fn general_sink(&mut self) -> Self::GeneralFut {
        //Todo
        unimplemented!()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RoutingError {
    Transient,
    RouterDropped,
    ConnectionError,
}

impl Display for RoutingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RoutingError::RouterDropped => write!(f, "Router was dropped."),
            RoutingError::ConnectionError => write!(f, "Connection error."),
            RoutingError::Transient => write!(f, "Transient error"),
        }
    }
}

impl Error for RoutingError {}

impl<T> From<SendError<T>> for RoutingError {
    fn from(_: SendError<T>) -> Self {
        RoutingError::RouterDropped
    }
}

impl From<RoutingError> for RequestError {
    fn from(_: RoutingError) -> Self {
        RequestError {}
    }
}
