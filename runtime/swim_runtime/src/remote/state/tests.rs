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

use crate::error::{ConnectionError, IoError};
use crate::remote::config::RemoteConnectionsConfig;
use crate::remote::pending::PendingRequest;
use crate::remote::state::{
    DeferredResult, Event, RemoteConnectionChannels, RemoteConnections, RemoteTasksState, State,
};
use crate::remote::table::{BidirectionalRegistrator, SchemeHostPort};
use crate::remote::test_fixture::connections::{FakeConnections, FakeListener, FakeWebsockets};
use crate::remote::test_fixture::LocalRoutes;
use crate::remote::{ConnectionDropped, Scheme, SchemeSocketAddr};
use crate::routing::{CloseSender, RoutingAddr, TaggedSender};
use futures::future::BoxFuture;
use futures::io::ErrorKind;
use ratchet::{NoExt, Role};
use ratchet_fixture::duplex::websocket_for;
use std::collections::HashMap;
use std::io;
use std::time::Duration;
use swim_async_runtime::time::timeout::timeout;
use swim_utilities::algebra::non_zero_usize;
use swim_utilities::future::open_ended::OpenEndedFutures;
use swim_utilities::future::request::Request;
use swim_utilities::future::retryable::RetryStrategy;
use swim_utilities::trigger::promise;
use tokio::io::{duplex, DuplexStream};
use tokio::sync::{mpsc, oneshot};

type TestSpawner = OpenEndedFutures<BoxFuture<'static, (RoutingAddr, ConnectionDropped)>>;
type TestConnections<'a> =
    RemoteConnections<'a, FakeConnections, FakeWebsockets, TestSpawner, LocalRoutes>;

struct TestFixture<'a> {
    connections: TestConnections<'a>,
    fake_connections: FakeConnections,
    local: LocalRoutes,
    stop_trigger: CloseSender,
}

fn make_state(
    addr: RoutingAddr,
    ws: &FakeWebsockets,
    incoming: mpsc::Receiver<io::Result<(DuplexStream, SchemeSocketAddr)>>,
) -> TestFixture<'_> {
    let buffer_size = non_zero_usize!(8);

    let config = RemoteConnectionsConfig {
        router_buffer_size: buffer_size,
        channel_buffer_size: buffer_size,
        activity_timeout: Duration::from_secs(30),
        write_timeout: Duration::from_secs(20),
        connection_retries: RetryStrategy::none(),
        yield_after: non_zero_usize!(256),
    };

    let fake_connections = FakeConnections::new(HashMap::new(), HashMap::new(), None, 0);
    let router = LocalRoutes::new(addr);

    let (stop_tx, stop_rx) = promise::promise();
    let (remote_tx, remote_rx) = mpsc::channel(8);

    let connections = RemoteConnections::new(
        ws,
        config,
        OpenEndedFutures::new(),
        fake_connections.clone(),
        Some(FakeListener::new(incoming)),
        router.clone(),
        RemoteConnectionChannels {
            request_tx: remote_tx,
            request_rx: remote_rx,
            stop_trigger: stop_rx,
        },
    );

    TestFixture {
        connections,
        fake_connections,
        local: router,
        stop_trigger: stop_tx,
    }
}

#[tokio::test]
async fn connections_state_stop_when_idle() {
    let addr = RoutingAddr::remote(45);
    let (_incoming_tx, incoming_rx) = mpsc::channel(8);
    let ws = FakeWebsockets;
    let TestFixture {
        mut connections,
        fake_connections: _fake_connections,
        local: _local,
        stop_trigger: _stop_trigger,
    } = make_state(addr, &ws, incoming_rx);

    assert_eq!(connections.state, State::Running);
    connections.stop();
    assert_eq!(connections.state, State::ClosingConnections);

    assert!(matches!(
        timeout(Duration::from_secs(5), connections.select_next()).await,
        Ok(None)
    ));
}

#[test]
fn connections_state_next_addr() {
    let addr = RoutingAddr::remote(45);
    let (_incoming_tx, incoming_rx) = mpsc::channel(8);
    let ws = FakeWebsockets;
    let TestFixture {
        mut connections,
        fake_connections: _fake_connections,
        local: _local,
        stop_trigger: _stop_trigger,
    } = make_state(addr, &ws, incoming_rx);

    let addr1 = connections.next_address();
    let addr2 = connections.next_address();
    assert_ne!(addr1, addr2);
}

fn sock_addr() -> SchemeSocketAddr {
    SchemeSocketAddr::new(Scheme::Ws, "192.168.0.1:80".parse().unwrap())
}

#[tokio::test]
async fn connections_state_spawn_task() {
    let addr = RoutingAddr::remote(45);
    let (_incoming_tx, incoming_rx) = mpsc::channel(8);
    let ws = FakeWebsockets;
    let TestFixture {
        mut connections,
        fake_connections: _fake_connections,
        local: _local,
        stop_trigger: _stop_trigger,
    } = make_state(addr, &ws, incoming_rx);

    let sa = sock_addr();

    let (web_sock, rx) = websocket_for(Role::Client, NoExt).await;
    drop(rx);
    let host = SchemeHostPort::new(Scheme::Ws, "my_host".to_string(), 80);

    let (req_tx, req_rx) = oneshot::channel();

    connections.pending.add(
        host.clone(),
        PendingRequest::Resolution(Request::new(req_tx)),
    );

    connections.spawn_task(sa, web_sock, Some(host.clone()));

    assert_eq!(connections.spawner.len(), 1);
    let table = &connections.table;
    let res_addr = table.try_resolve(&host);
    assert!(res_addr.is_some());
    let task_addr = res_addr.unwrap();
    assert!(table.resolve(task_addr).is_some());

    let result = timeout(Duration::from_secs(5), req_rx).await;
    assert!(matches!(result, Ok(Ok(Ok(a))) if a == task_addr));

    let next = timeout(Duration::from_secs(5), connections.select_next()).await;

    assert!(matches!(next, Ok(Some(Event::ConnectionClosed(a, _))) if a == task_addr));
}

#[tokio::test]
async fn connections_state_defer_handshake() {
    let addr = RoutingAddr::remote(45);
    let (_incoming_tx, incoming_rx) = mpsc::channel(8);
    let ws = FakeWebsockets;
    let TestFixture {
        mut connections,
        fake_connections: _fake_connections,
        local: _local,
        stop_trigger: _stop_trigger,
    } = make_state(addr, &ws, incoming_rx);

    let sa = sock_addr();

    let (_ws_tx, ws_rx) = duplex(128);
    connections.defer_handshake(ws_rx, sa);

    assert_eq!(connections.deferred.len(), 1);

    let next = timeout(Duration::from_secs(5), connections.select_next()).await;

    match next {
        Ok(Some(Event::Deferred(DeferredResult::ServerHandshake { result, sock_addr }))) => {
            assert!(result.is_ok());
            assert_eq!(sock_addr, sa);
        }
        _ => {
            panic!("Unexpected event.");
        }
    }
}

#[tokio::test]
async fn connections_state_defer_connect_good() {
    let addr = RoutingAddr::remote(45);
    let (_incoming_tx, incoming_rx) = mpsc::channel(8);
    let ws = FakeWebsockets;
    let TestFixture {
        mut connections,
        fake_connections,
        local: _local,
        stop_trigger: _stop_trigger,
    } = make_state(addr, &ws, incoming_rx);

    let target = SchemeHostPort::new(Scheme::Ws, "my_host".to_string(), 80);
    let sa = sock_addr();

    let (_ws_tx, ws_rx) = duplex(128);
    fake_connections.add_dns(target.to_string(), sa);
    fake_connections.add_socket(sa, ws_rx);

    connections.defer_connect_and_handshake(target.clone(), sa, vec![].into_iter());

    assert_eq!(connections.deferred.len(), 1);

    let next = timeout(Duration::from_secs(5), connections.select_next()).await;

    match next {
        Ok(Some(Event::Deferred(DeferredResult::ClientHandshake { result, host }))) => {
            assert!(result.is_ok());
            assert_eq!(host, target);
        }
        _ => {
            panic!("Unexpected event.");
        }
    }
}

#[tokio::test]
async fn connections_state_defer_connect_failed() {
    let addr = RoutingAddr::remote(45);
    let (_incoming_tx, incoming_rx) = mpsc::channel(8);
    let ws = FakeWebsockets;
    let TestFixture {
        mut connections,
        fake_connections,
        local: _local,
        stop_trigger: _stop_trigger,
    } = make_state(addr, &ws, incoming_rx);

    let target = SchemeHostPort::new(Scheme::Ws, "my_host".to_string(), 80);
    let sa = sock_addr();

    fake_connections.add_dns(target.to_string(), sa);
    fake_connections.add_error(sa, ErrorKind::ConnectionReset.into());

    connections.defer_connect_and_handshake(target.clone(), sa, vec![].into_iter());

    assert_eq!(connections.deferred.len(), 1);

    let next = timeout(Duration::from_secs(5), connections.select_next()).await;

    match next {
        Ok(Some(Event::Deferred(DeferredResult::FailedConnection {
            error,
            mut remaining,
            host,
        }))) => {
            assert_eq!(
                error,
                ConnectionError::Io(IoError::new(
                    ErrorKind::ConnectionReset,
                    Some("connection reset".to_string())
                ))
            );
            assert!(remaining.next().is_none());
            assert_eq!(host, target);
        }
        _ => {
            panic!("Unexpected event.");
        }
    }
}

#[tokio::test]
async fn connections_state_defer_dns_good() {
    let addr = RoutingAddr::remote(45);
    let (_incoming_tx, incoming_rx) = mpsc::channel(8);
    let ws = FakeWebsockets;
    let TestFixture {
        mut connections,
        fake_connections,
        local: _local,
        stop_trigger: _stop_trigger,
    } = make_state(addr, &ws, incoming_rx);

    let target = SchemeHostPort::new(Scheme::Ws, "my_host".to_string(), 80);
    let sa = sock_addr();
    fake_connections.add_dns(target.to_string(), sa);

    let (req_tx, req_rx) = oneshot::channel();

    let (envelope_tx, _envelope_rx) = mpsc::channel(8);
    let (request_tx, _request_rx) = mpsc::channel(8);
    let (_drop_tx, drop_rx) = promise::promise();

    let bidirectional_registrator =
        BidirectionalRegistrator::new(TaggedSender::new(addr, envelope_tx), request_tx, drop_rx);

    connections.defer_dns_lookup(
        target.clone(),
        PendingRequest::Resolution(Request::new(req_tx)),
    );

    assert_eq!(connections.deferred.len(), 1);

    let next = timeout(Duration::from_secs(5), connections.select_next()).await;

    match next {
        Ok(Some(Event::Deferred(DeferredResult::Dns {
            result: Ok(it),
            host,
        }))) => {
            assert_eq!(it.collect::<Vec<_>>(), vec![sa]);
            assert_eq!(host, target);
        }
        _ => {
            panic!("Unexpected event.");
        }
    }

    //Check that the pending request was registered.

    connections
        .pending
        .send_ok(&target, RoutingAddr::remote(42), bidirectional_registrator);

    let result = timeout(Duration::from_secs(5), req_rx).await;
    assert!(matches!(result, Ok(Ok(Ok(a))) if a == RoutingAddr::remote(42)));
}

#[tokio::test]
async fn connections_state_defer_dns_failed() {
    let addr = RoutingAddr::remote(45);
    let (_incoming_tx, incoming_rx) = mpsc::channel(8);
    let ws = FakeWebsockets;
    let TestFixture {
        mut connections,
        fake_connections: _fake_connections,
        local: _local,
        stop_trigger: _stop_trigger,
    } = make_state(addr, &ws, incoming_rx);

    let target = SchemeHostPort::new(Scheme::Ws, "my_host".to_string(), 80);
    let (envelope_tx, _envelope_rx) = mpsc::channel(8);
    let (request_tx, _request_rx) = mpsc::channel(8);
    let (_drop_tx, drop_rx) = promise::promise();

    let bidirectional_registrator =
        BidirectionalRegistrator::new(TaggedSender::new(addr, envelope_tx), request_tx, drop_rx);

    let (req_tx, req_rx) = oneshot::channel();

    connections.defer_dns_lookup(
        target.clone(),
        PendingRequest::Resolution(Request::new(req_tx)),
    );

    assert_eq!(connections.deferred.len(), 1);

    let next = timeout(Duration::from_secs(5), connections.select_next()).await;

    match next {
        Ok(Some(Event::Deferred(DeferredResult::Dns {
            result: Err(err),
            host,
        }))) => {
            assert_eq!(err.kind(), ErrorKind::NotFound);
            assert_eq!(host, target);
        }
        _ => {
            panic!("Unexpected event.");
        }
    }

    //Check that the pending request was registered.

    connections
        .pending
        .send_ok(&target, RoutingAddr::remote(42), bidirectional_registrator);

    let result = timeout(Duration::from_secs(5), req_rx).await;
    assert!(matches!(result, Ok(Ok(Ok(a))) if a == RoutingAddr::remote(42)));
}

#[tokio::test]
async fn connections_failure_triggers_pending() {
    let addr = RoutingAddr::remote(45);
    let (_incoming_tx, incoming_rx) = mpsc::channel(8);
    let ws = FakeWebsockets;
    let TestFixture {
        mut connections,
        fake_connections: _fake_connections,
        local: _local,
        stop_trigger: _stop_trigger,
    } = make_state(addr, &ws, incoming_rx);

    let target = SchemeHostPort::new(Scheme::Ws, "my_host".to_string(), 80);
    let (req_tx, req_rx) = oneshot::channel();
    connections.pending.add(
        target.clone(),
        PendingRequest::Resolution(Request::new(req_tx)),
    );

    connections.fail_connection(
        &target,
        ConnectionError::Io(IoError::new(ErrorKind::ConnectionReset, None)),
    );

    let result = timeout(Duration::from_secs(5), req_rx).await;
    let _err = ConnectionError::Io(IoError::new(ErrorKind::ConnectionReset, None));
    assert!(matches!(result, Ok(Ok(Err(_err)))));
}

#[tokio::test]
async fn connections_check_in_table_clears_pending() {
    let addr = RoutingAddr::remote(45);
    let (_incoming_tx, incoming_rx) = mpsc::channel(8);
    let ws = FakeWebsockets;
    let TestFixture {
        mut connections,
        fake_connections: _fake_connections,
        local: _local,
        stop_trigger: _stop_trigger,
    } = make_state(addr, &ws, incoming_rx);

    let host1 = SchemeHostPort::new(Scheme::Ws, "my_host".to_string(), 80);
    let host2 = SchemeHostPort::new(Scheme::Ws, "other_host".to_string(), 80);
    let (req_tx, req_rx) = oneshot::channel();
    let (task_tx, _task_rx) = mpsc::channel(8);

    let (bidirectional_request_tx, _bidirectional_request_rx) = mpsc::channel(8);

    connections.pending.add(
        host2.clone(),
        PendingRequest::Resolution(Request::new(req_tx)),
    );

    let entry_addr = RoutingAddr::plane(5);
    let sa = sock_addr();

    connections.table.insert(
        entry_addr,
        Some(host1),
        sa,
        task_tx,
        bidirectional_request_tx,
    );

    assert!(connections.check_socket_addr(host2, sa).is_ok());

    let result = timeout(Duration::from_secs(5), req_rx).await;

    assert!(matches!(
        result,
        Ok(Ok(Ok(a))) if a == entry_addr
    ));
}

#[tokio::test]
async fn connections_state_shutdown_process() {
    let addr = RoutingAddr::remote(45);
    let (_incoming_tx, incoming_rx) = mpsc::channel(8);
    let ws = FakeWebsockets;
    let TestFixture {
        mut connections,
        fake_connections: _fake_connections,
        local: _local,
        stop_trigger,
    } = make_state(addr, &ws, incoming_rx);

    let sa = sock_addr();

    let (web_sock, _ws_rx) = websocket_for(Role::Client, NoExt).await;
    let host1 = SchemeHostPort::new(Scheme::Ws, "my_host".to_string(), 80);
    let host2 = SchemeHostPort::new(Scheme::Ws, "other".to_string(), 80);

    let (req_tx, _req_rx) = oneshot::channel();

    connections.spawn_task(sa, web_sock, Some(host1.clone()));
    connections.defer_dns_lookup(
        host2.clone(),
        PendingRequest::Resolution(Request::new(req_tx)),
    );

    let (result_tx, _result_rx) = mpsc::channel(8);
    stop_trigger.provide(result_tx).unwrap();

    let first = timeout(Duration::from_secs(5), connections.select_next()).await;
    assert!(matches!(first, Ok(Some(_))));

    let second = timeout(Duration::from_secs(5), connections.select_next()).await;
    assert!(matches!(second, Ok(Some(_))));

    let term = timeout(Duration::from_secs(5), connections.select_next()).await;
    assert!(matches!(term, Ok(None)));
}
