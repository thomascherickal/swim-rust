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

use crate::connections::{
    Connection, ConnectionError, ConnectionPool, ConnectionPoolMessage, SwimConnectionFactory,
};
use crate::sink::item::map_err::SenderErrInto;
use crate::sink::item::{ItemSender, ItemSink};
use common::warp::envelope::{Envelope, LaneAddressed};
use common::warp::path::AbsolutePath;
use futures::future::{ready, Ready};
use futures::{Future, Stream};
use std::error::Error;
use std::fmt::{Display, Formatter};
use tokio::sync::mpsc;

#[cfg(test)]
mod tests;

pub trait Router: Send {
    type ConnectionStream: Stream<Item = Envelope> + Send + 'static;
    type ConnectionSink: ItemSender<Envelope, RoutingError> + Send + 'static;
    type GeneralSink: ItemSender<(String, Envelope), RoutingError> + Send + 'static;

    type ConnectionFut: Future<Output = (Self::ConnectionSink, Self::ConnectionStream)> + Send;
    type GeneralFut: Future<Output = Self::GeneralSink> + Send;

    fn connection_for(&mut self, target: &AbsolutePath) -> Self::ConnectionFut;

    fn general_sink(&mut self) -> Self::GeneralFut;
}

pub struct SwimRouter {}

impl SwimRouter {
    fn new(buffer_size: usize) -> SwimRouter {
        let (router_tx, router_rx) = mpsc::channel(buffer_size);

        //TODO add to struct
        // let (connection_request_tx, connection_request_rx) = mpsc::channel(buffer_size);
        let (envelope_tx, envelope_rx) = mpsc::channel(buffer_size);

        //Todo Use this to create request_sink()
        // let (sinks_tx, sinks_rx) = mpsc::channel(buffer_size);

        // Todo Use this to create request_connection()
        // let mut connection_pool =
        //     ConnectionPool::new(buffer_size, router_tx, SwimConnectionFactory {});

        let receive = SwimRouter::receive_messages_from_pool(router_rx);
        let send = SwimRouter::send_envelopes_to_pool(envelope_rx);

        // Todo Add the handlers to the SwimRouter
        let send_handler = tokio::spawn(send);
        let receive_handler = tokio::spawn(receive);

        SwimRouter {}
    }

    // rx receives messages directly from every open connection in the pool
    async fn receive_messages_from_pool(
        mut router_rx: mpsc::Receiver<Result<ConnectionPoolMessage, ConnectionError>>,
    ) {
        loop {
            let pool_message = router_rx.recv().await.unwrap().unwrap();
            let ConnectionPoolMessage { host, message } = pool_message;

            //TODO this needs to be implemented
            let mut sink = SwimRouter::request_sink(host);
            sink.send_item(text);

            //TODO parse the message to envelope. This should be moved in the subtasks.

            // let lane_addressed = LaneAddressed {
            //     node_uri: String::from("node_uri"),
            //     lane_uri: String::from("lane_uri"),
            //     body: None,
            // };
            // let envelope = Envelope::EventMessage(lane_addressed);
            //
            // sink.send_item(envelope);
        }
    }

    async fn send_envelopes_to_pool(mut envelope_rx: mpsc::Receiver<Envelope>) {
        //Todo wrap message and host into one struct
        loop {
            let envelope = envelope_rx.recv().await.unwrap();
            //TODO Parse the envelope to obtain host
            let host = String::from("foo");
            let mut connection = SwimRouter::request_connection(host);

            connection.send_message(&message);
        }
    }
}

impl Router for SwimRouter {
    type ConnectionStream = mpsc::Receiver<Envelope>;
    type ConnectionSink = SenderErrInto<mpsc::Sender<Envelope>, RoutingError>;
    type GeneralSink = SenderErrInto<mpsc::Sender<(String, Envelope)>, RoutingError>;
    type ConnectionFut = Ready<(Self::ConnectionSink, Self::ConnectionStream)>;
    type GeneralFut = Ready<Self::GeneralSink>;

    fn connection_for(&mut self, target: &AbsolutePath) -> Self::ConnectionFut {
        // Todo remove unwrap
        let host_url = url::Url::parse(&target.host).unwrap();

        //Todo this should have two different channels
        let (envelope_tx, envelope_rx) = mpsc::channel::<Envelope>(5);

        let envelope_tx = envelope_tx.map_err_into();

        ready((envelope_tx, envelope_rx))
    }

    fn general_sink(&mut self) -> Self::GeneralFut {
        //Todo
        unimplemented!()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RoutingError {
    RouterDropped,
}

impl Display for RoutingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RoutingError::RouterDropped => write!(f, "Router was dropped."),
        }
    }
}

impl Error for RoutingError {}

impl<T> From<mpsc::error::SendError<T>> for RoutingError {
    fn from(_: mpsc::error::SendError<T>) -> Self {
        //TODO add impl
        unimplemented!()
    }
}
