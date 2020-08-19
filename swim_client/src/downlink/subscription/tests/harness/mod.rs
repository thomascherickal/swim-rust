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

use crate::router::{Router, RouterEvent};
use futures::future::{ready, Ready};
use futures::stream::{pending, Pending};
use swim_common::request::request_future::RequestError;
use swim_common::routing::RoutingError;
use swim_common::sink::item::drop_all::{drop_all, DropAll};
use swim_common::warp::envelope::Envelope;
use swim_common::warp::path::AbsolutePath;

/// A mock [`Router`] where connections produce no data and all outputs are silently dropped.
pub struct StubRouter {}

impl Router for StubRouter {
    type ConnectionStream = Pending<RouterEvent>;
    type ConnectionSink = DropAll<Envelope, RoutingError>;
    type GeneralSink = DropAll<(url::Url, Envelope), RoutingError>;
    type ConnectionFut =
        Ready<Result<(Self::ConnectionSink, Self::ConnectionStream), RequestError>>;

    fn connection_for(&mut self, _target: &AbsolutePath) -> Self::ConnectionFut {
        ready(Ok((drop_all(), pending())))
    }

    fn general_sink(&mut self) -> Self::GeneralSink {
        drop_all()
    }
}

impl StubRouter {
    pub fn new() -> Self {
        StubRouter {}
    }
}
