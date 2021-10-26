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
use std::collections::HashMap;
use std::num::NonZeroUsize;
use swim_common::form::structural::read::ReadError;
use swim_common::model::parser::ParseFailure;
use swim_common::routing::remote::config::RemoteConnectionsConfig;
use swim_common::warp::path::{AbsolutePath, Addressable};
use swim_utilities::future::retryable::strategy::RetryStrategy;
use thiserror::Error;
use tokio::time::Duration;
use tokio_tungstenite::tungstenite::extensions::compression::WsCompression;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use url::Url;

mod recognizers;
mod tags;
#[cfg(test)]
mod tests;
mod writers;

const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(60000);
const DEFAULT_DOWNLINK_BUFFER_SIZE: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(32) };
const DEFAULT_YIELD_AFTER: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(256) };
const DEFAULT_BUFFER_SIZE: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(128) };
const DEFAULT_DL_REQUEST_BUFFER_SIZE: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(8) };
const DEFAULT_BACK_PRESSURE_INPUT_BUFFER_SIZE: NonZeroUsize =
    unsafe { NonZeroUsize::new_unchecked(32) };
const DEFAULT_BACK_PRESSURE_BRIDGE_BUFFER_SIZE: NonZeroUsize =
    unsafe { NonZeroUsize::new_unchecked(16) };
const DEFAULT_BACK_PRESSURE_MAX_ACTIVE_KEYS: NonZeroUsize =
    unsafe { NonZeroUsize::new_unchecked(16) };
const DEFAULT_BACK_PRESSURE_YIELD_AFTER: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(256) };

/// Configuration for the swim client.
///
/// * `downlink_connections_config` - Configuration parameters for the downlink connections.
/// * `remote_connections_config` - Configuration parameters the remote connections.
/// * `websocket_config` - Configuration parameters the WebSocket connections.
/// * `downlinks_config` - Configuration for the behaviour of downlinks.
#[derive(Clone, Debug, Default)]
pub struct SwimClientConfig {
    pub downlink_connections_config: DownlinkConnectionsConfig,
    pub remote_connections_config: RemoteConnectionsConfig,
    pub websocket_config: WebSocketConfig,
    pub downlinks_config: ClientDownlinksConfig,
}

impl PartialEq<Self> for SwimClientConfig {
    fn eq(&self, other: &Self) -> bool {
        self.downlink_connections_config == other.downlink_connections_config
            && self.remote_connections_config == other.remote_connections_config
            && self.websocket_config.max_send_queue == other.websocket_config.max_send_queue
            && self.websocket_config.max_message_size == other.websocket_config.max_message_size
            && self.websocket_config.max_frame_size == other.websocket_config.max_frame_size
            && self.websocket_config.accept_unmasked_frames
                == other.websocket_config.accept_unmasked_frames
            && match (
                self.websocket_config.compression,
                other.websocket_config.compression,
            ) {
                (WsCompression::None(self_val), WsCompression::None(other_val)) => {
                    self_val == other_val
                }
                (WsCompression::Deflate(self_deflate), WsCompression::Deflate(other_deflate)) => {
                    self_deflate == other_deflate
                }
                _ => false,
            }
            && self.downlinks_config == other.downlinks_config
    }
}

impl SwimClientConfig {
    pub fn new(
        downlink_connections_config: DownlinkConnectionsConfig,
        remote_connections_config: RemoteConnectionsConfig,
        websocket_config: WebSocketConfig,
        downlinks_config: ClientDownlinksConfig,
    ) -> SwimClientConfig {
        SwimClientConfig {
            downlink_connections_config,
            remote_connections_config,
            websocket_config,
            downlinks_config,
        }
    }
}

/// Configuration parameters for the router.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DownlinkConnectionsConfig {
    /// Buffer size for servicing requests for new downlinks.
    pub dl_req_buffer_size: NonZeroUsize,
    /// Size of the internal buffers of the downlinks connections task.
    pub buffer_size: NonZeroUsize,
    /// Number of values to process before yielding to the runtime.
    pub yield_after: NonZeroUsize,
    /// The retry strategy that will be used when attempting to make a request to a Web Agent.
    pub retry_strategy: RetryStrategy,
}

impl DownlinkConnectionsConfig {
    pub fn new(
        dl_req_buffer_size: NonZeroUsize,
        buffer_size: NonZeroUsize,
        yield_after: NonZeroUsize,
        retry_strategy: RetryStrategy,
    ) -> DownlinkConnectionsConfig {
        DownlinkConnectionsConfig {
            dl_req_buffer_size,
            buffer_size,
            yield_after,
            retry_strategy,
        }
    }
}

impl Default for DownlinkConnectionsConfig {
    fn default() -> Self {
        DownlinkConnectionsConfig {
            dl_req_buffer_size: DEFAULT_DL_REQUEST_BUFFER_SIZE,
            retry_strategy: RetryStrategy::default(),
            buffer_size: DEFAULT_BUFFER_SIZE,
            yield_after: DEFAULT_YIELD_AFTER,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ClientDownlinksConfig {
    default: DownlinkConfig,
    by_host: HashMap<Url, DownlinkConfig>,
    by_lane: HashMap<AbsolutePath, DownlinkConfig>,
}

impl ClientDownlinksConfig {
    pub fn new(default: DownlinkConfig) -> ClientDownlinksConfig {
        ClientDownlinksConfig {
            default,
            by_host: HashMap::new(),
            by_lane: HashMap::new(),
        }
    }
}

impl DownlinksConfig for ClientDownlinksConfig {
    type PathType = AbsolutePath;

    fn config_for(&self, path: &Self::PathType) -> DownlinkConfig {
        let ClientDownlinksConfig {
            default,
            by_host,
            by_lane,
            ..
        } = self;
        match by_lane.get(path) {
            Some(config) => *config,
            _ => {
                let maybe_host = path.host();

                match maybe_host {
                    Some(host) => match by_host.get(&host) {
                        Some(config) => *config,
                        _ => *default,
                    },
                    None => *default,
                }
            }
        }
    }

    fn for_host(&mut self, host: Url, params: DownlinkConfig) {
        self.by_host.insert(host, params);
    }

    fn for_lane(&mut self, lane: &AbsolutePath, params: DownlinkConfig) {
        self.by_lane.insert(lane.clone(), params);
    }
}

/// Configuration parameters for a single downlink.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DownlinkConfig {
    /// Whether the downlink propagates back-pressure.
    pub back_pressure: BackpressureMode,
    /// Timeout after which an idle downlink will be closed.
    /// Todo #412 (not yet implemented).
    pub idle_timeout: Duration,
    /// Buffer size for local actions performed on the downlink.
    pub buffer_size: NonZeroUsize,
    /// What do do on receipt of an invalid message.
    pub on_invalid: OnInvalidMessage,
    /// Number of operations after which a downlink will yield to the runtime.
    pub yield_after: NonZeroUsize,
}

impl DownlinkConfig {
    pub fn new(
        back_pressure: BackpressureMode,
        idle_timeout: Duration,
        buffer_size: NonZeroUsize,
        on_invalid: OnInvalidMessage,
        yield_after: NonZeroUsize,
    ) -> DownlinkConfig {
        DownlinkConfig {
            back_pressure,
            idle_timeout,
            buffer_size,
            on_invalid,
            yield_after,
        }
    }
}

impl From<&DownlinkConfig> for DownlinkConfig {
    fn from(conf: &DownlinkConfig) -> Self {
        *conf
    }
}

impl Default for DownlinkConfig {
    fn default() -> Self {
        DownlinkConfig::new(
            BackpressureMode::default(),
            DEFAULT_IDLE_TIMEOUT,
            DEFAULT_DOWNLINK_BUFFER_SIZE,
            OnInvalidMessage::default(),
            DEFAULT_YIELD_AFTER,
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
/// Mode indicating whether or not the downlink propagates back-pressure.
pub enum BackpressureMode {
    /// Propagate back-pressure through the downlink.
    Propagate,
    /// Attempt to relieve back-pressure through the downlink as much as possible.
    Release {
        /// Input queue size for the back-pressure relief component.
        input_buffer_size: NonZeroUsize,
        /// Queue size for control messages between different components of the pressure
        /// relief component. This only applies to map downlinks.
        bridge_buffer_size: NonZeroUsize,
        /// Maximum number of active keys in the pressure relief component for map downlinks.
        max_active_keys: NonZeroUsize,
        /// Number of values to process before yielding to the runtime.
        yield_after: NonZeroUsize,
    },
}

impl Default for BackpressureMode {
    fn default() -> Self {
        BackpressureMode::Propagate
    }
}

/// Instruction on how to respond when an invalid message is received for a downlink.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnInvalidMessage {
    /// Disregard the message and continue.
    Ignore,
    /// Terminate the downlink.
    Terminate,
}

impl Default for OnInvalidMessage {
    fn default() -> Self {
        OnInvalidMessage::Terminate
    }
}

/// Configuration for the creation and management of downlinks for a Warp client.
pub trait DownlinksConfig: Send + Sync {
    type PathType: Addressable;

    /// Get the downlink configuration for a downlink for a specific path.
    fn config_for(&self, path: &Self::PathType) -> DownlinkConfig;

    /// Add specific configuration for a host.
    fn for_host(&mut self, host: Url, params: DownlinkConfig);

    /// Add specific configuration for an absolute path (this will override host level
    /// configuration).
    fn for_lane(&mut self, lane: &Self::PathType, params: DownlinkConfig);
}

impl<'a, Path: Addressable> DownlinksConfig for Box<dyn DownlinksConfig<PathType = Path> + 'a> {
    type PathType = Path;

    fn config_for(&self, path: &Self::PathType) -> DownlinkConfig {
        (**self).config_for(path)
    }

    fn for_host(&mut self, host: Url, params: DownlinkConfig) {
        (**self).for_host(host, params)
    }

    fn for_lane(&mut self, lane: &Path, params: DownlinkConfig) {
        (**self).for_lane(lane, params)
    }
}

#[derive(Debug, Error)]
#[error("Could not process client configuration: {0}")]
pub enum ConfigError {
    File(std::io::Error),
    Parse(ParseFailure),
    Recognizer(ReadError),
}
