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

use crate::routing::remote::pending::PendingRequests;
use crate::routing::remote::table::HostAndPort;
use crate::routing::RoutingAddr;
use futures::future::join;
use swim_common::request::Request;
use swim_common::routing::{CloseError, CloseErrorKind, ConnectionError};
use tokio::sync::oneshot;

#[tokio::test]
async fn add_single_and_send_err() {
    let key = HostAndPort::new("host".to_string(), 42);
    let (tx, rx) = oneshot::channel();
    let req = Request::new(tx);

    let mut pending = PendingRequests::default();
    pending.add(key.clone(), req);
    pending.send_err(
        &key,
        ConnectionError::Closed(CloseError::new(CloseErrorKind::ClosedRemotely, None)),
    );

    let result = rx.await;
    assert_eq!(
        result,
        Ok(Err(ConnectionError::Closed(CloseError::new(
            CloseErrorKind::ClosedRemotely,
            None,
        ))))
    );
}

#[tokio::test]
async fn add_two_and_send_err() {
    let key = HostAndPort::new("host".to_string(), 42);
    let (tx1, rx1) = oneshot::channel();
    let (tx2, rx2) = oneshot::channel();
    let req1 = Request::new(tx1);
    let req2 = Request::new(tx2);

    let mut pending = PendingRequests::default();
    pending.add(key.clone(), req1);
    pending.add(key.clone(), req2);
    pending.send_err(
        &key,
        ConnectionError::Closed(CloseError::new(CloseErrorKind::ClosedRemotely, None)),
    );

    let results = join(rx1, rx2).await;

    assert_eq!(
        results,
        (
            Ok(Err(ConnectionError::Closed(CloseError::new(
                CloseErrorKind::ClosedRemotely,
                None,
            )))),
            Ok(Err(ConnectionError::Closed(CloseError::new(
                CloseErrorKind::ClosedRemotely,
                None,
            ))))
        )
    );
}

#[tokio::test]
async fn add_single_and_send_ok() {
    let key = HostAndPort::new("host".to_string(), 42);
    let (tx, rx) = oneshot::channel();
    let req = Request::new(tx);
    let addr = RoutingAddr::remote(2);

    let mut pending = PendingRequests::default();
    pending.add(key.clone(), req);
    pending.send_ok(&key, addr);

    let result = rx.await;
    assert_eq!(result, Ok(Ok(addr)));
}

#[tokio::test]
async fn add_two_and_send_ok() {
    let key = HostAndPort::new("host".to_string(), 42);
    let (tx1, rx1) = oneshot::channel();
    let (tx2, rx2) = oneshot::channel();
    let req1 = Request::new(tx1);
    let req2 = Request::new(tx2);
    let addr = RoutingAddr::remote(2);

    let mut pending = PendingRequests::default();
    pending.add(key.clone(), req1);
    pending.add(key.clone(), req2);
    pending.send_ok(&key, addr);

    let results = join(rx1, rx2).await;

    assert_eq!(results, (Ok(Ok(addr)), Ok(Ok(addr))));
}

#[tokio::test]
async fn add_two_drop_one() {
    let key = HostAndPort::new("host".to_string(), 42);
    let (tx1, rx1) = oneshot::channel();
    let (tx2, rx2) = oneshot::channel();
    let req1 = Request::new(tx1);
    let req2 = Request::new(tx2);
    let addr = RoutingAddr::remote(2);

    let mut pending = PendingRequests::default();
    pending.add(key.clone(), req1);
    pending.add(key.clone(), req2);

    drop(rx1);

    pending.send_ok(&key, addr);

    let results = rx2.await;

    assert_eq!(results, Ok(Ok(addr)));
}
