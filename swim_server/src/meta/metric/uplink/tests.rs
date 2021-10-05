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

use crate::agent::lane::channels::uplink::backpressure::KeyedBackpressureConfig;
use crate::agent::lane::model::supply::SupplyLane;
use crate::agent::model::supply::make_lane_model;
use crate::meta::metric::config::MetricAggregatorConfig;
use crate::meta::metric::tests::{backpressure_config, DEFAULT_BUFFER, DEFAULT_YIELD};
use crate::meta::metric::uplink::{
    uplink_aggregator, uplink_observer, AggregatorConfig, TaggedWarpUplinkProfile, TrySendError,
    UplinkObserver, UplinkProfileSender, WarpUplinkProfile, WarpUplinkPulse,
};
use crate::meta::metric::MetricStage;
use futures::future::{join, join3};
use futures::stream::iter;
use futures::{FutureExt, StreamExt};
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::ops::Add;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::time::Duration;
use swim_async_runtime::time::delay::delay_for;
use swim_async_runtime::time::timeout::timeout;
use swim_model::path::RelativePath;
use swim_utilities::routing::uri::RelativeUri;
use swim_utilities::trigger;
use tokio::sync::mpsc;
use tokio::time::sleep;

struct UplinkMetricObserver {
    sample_rate: Duration,
    node_uri: String,
    metric_tx: mpsc::Sender<TaggedWarpUplinkProfile>,
}

impl UplinkMetricObserver {
    fn new(
        sample_rate: Duration,
        node_uri: RelativeUri,
        metric_tx: mpsc::Sender<TaggedWarpUplinkProfile>,
    ) -> UplinkMetricObserver {
        UplinkMetricObserver {
            sample_rate,
            node_uri: node_uri.to_string(),
            metric_tx,
        }
    }

    fn uplink_observer(&self, lane_uri: String) -> UplinkObserver {
        let UplinkMetricObserver {
            sample_rate,
            node_uri,
            metric_tx,
        } = self;
        let profile_sender =
            UplinkProfileSender::new(RelativePath::new(node_uri, lane_uri), metric_tx.clone());

        uplink_observer(*sample_rate, profile_sender)
    }
}

#[tokio::test]
async fn uplink_sender_ok() {
    let path = RelativePath::new("/node", "/lane");
    let (tx, mut rx) = mpsc::channel(1);
    let sender = UplinkProfileSender::new(path.clone(), tx);
    let profile = WarpUplinkProfile::default();

    assert!(sender.try_send(profile.clone()).is_ok());
    let expected = TaggedWarpUplinkProfile { path, profile };

    assert_eq!(rx.recv().now_or_never().unwrap().unwrap(), expected);
}

#[tokio::test]
async fn uplink_sender_err() {
    let path = RelativePath::new("/node", "/lane");
    let (tx, _) = mpsc::channel(1);
    let sender = UplinkProfileSender::new(path.clone(), tx);

    assert_eq!(
        sender.try_send(WarpUplinkProfile::default()),
        Err(TrySendError)
    );
}

#[tokio::test]
async fn receive() {
    let path = RelativePath::new("/node", "/lane");
    let (tx, mut rx) = mpsc::channel(1);
    let sender = UplinkProfileSender::new(path.clone(), tx);

    let observer = uplink_observer(Duration::from_nanos(1), sender);

    observer.inner.command_delta.fetch_add(2, Ordering::Acquire);
    observer.inner.event_delta.fetch_add(2, Ordering::Acquire);

    observer.inner.flush();

    let tagged = rx.recv().await.unwrap();

    assert_eq!(tagged.profile.event_delta, 2);
    assert_eq!(tagged.profile.command_delta, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn receive_threaded() {
    let path = RelativePath::new("/node", "/lane");
    let (tx, mut rx) = mpsc::channel(2048);
    let sender = UplinkProfileSender::new(path.clone(), tx);

    let sample_rate = Duration::from_secs(1);
    let observer = uplink_observer(sample_rate, sender);

    let make_task = |observer: UplinkObserver| async move {
        for i in 0..100 {
            observer.on_event();
            observer.on_command();
            observer.did_open();
            observer.did_close();

            if i % 10 == 0 {
                sleep(Duration::from_millis(100)).await;
            }
        }

        observer.inner.accumulate_and_send();
    };

    let task_left = make_task(observer.clone());
    let task_right = make_task(observer);

    let _r = join(task_left, task_right).await;

    sleep(sample_rate * 2).await;

    let mut accumulated = WarpUplinkProfile::default();

    while let Some(part) = rx.recv().await {
        accumulated = accumulated.add(part.profile);
    }

    assert_eq!(accumulated.event_delta as u64, 200);
    assert_eq!(accumulated.command_delta as u64, 200);
    assert_eq!(accumulated.open_delta, 200);
    assert_eq!(accumulated.close_delta, 200);
}

#[tokio::test]
async fn task_backpressure() {
    let (stop_tx, stop_rx) = trigger::trigger();

    let format_lane = |id: usize| -> String { format!("/lane/{}", id) };

    // the number of lanes
    let count = 10;
    let buffer_size = 2;
    // The number of messages to send to each lane. Twice the buffer size to ensure that it overflows
    let message_count = buffer_size * 2;

    let sample_rate = Duration::from_millis(100);

    let config = MetricAggregatorConfig {
        sample_rate,
        backpressure_config: KeyedBackpressureConfig {
            buffer_size: NonZeroUsize::new(buffer_size).unwrap(),
            yield_after: DEFAULT_YIELD,
            bridge_buffer_size: NonZeroUsize::new(buffer_size).unwrap(),
            cache_size: NonZeroUsize::new(count).unwrap(),
        },
        ..Default::default()
    };

    let mut lanes = HashMap::new();
    let mut lane_rx = HashMap::new();
    let mut lane_set = HashSet::new();

    (0..count).into_iter().for_each(|i| {
        let (supply_lane, supply_rx) = make_lane_model(NonZeroUsize::new(10).unwrap());
        let key = format_lane(i);

        lane_set.insert(key.clone());
        let path = RelativePath::new("/node", key.clone());
        lanes.insert(path, supply_lane);
        lane_rx.insert(key, supply_rx);
    });

    let (lane_profile_tx, _lane_profile_rx) = mpsc::channel(4096);
    let (finish_tx, finish_rx) = trigger::trigger();
    let aggregator_config = AggregatorConfig {
        sample_rate,
        buffer_size: NonZeroUsize::new(buffer_size).unwrap(),
        yield_after: DEFAULT_YIELD,
        backpressure_config: config.backpressure_config,
    };
    let (uplink_task, uplink_tx) = uplink_aggregator(
        aggregator_config,
        stop_rx,
        lanes,
        lane_profile_tx,
        finish_tx,
    );

    let observer = UplinkMetricObserver::new(
        config.sample_rate,
        RelativeUri::from_str("/node").unwrap(),
        uplink_tx,
    );

    let task_jh = tokio::spawn(uplink_task);

    iter(0..count)
        .fold(observer, |observer, lane_id| async move {
            let inner_observer = observer.uplink_observer(format_lane(lane_id));

            iter(0..message_count)
                .fold(inner_observer, |observer, _message_id| async {
                    delay_for(sample_rate).await;

                    observer.inner.event_delta.fetch_add(1, Ordering::Acquire);
                    observer.inner.command_delta.fetch_add(1, Ordering::Acquire);
                    observer.inner.flush();

                    observer
                })
                .await;

            observer
        })
        .await;

    stop_tx.trigger();

    let (_, lane_set) = iter(0..count)
        .fold(
            (lane_rx, lane_set),
            |(mut lane_rx, mut lane_set), lane_id| async move {
                let lane_key = format_lane(lane_id);
                let lane = lane_rx.get_mut(&lane_key).unwrap();

                let lane_set = timeout(Duration::from_secs(5), async move {
                    while let Some(_) = lane.next().await {
                        lane_set.remove(&lane_key);
                    }

                    lane_set
                })
                .await
                .expect("Failed to receive any profiles");

                (lane_rx, lane_set)
            },
        )
        .await;

    assert!(lane_set.is_empty());

    let _ = task_jh.await.unwrap();
    assert!(finish_rx.await.is_ok());
}

#[tokio::test]
async fn with_observer() {
    let (stop_tx, stop_rx) = trigger::trigger();
    let (supply_lane_tx, mut supply_lane_rx) = mpsc::channel(5);
    let lane = SupplyLane::new(supply_lane_tx);

    let mut lane_map = HashMap::new();
    let path = RelativePath::new("/node", "lane");

    lane_map.insert(path.clone(), lane);

    let (lane_tx, mut lane_rx) = mpsc::channel(5);
    let sample_rate = Duration::from_millis(100);

    let supply_rcv_task = async move {
        let pulse: WarpUplinkPulse = supply_lane_rx.recv().await.unwrap();
        assert_eq!(pulse.event_count, 1);
    };

    let lane_rcv_task = async move {
        let (_path, profile): (_, WarpUplinkProfile) = lane_rx.recv().await.unwrap();

        assert_eq!(profile.event_delta, 1);
        assert_eq!(profile.open_delta, 1);
        assert_eq!(profile.close_delta, 1);
    };
    let (finish_tx, finish_rx) = trigger::trigger();
    let config = AggregatorConfig {
        sample_rate,
        buffer_size: DEFAULT_BUFFER,
        yield_after: DEFAULT_YIELD,
        backpressure_config: backpressure_config(),
    };
    let (uplink_task, uplink_tx) = uplink_aggregator(config, stop_rx, lane_map, lane_tx, finish_tx);

    let task = tokio::spawn(join3(uplink_task, lane_rcv_task, supply_rcv_task));

    let observer = UplinkMetricObserver::new(
        sample_rate,
        RelativeUri::from_str("/node").unwrap(),
        uplink_tx,
    );

    let observer = observer.uplink_observer("lane".to_string());

    observer.inner.event_delta.fetch_add(1, Ordering::Relaxed);
    observer.inner.open_delta.fetch_add(1, Ordering::Relaxed);
    observer.inner.close_delta.fetch_add(1, Ordering::Relaxed);

    sleep(sample_rate).await;

    observer.inner.flush();

    sleep(sample_rate).await;

    stop_tx.trigger();

    let (task_result, _, _) = task.await.unwrap();

    assert_eq!(task_result, Ok(MetricStage::Uplink));
    assert!(finish_rx.await.is_ok());
}
