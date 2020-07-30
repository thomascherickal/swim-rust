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

use crate::agent::lane::channels::uplink::{Uplink, UplinkAction, UplinkMessage};
use crate::agent::lane::model::value;
use crate::agent::lane::strategy::Queue;
use futures::future::join;
use futures::ready;
use futures::{Stream, StreamExt};
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use utilities::sync::trigger;

struct ReportingStream<S> {
    notify: VecDeque<trigger::Sender>,
    values: S,
}

impl<S: Unpin> Stream for ReportingStream<S>
where
    S: Stream,
{
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let ReportingStream { notify, values } = self.get_mut();

        let v = ready!(values.poll_next_unpin(cx));
        if v.is_some() {
            if let Some(trigger) = notify.pop_front() {
                trigger.trigger();
            }
        }
        Poll::Ready(v)
    }
}

impl<S: Stream> ReportingStream<S> {
    pub fn new(inner: S, notify: Vec<trigger::Sender>) -> Self {
        ReportingStream {
            notify: notify.into_iter().collect(),
            values: inner,
        }
    }
}

#[tokio::test]
async fn uplink_not_linked() {
    let (lane, events) = value::make_lane_model::<i32, Queue>(0, Queue::default());

    let (on_event_tx, on_event_rx) = trigger::trigger();

    let events = ReportingStream::new(events, vec![on_event_tx]);

    let (mut tx_action, rx_action) = mpsc::channel::<UplinkAction>(5);

    let uplink = Uplink::new(lane.clone(), rx_action.fuse(), events.fuse());

    let (tx_event, rx_event) = mpsc::channel(5);

    let uplink_task = uplink.run_uplink(tx_event);

    let send_task = async move {
        lane.store(12).await;
        assert!(on_event_rx.await.is_ok());
        assert!(tx_action.send(UplinkAction::Unlink).await.is_ok());
        rx_event.collect::<Vec<_>>().await
    };

    let (uplink_result, send_result) = join(
        timeout(Duration::from_secs(10), uplink_task),
        timeout(Duration::from_secs(10), send_task),
    )
    .await;

    assert!(matches!(uplink_result, Ok(Ok(_))));
    assert!(send_result.is_ok());
    assert!(matches!(
        send_result.unwrap().as_slice(),
        [UplinkMessage::Unlinked]
    ));
}

#[tokio::test]
async fn uplink_open_to_linked() {
    let (lane, events) = value::make_lane_model::<i32, Queue>(0, Queue::default());

    let (on_event_tx_1, on_event_rx_1) = trigger::trigger();
    let (on_event_tx_2, on_event_rx_2) = trigger::trigger();

    let events = ReportingStream::new(events, vec![on_event_tx_1, on_event_tx_2]);

    let (mut tx_action, rx_action) = mpsc::channel::<UplinkAction>(5);

    let uplink = Uplink::new(lane.clone(), rx_action.fuse(), events.fuse());

    let (tx_event, rx_event) = mpsc::channel(5);

    let uplink_task = uplink.run_uplink(tx_event);

    let send_task = async move {
        lane.store(12).await;
        assert!(on_event_rx_1.await.is_ok());
        assert!(tx_action.send(UplinkAction::Link).await.is_ok());
        lane.store(25).await;
        assert!(on_event_rx_2.await.is_ok());
        assert!(tx_action.send(UplinkAction::Unlink).await.is_ok());
        rx_event.collect::<Vec<_>>().await
    };

    let (uplink_result, send_result) = join(
        timeout(Duration::from_secs(10), uplink_task),
        timeout(Duration::from_secs(10), send_task),
    )
    .await;

    assert!(matches!(uplink_result, Ok(Ok(_))));
    assert!(send_result.is_ok());
    assert!(matches!(
        send_result.unwrap().as_slice(),
        [
            UplinkMessage::Linked,
            UplinkMessage::Event(v),
            UplinkMessage::Unlinked
        ] if **v == 25
    ));
}

#[tokio::test]
async fn uplink_open_to_synced() {
    let (lane, events) = value::make_lane_model::<i32, Queue>(0, Queue::default());

    let (on_event_tx, on_event_rx) = trigger::trigger();

    let events = ReportingStream::new(events, vec![on_event_tx]);

    let (mut tx_action, rx_action) = mpsc::channel::<UplinkAction>(5);

    let uplink = Uplink::new(lane.clone(), rx_action.fuse(), events.fuse());

    let (tx_event, rx_event) = mpsc::channel(5);

    let uplink_task = uplink.run_uplink(tx_event);

    let send_task = async move {
        lane.store(12).await;
        assert!(on_event_rx.await.is_ok());
        assert!(tx_action.send(UplinkAction::Sync).await.is_ok());
        assert!(tx_action.send(UplinkAction::Unlink).await.is_ok());
        rx_event.collect::<Vec<_>>().await
    };

    let (uplink_result, send_result) = join(
        timeout(Duration::from_secs(10), uplink_task),
        timeout(Duration::from_secs(10), send_task),
    )
    .await;

    assert!(matches!(uplink_result, Ok(Ok(_))));
    assert!(send_result.is_ok());
    assert!(matches!(
        send_result.unwrap().as_slice(),
        [
            UplinkMessage::Linked,
            UplinkMessage::Event(v),
            UplinkMessage::Synced,
            UplinkMessage::Unlinked
        ] if **v == 12
    ));
}
