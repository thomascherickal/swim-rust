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

use crate::agent::lane::channels::uplink::{Uplink, UplinkAction, UplinkError, UplinkMessage};
use crate::agent::lane::channels::{LaneMessageHandler, OutputMessage, TaggedAction};
use crate::routing::{RoutingAddr, ServerRouter};
use common::model::Value;
use common::sink::item::ItemSender;
use common::topic::Topic;
use common::warp::envelope::Envelope;
use common::warp::path::RelativePath;
use futures::future::{join_all, BoxFuture};
use futures::{FutureExt, StreamExt};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::num::NonZeroUsize;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{event, span, Level};
use tracing_futures::Instrument;
use utilities::sync::trigger;

const FAILED_ERR_REPORT: &str = "Failed to send error report.";
const UPLINK_TERMINATED: &str = "An uplink terminated uncleanly.";

pub struct UplinkSpawner<Handler, Top> {
    handler: Arc<Handler>,
    topic: Top,
    actions: mpsc::Receiver<TaggedAction>,
    action_buffer_size: NonZeroUsize,
    max_start_attempts: NonZeroUsize,
    route: RelativePath,
}

impl<Handler, Top> UplinkSpawner<Handler, Top>
where
    Handler: LaneMessageHandler,
    OutputMessage<Handler>: Into<Value>,
    Top: Topic<Handler::Event>,
{
    pub fn new(
        handler: Arc<Handler>,
        topic: Top,
        rx: mpsc::Receiver<TaggedAction>,
        action_buffer_size: NonZeroUsize,
        max_start_attempts: NonZeroUsize,
        route: RelativePath,
    ) -> Self {
        UplinkSpawner {
            handler,
            topic,
            actions: rx,
            action_buffer_size,
            max_start_attempts,
            route,
        }
    }

    pub async fn run<Router>(
        mut self,
        mut router: Router,
        mut spawn_tx: mpsc::Sender<BoxFuture<'static, ()>>,
        mut error_collector: mpsc::Sender<UplinkErrorReport>,
    ) where
        Router: ServerRouter,
    {
        let mut uplink_senders: HashMap<RoutingAddr, UplinkHandle> = HashMap::new();

        while let Some(TaggedAction(addr, mut action)) = self.actions.recv().await {
            let mut attempts = 0;
            let is_done = loop {
                let sender = match uplink_senders.entry(addr) {
                    Entry::Occupied(entry) => Some(entry.into_mut()),
                    Entry::Vacant(entry) => {
                        if let Some(handle) = self
                            .make_uplink(addr, error_collector.clone(), &mut spawn_tx, &mut router)
                            .await
                        {
                            Some(entry.insert(handle))
                        } else {
                            None
                        }
                    }
                };
                if let Some(sender) = sender {
                    if let Err(mpsc::error::SendError(act)) = sender.send(action).await {
                        if let Some(handle) = uplink_senders.remove(&addr) {
                            if !handle.cleanup().await {
                                event!(Level::ERROR, message = UPLINK_TERMINATED, route = ?&self.route, ?addr);
                            }
                        }
                        action = act;
                        attempts += 1;
                        if attempts >= self.max_start_attempts.get() {
                            let report =
                                UplinkErrorReport::new(UplinkError::FailedToStart(attempts), addr);
                            if let Err(mpsc::error::SendError(report)) =
                                error_collector.send(report).await
                            {
                                event!(Level::ERROR, message = FAILED_ERR_REPORT, ?report);
                            }
                            break false;
                        }
                    } else {
                        break false;
                    }
                } else {
                    break true;
                }
            };
            if is_done {
                break;
            }
        }
        join_all(uplink_senders.into_iter().map(|(_, h)| h.cleanup())).await;
    }

    async fn make_uplink<Router>(
        &mut self,
        addr: RoutingAddr,
        mut err_tx: mpsc::Sender<UplinkErrorReport>,
        spawn_tx: &mut mpsc::Sender<BoxFuture<'static, ()>>,
        router: &mut Router,
    ) -> Option<UplinkHandle>
    where
        Router: ServerRouter,
    {
        let UplinkSpawner {
            handler,
            topic,
            action_buffer_size,
            route,
            ..
        } = self;
        let (tx, rx) = mpsc::channel(action_buffer_size.get());
        let (cleanup_tx, cleanup_rx) = trigger::trigger();
        let state_machine = handler.make_uplink();
        let updates = if let Ok(sub) = topic.subscribe().await {
            sub.fuse()
        } else {
            return None;
        };
        let uplink = Uplink::new(state_machine, rx.fuse(), updates);

        let route_cpy = route.clone();

        let sink = if let Ok(sender) = router.get_sender(addr) {
            sender.comap(
                move |msg: UplinkMessage<OutputMessage<Handler>>| match msg {
                    UplinkMessage::Linked => Envelope::linked(&route_cpy.node, &route_cpy.lane),
                    UplinkMessage::Synced => Envelope::synced(&route_cpy.node, &route_cpy.lane),
                    UplinkMessage::Unlinked => Envelope::unlinked(&route_cpy.node, &route_cpy.lane),
                    UplinkMessage::Event(ev) => {
                        Envelope::make_event(&route_cpy.node, &route_cpy.lane, Some(ev.into()))
                    }
                },
            )
        } else {
            return None;
        };
        let ul_task = async move {
            if let Err(err) = uplink.run_uplink(sink).await {
                let report = UplinkErrorReport::new(err, addr);
                if let Err(mpsc::error::SendError(report)) = err_tx.send(report).await {
                    event!(Level::ERROR, message = FAILED_ERR_REPORT, ?report);
                }
                cleanup_tx.trigger();
            }
        }
        .instrument(span!(Level::INFO, "Lane uplink.", ?route, ?addr));
        if spawn_tx.send(ul_task.boxed()).await.is_err() {
            return None;
        }
        Some(UplinkHandle::new(tx, cleanup_rx))
    }
}

struct UplinkHandle {
    sender: mpsc::Sender<UplinkAction>,
    wait_on_cleanup: trigger::Receiver,
}

impl UplinkHandle {
    fn new(sender: mpsc::Sender<UplinkAction>, wait_on_cleanup: trigger::Receiver) -> Self {
        UplinkHandle {
            sender,
            wait_on_cleanup,
        }
    }

    async fn send(
        &mut self,
        action: UplinkAction,
    ) -> Result<(), mpsc::error::SendError<UplinkAction>> {
        self.sender.send(action).await
    }

    async fn cleanup(self) -> bool {
        let UplinkHandle {
            sender,
            wait_on_cleanup,
        } = self;
        drop(sender);
        wait_on_cleanup.await.is_ok()
    }
}

#[derive(Debug)]
pub struct UplinkErrorReport {
    pub error: UplinkError,
    pub addr: RoutingAddr,
}

impl UplinkErrorReport {
    fn new(error: UplinkError, addr: RoutingAddr) -> Self {
        UplinkErrorReport { error, addr }
    }
}

impl Display for UplinkErrorReport {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Uplink to {} failed: {}", &self.addr, &self.error)
    }
}
