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

use crate::plane::router::{PlaneRouter, PlaneRouterFactory};
use crate::routing::{PlaneRoutingRequest, TopLevelServerRouter, TopLevelServerRouterFactory};
use crate::uri::RelativeUri;
use futures::future::join;
use swim_runtime::error::{ConnectionError, ResolutionErrorKind, RouterError, Unresolvable};
use swim_runtime::remote::RawOutRoute;
use swim_runtime::routing::{Router, RouterFactory, RoutingAddr, TaggedEnvelope};
use swim_utilities::trigger::promise;
use swim_warp::envelope::Envelope;
use tokio::sync::mpsc;
use url::Url;

#[tokio::test]
async fn plane_router_get_sender() {
    let addr = RoutingAddr::plane(5);

    let (req_tx, mut req_rx) = mpsc::channel(8);
    let (send_tx, mut send_rx) = mpsc::channel(8);
    let (_drop_tx, drop_rx) = promise::promise();

    let (remote_tx, _remote_rx) = mpsc::channel(8);
    let (client_tx, _client_rx) = mpsc::channel(8);
    let top_level_router = TopLevelServerRouter::new(addr, req_tx.clone(), client_tx, remote_tx);

    let mut router = PlaneRouter::new(addr, top_level_router, req_tx);

    let provider_task = async move {
        while let Some(req) = req_rx.recv().await {
            if let PlaneRoutingRequest::Endpoint { id, request } = req {
                if id == addr {
                    assert!(request
                        .send_ok(RawOutRoute::new(send_tx.clone(), drop_rx.clone()))
                        .is_ok());
                } else {
                    assert!(request.send_err(Unresolvable(id)).is_ok());
                }
            } else {
                panic!("Unexpected request {:?}!", req);
            }
        }
    };

    let send_task = async move {
        let result1 = router.resolve_sender(addr).await;
        assert!(result1.is_ok());
        let mut sender = result1.unwrap();
        assert!(sender
            .send_item(Envelope::linked().node_uri("/node").lane_uri("lane").done())
            .await
            .is_ok());
        assert_eq!(
            send_rx.recv().await,
            Some(TaggedEnvelope(
                addr,
                Envelope::linked().node_uri("/node").lane_uri("lane").done()
            ))
        );

        let result2 = router.resolve_sender(RoutingAddr::plane(56)).await;

        assert!(matches!(
            result2.err().unwrap().kind(),
            ResolutionErrorKind::Unresolvable
        ));
    };

    join(provider_task, send_task).await;
}

#[tokio::test]
async fn plane_router_factory() {
    let (req_tx, _req_rx) = mpsc::channel(8);

    let (remote_tx, _remote_rx) = mpsc::channel(8);
    let (client_tx, _client_rx) = mpsc::channel(8);
    let top_level_router_factory =
        TopLevelServerRouterFactory::new(req_tx.clone(), client_tx, remote_tx);

    let fac = PlaneRouterFactory::new(req_tx, top_level_router_factory);
    let router = fac.create_for(RoutingAddr::plane(56));
    assert_eq!(router.tag, RoutingAddr::plane(56));
}

#[tokio::test]
async fn plane_router_resolve() {
    let host = Url::parse("warp:://somewhere").unwrap();
    let addr = RoutingAddr::remote(5);

    let (req_tx, mut req_rx) = mpsc::channel(8);

    let (remote_tx, _remote_rx) = mpsc::channel(8);
    let (client_tx, _client_rx) = mpsc::channel(8);
    let top_level_router = TopLevelServerRouter::new(addr, req_tx.clone(), client_tx, remote_tx);

    let mut router = PlaneRouter::new(addr, top_level_router, req_tx);

    let provider_task = async move {
        while let Some(req) = req_rx.recv().await {
            if let PlaneRoutingRequest::Resolve { name, request } = req {
                if name == "/node" {
                    assert!(request.send_ok(addr).is_ok());
                } else {
                    assert!(request.send_err(RouterError::NoAgentAtRoute(name)).is_ok());
                }
            } else {
                panic!("Unexpected request {:?}!", req);
            }
        }
    };

    let send_task = async move {
        let result1 = router.lookup(None, "/node".parse().unwrap()).await;
        assert!(matches!(result1, Ok(a) if a == addr));

        let uri: RelativeUri = "/node".parse().unwrap();

        let result2 = router.lookup(Some(host.clone()), uri.clone()).await;

        assert!(
            matches!(result2, Err(RouterError::ConnectionFailure(ConnectionError::Resolution(msg))) if msg == host.to_string())
        );

        let result3 = router.lookup(None, "/other".parse().unwrap()).await;
        assert!(matches!(result3, Err(RouterError::NoAgentAtRoute(name)) if name == "/other"));
    };

    join(provider_task, send_task).await;
}
