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

mod second;
//
// use crate::errors::Error;
// use crate::handshake::io::BufferedIo;
// use crate::protocol::WebSocketConfig;
// use crate::{ProtocolRegistry, Request, WebSocketStream};
// use bytes::BytesMut;
// use futures::future::BoxFuture;
// use http::StatusCode;
// use httparse::Header;
// use tokio::io::{AsyncRead, AsyncWrite};
// use tokio_native_tls::TlsConnector;
//
// pub trait HandshakeInterceptor {
//     fn intercept(request: &Request) -> BoxFuture<ServerResponse>;
// }
//
// pub enum ServerResponse {
//     Accept(HandshakeResponse),
//     Reject(Rejection),
// }
//
// pub struct Rejection {
//     status: StatusCode,
// }
//
// pub struct HandshakeResponse<'s> {
//     subprotocol: &'static str,
//     headers: &'s [Header<'s>],
// }
//
// pub async fn exec_server_handshake<S>(
//     stream: &mut S,
//     _config: &WebSocketConfig,
//     _connector: Option<TlsConnector>,
//     subprotocols: ProtocolRegistry,
//     buf: &mut BytesMut,
// ) -> Result<HandshakeResult<E::Extension>, Error>
// where
//     S: AsyncRead + AsyncWrite + Unpin,
// {
//     let mut read_buffer = BytesMut::new();
//     let machine = HandshakeMachine::new(stream, Vec::new(), Vec::new(), &mut read_buffer);
//     machine.exec().await
// }
//
// struct HandshakeMachine<'s, S> {
//     buffered: BufferedIo<'s, S>,
//     subprotocols: Vec<&'static str>,
//     extensions: Vec<&'static str>,
// }
//
// impl<'s, S> HandshakeMachine<'s, S>
// where
//     S: AsyncRead + AsyncWrite + Unpin,
// {
//     pub fn new(
//         socket: &'s mut S,
//         subprotocols: Vec<&'static str>,
//         extensions: Vec<&'static str>,
//         read_buffer: &'s mut BytesMut,
//     ) -> HandshakeMachine<'s, S> {
//         HandshakeMachine {
//             buffered: BufferedIo::new(socket, read_buffer),
//             subprotocols,
//             extensions,
//         }
//     }
//
//     pub async fn exec(self) -> Result<(), Error> {
//         unimplemented!()
//     }
// }
