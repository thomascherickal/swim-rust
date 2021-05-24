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

use crate::meta::log::{LogEntry, LogLevel, NodeLogger};
use crate::meta::metric::aggregator::{AggregatorTask, MetricState};
use crate::meta::metric::config::MetricAggregatorConfig;
use crate::meta::metric::lane::LaneMetricReporter;
use crate::meta::metric::node::NodeAggregatorTask;
use crate::meta::metric::uplink::{
    uplink_aggregator, uplink_observer, TaggedWarpUplinkProfile, UplinkActionObserver,
    UplinkEventObserver, UplinkProfileSender,
};
use crate::meta::pulse::PulseLanes;
use futures::future::try_join3;
use futures::Future;
use std::fmt::{Display, Formatter};
use std::ops::Add;
use swim_common::warp::path::RelativePath;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::SendError as TokioSendError;
use tokio::sync::mpsc::Sender;
use tokio::time::Duration;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{span, Level};
use tracing_futures::Instrument;
use utilities::sync::trigger;
use utilities::uri::RelativeUri;

pub mod aggregator;
pub mod config;
pub mod lane;
pub mod node;
pub mod uplink;

#[cfg(test)]
mod tests;

const AGGREGATOR_TASK: &str = "Metric aggregator task";
pub(crate) const STOP_OK: &str = "Aggregator stopped normally";
pub(crate) const STOP_CLOSED: &str = "Aggregator event stream unexpectedly closed";
const LOG_ERROR_MSG: &str = "Node aggregator failed";
const LOG_TASK_FINISHED_MSG: &str = "Node aggregator task completed";

/// A metric aggregator kind or stage in the pipeline.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum MetricStage {
    /// A node aggregator which accepts lane profiles and produces node pulses/profiles.
    Node,
    /// A lane aggregator which accepts uplink profiles and produces lane pulses/profiles.
    Lane,
    /// An uplink aggregator which aggregates events and actions which occur in the uplink and then
    /// produces uplink profiles and a pulse.
    Uplink,
}

impl Display for MetricStage {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct AggregatorError {
    /// The type of aggregator that errored.
    pub(crate) aggregator: MetricStage,
    /// The underlying error.
    error: AggregatorErrorKind,
}

impl Display for AggregatorError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let AggregatorError { aggregator, error } = self;
        write!(f, "{} aggregator errored with: {}", aggregator, error)
    }
}

impl From<TokioSendError<TaggedWarpUplinkProfile>> for AggregatorError {
    fn from(_: TokioSendError<TaggedWarpUplinkProfile>) -> Self {
        AggregatorError {
            aggregator: MetricStage::Uplink,
            error: AggregatorErrorKind::ForwardChannelClosed,
        }
    }
}

/// An error produced by a metric aggregator.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum AggregatorErrorKind {
    /// The aggregator's forward/output channel closed.
    ForwardChannelClosed,
    /// The input stream to the aggregator closed unexpectedly.
    AbnormalStop,
}

impl Display for AggregatorErrorKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            AggregatorErrorKind::ForwardChannelClosed => {
                write!(f, "Aggregator's forward channel closed")
            }
            AggregatorErrorKind::AbnormalStop => {
                write!(f, "Aggregator's input stream closed unexpectedly")
            }
        }
    }
}

/// A metric reporter which will take its input and produce a node and profile for the metric.
pub trait MetricReporter {
    /// The stage in the pipeline that this metric is.
    const METRIC_STAGE: MetricStage;

    /// The type of pulse that is generated by this metric.
    type Pulse: Send + Sync + 'static;
    /// The type of pulse that is generated by this metric.
    type Profile: Send + Sync + 'static;
    /// An aggregated metric that this reporter will use to produce a pulse and profile from.
    type Input: Add<Self::Input, Output = Self::Input> + Copy + Default;

    /// Produce a pulse and profile from an accumulated metric.
    fn report(&mut self, part: Self::Input) -> (Self::Pulse, Self::Profile);
}

/// A node metric aggregator.
///
/// The aggregator has an input channel which is fed WARP uplink profiles which are produced by
/// uplink observers. The reporting interval of these profiles is configurable and backpressure
/// relief is also applied. Profile reporting is event driven and a profile will only be reported
/// if a metric is reported *and* the sample period has elapsed; no periodic flushing of stale
/// profiles is applied. These profiles are then aggregated by an uplink aggregator and a WARP
/// uplink pulse is produced at the corresponding supply lane as well as a WARP uplink profile being
/// forwarded the the next stage in the pipeline. The same process is repeated for lanes and finally
/// a node pulse and profile is produced.
///
/// This aggregator is structured in a fan-in fashion and profiles and pulses are debounced by the
/// sample rate which is provided at creation. In addition to this, no guarantees are made as to
/// whether the the pulse or profiles will be delivered; if the channel is full, then the message is
/// dropped.
#[derive(Debug, Clone)]
pub struct NodeMetricAggregator {
    /// The same rate at which profiles will be reported.
    sample_rate: Duration,
    /// The URI of the metric aggregator.
    node_uri: String,
    /// A reporting channel for the accumulated profile.
    metric_tx: Sender<TaggedWarpUplinkProfile>,
}

#[cfg(test)]
pub fn aggregator_sink() -> NodeMetricAggregator {
    NodeMetricAggregator {
        sample_rate: Duration::default(),
        node_uri: String::from("test"),
        metric_tx: mpsc::channel(1).0,
    }
}

impl NodeMetricAggregator {
    /// Creates a new node metric aggregator for the `node_uri`.
    ///
    /// # Arguments:
    ///
    /// * `node_uri` - The URI that this aggregator corresponds to.
    /// * `stop_rx` - A stop signal for shutting down the aggregator. When this is triggered, it
    /// will cause all of the pending profiles and pulses to be flushed. Regardless of the last
    /// flush time.
    /// * `config` - A configuration for the aggregator and backpressure.
    /// * `lanes`- A collection of lanes that the metrics will be sent to for: uplink, lane and node
    /// pulses.
    /// * `uplink_pulse_lanes` - A map keyed by lane paths and that contains supply lanes for
    /// WARP uplink pulses.
    /// * `lane_pulse_lanes` - A map keyed by lane paths and that contains supply lanes for lane
    /// pulses.
    /// * `agent_pulse` - A supply lane for producing a node's pulse.
    /// * `log_context` - Logging context for reporting errors that occur.
    pub fn new(
        node_uri: RelativeUri,
        stop_rx: trigger::Receiver,
        config: MetricAggregatorConfig,
        lanes: PulseLanes,
        log_context: NodeLogger,
    ) -> (
        NodeMetricAggregator,
        impl Future<Output = Result<(), AggregatorError>>,
    ) {
        let MetricAggregatorConfig {
            sample_rate,
            buffer_size,
            yield_after,
            backpressure_config,
        } = config;

        let PulseLanes {
            uplinks,
            lanes,
            node,
        } = lanes;

        let (node_tx, node_rx) = mpsc::channel(buffer_size.get());
        let node_aggregator = NodeAggregatorTask::new(
            stop_rx.clone(),
            sample_rate,
            node,
            ReceiverStream::new(node_rx),
        );

        let (lane_tx, lane_rx) = mpsc::channel(buffer_size.get());
        let lane_pulse_lanes = lanes
            .into_iter()
            .map(|(k, v)| {
                let inner = MetricState::new(LaneMetricReporter::default(), v);
                (k, inner)
            })
            .collect();

        let lane_aggregator = AggregatorTask::new(
            lane_pulse_lanes,
            sample_rate,
            stop_rx.clone(),
            ReceiverStream::new(lane_rx),
            node_tx,
        );

        let (uplink_task, uplink_tx) = uplink_aggregator(
            stop_rx,
            sample_rate,
            buffer_size,
            yield_after,
            backpressure_config,
            uplinks,
            lane_tx,
        );

        let task_node_uri = node_uri.clone();

        let task = async move {
            let result = try_join3(
                node_aggregator.run(yield_after),
                lane_aggregator.run(yield_after),
                uplink_task,
            )
            .instrument(span!(Level::DEBUG, AGGREGATOR_TASK, ?task_node_uri))
            .await;

            match &result {
                Ok((node, lane, uplink)) => {
                    let stages = vec![node, lane, uplink];
                    for state in stages {
                        let entry = LogEntry::make(
                            LOG_TASK_FINISHED_MSG.to_string(),
                            LogLevel::Debug,
                            task_node_uri.clone(),
                            state.to_string().to_lowercase(),
                        );
                        let _res = log_context.log_entry(entry).await;
                    }
                }
                Err(e) => {
                    let message = format!("{}: {}", LOG_ERROR_MSG, e);
                    let entry = LogEntry::make(
                        message,
                        LogLevel::Error,
                        task_node_uri,
                        e.aggregator.to_string().to_lowercase(),
                    );

                    let _res = log_context.log_entry(entry).await;
                }
            };

            result.map(|_| ())
        };

        let metrics = NodeMetricAggregator {
            sample_rate: config.sample_rate,
            node_uri: node_uri.to_string(),
            metric_tx: uplink_tx,
        };
        (metrics, task)
    }

    /// Returns a new event and action observer pair for the provided `lane_uri`.
    pub fn uplink_observer(&self, lane_uri: String) -> (UplinkEventObserver, UplinkActionObserver) {
        let NodeMetricAggregator {
            sample_rate,
            node_uri,
            metric_tx,
        } = self;
        let profile_sender =
            UplinkProfileSender::new(RelativePath::new(node_uri, lane_uri), metric_tx.clone());

        uplink_observer(*sample_rate, profile_sender)
    }

    pub fn uplink_observer_for_path(
        &self,
        uri: RelativePath,
    ) -> (UplinkEventObserver, UplinkActionObserver) {
        let NodeMetricAggregator {
            sample_rate,
            metric_tx,
            ..
        } = self;
        let profile_sender = UplinkProfileSender::new(uri, metric_tx.clone());

        uplink_observer(*sample_rate, profile_sender)
    }
}
