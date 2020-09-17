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

use futures::future::BoxFuture;
use std::fmt::{Display, Formatter};
use swim_common::routing::RoutingError;
use swim_common::sink::item::ItemSender;
use swim_common::warp::envelope::{Envelope, OutgoingLinkMessage};

#[cfg(test)]
mod tests;

/// A key into the server routing table specifying an endpoint to which [`Envelope`]s can be sent.
/// This is deliberately non-descriptive to allow it to be [`Copy`] and so very cheap to use as a
/// key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Location {
    /// Indicates that envelopes will be routed to a remote host.
    RemoteEndpoint(u32),
    /// Indicates that envelopes will be routed to another agent on this host.
    Local(u32),
}

/// An opaque routing address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoutingAddr(Location);

impl RoutingAddr {
    pub const fn remote(id: u32) -> Self {
        RoutingAddr(Location::RemoteEndpoint(id))
    }

    pub const fn local(id: u32) -> Self {
        RoutingAddr(Location::Local(id))
    }

    pub fn is_local(&self) -> bool {
        matches!(self, RoutingAddr(Location::Local(_)))
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, RoutingAddr(Location::RemoteEndpoint(_)))
    }
}

impl Display for RoutingAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RoutingAddr(Location::RemoteEndpoint(id)) => write!(f, "Remote Endpoint ({:X})", id),
            RoutingAddr(Location::Local(id)) => write!(f, "Local consumer ({:X})", id),
        }
    }
}

/// An [`Envelope`] tagged with the key of the endpoint into routing table from which it originated.
#[derive(Debug, Clone, PartialEq)]
pub struct TaggedEnvelope(pub RoutingAddr, pub Envelope);

/// An [`OutgoingLinkMessage`] tagged with the key of the endpoint into routing table from which it
/// originated.
#[derive(Debug, Clone, PartialEq)]
pub struct TaggedClientEnvelope(pub RoutingAddr, pub OutgoingLinkMessage);

impl TaggedClientEnvelope {
    pub fn lane(&self) -> &str {
        self.1.path.lane.as_str()
    }
}

/// Interface for interacting with the server [`Envelope`] router.
pub trait ServerRouter: Send + Sync {
    type Sender: ItemSender<Envelope, RoutingError> + Send + 'static;

    fn get_sender(&mut self, addr: RoutingAddr) -> BoxFuture<Result<Self::Sender, RoutingError>>;
}
