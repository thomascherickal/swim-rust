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

use crate::routing::{ConnectionDropped, Route, RoutingAddr, TaggedEnvelope};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::net::SocketAddr;
use tokio::sync::mpsc;
use utilities::sync::promise;

/// A combination of host name and port to be used as a key into the routing table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostAndPort(String, u16);

impl HostAndPort {
    pub fn new(host: String, port: u16) -> Self {
        HostAndPort(host, port)
    }
}

impl Display for HostAndPort {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let HostAndPort(host, port) = self;
        write!(f, "{}:{}", host, port)
    }
}

/// Routing table for active routes to remote hosts. An entry in the table contains a channel
/// sender which can send envelopes to the task that manages to route and a promise that will
/// be satisfied when the task stops running.
#[derive(Debug, Default)]
pub struct RoutingTable {
    open_sockets: HashMap<SocketAddr, RoutingAddr>,
    resolved_forward: HashMap<HostAndPort, RoutingAddr>,
    endpoints: HashMap<RoutingAddr, Handle>,
}

impl RoutingTable {
    /// Try to get the routing key in the table for a given host/port combination.
    pub fn try_resolve(&self, target: &HostAndPort) -> Option<RoutingAddr> {
        self.resolved_forward.get(target).copied()
    }

    /// Try to get a routing key in the table for a resolved socket address.
    pub fn get_resolved(&self, target: &SocketAddr) -> Option<RoutingAddr> {
        self.open_sockets.get(target).copied()
    }

    /// Get the entry in the table associated with a routing key, if it exsits.
    pub fn resolve(&self, addr: RoutingAddr) -> Option<Route<mpsc::Sender<TaggedEnvelope>>> {
        self.endpoints
            .get(&addr)
            .map(|h| Route::new(h.tx.clone(), h.drop_rx.clone()))
    }

    /// Insert an entry into the table.
    pub fn insert(
        &mut self,
        addr: RoutingAddr,
        host: Option<HostAndPort>,
        sock_addr: SocketAddr,
        tx: mpsc::Sender<TaggedEnvelope>,
    ) {
        let RoutingTable {
            open_sockets,
            resolved_forward,
            endpoints,
        } = self;
        debug_assert!(!open_sockets.contains_key(&sock_addr));

        open_sockets.insert(sock_addr, addr);
        let mut hosts = HashSet::new();
        if let Some(host) = host {
            resolved_forward.insert(host.clone(), addr);
            hosts.insert(host);
        }

        endpoints.insert(addr, Handle::new(tx, sock_addr, hosts));
    }

    /// Associate another hose/port combination with a socket address that already has an entry in
    /// the table. This will return [`Some`] if and only if there is already an entry for that
    /// address.
    pub fn add_host(&mut self, host: HostAndPort, sock_addr: SocketAddr) -> Option<RoutingAddr> {
        let RoutingTable {
            open_sockets,
            resolved_forward,
            endpoints,
            ..
        } = self;

        if let Some(addr) = open_sockets.get(&sock_addr) {
            debug_assert!(!resolved_forward.contains_key(&host));
            resolved_forward.insert(host.clone(), *addr);
            let handle = endpoints.get_mut(&addr).expect("Inconsistent table.");
            handle.bindings.insert(host);
            Some(*addr)
        } else {
            None
        }
    }

    /// Remove an entry from the table, returning the sender for the promise associated with the
    /// entry (that can then be used to report why the entry is being removed).
    pub fn remove(&mut self, addr: RoutingAddr) -> Option<promise::Sender<ConnectionDropped>> {
        let RoutingTable {
            open_sockets,
            resolved_forward,
            endpoints,
            ..
        } = self;
        if let Some(Handle {
            peer,
            bindings,
            drop_tx,
            ..
        }) = endpoints.remove(&addr)
        {
            open_sockets.remove(&peer);
            bindings.iter().for_each(move |h| {
                resolved_forward.remove(h);
            });
            Some(drop_tx)
        } else {
            None
        }
    }
}

#[derive(Debug)]
struct Handle {
    tx: mpsc::Sender<TaggedEnvelope>,
    drop_tx: promise::Sender<ConnectionDropped>,
    drop_rx: promise::Receiver<ConnectionDropped>,
    peer: SocketAddr,
    bindings: HashSet<HostAndPort>,
}

impl Handle {
    fn new(
        tx: mpsc::Sender<TaggedEnvelope>,
        peer: SocketAddr,
        bindings: HashSet<HostAndPort>,
    ) -> Self {
        let (drop_tx, drop_rx) = promise::promise();
        Handle {
            tx,
            drop_tx,
            drop_rx,
            peer,
            bindings,
        }
    }
}
