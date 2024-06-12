// Copyright 2015-2023 Swim Inc.
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

use swimos_form::Form;
use swimos_model::Text;

use swimos_api::agent::LaneKind;

/// Lane information metadata that can be retrieved when syncing to
/// `/swimos:meta:node/percent-encoded-nodeuri/lanes`.
///
/// E.g: `swimos:meta:node/unit%2Ffoo/lanes/`
#[derive(Debug, Clone, PartialEq, Eq, Form)]
pub struct LaneInfo {
    /// The URI of the lane.
    #[form(name = "laneUri")]
    pub lane_uri: Text,
    /// The type of the lane.
    #[form(name = "laneType")]
    pub lane_type: LaneKind,
}

impl LaneInfo {
    pub fn new<L>(lane_uri: L, lane_type: LaneKind) -> Self
    where
        L: Into<Text>,
    {
        LaneInfo {
            lane_uri: lane_uri.into(),
            lane_type,
        }
    }
}
