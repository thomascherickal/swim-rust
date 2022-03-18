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

use futures::future::BoxFuture;
use swim_utilities::{io::byte_channel::{ByteReader, ByteWriter}, routing::uri::RelativeUri};

use crate::{error::{AgentRuntimeError, AgentTaskError, AgentInitError}, downlink::{DownlinkConfig, Downlink}};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UplinkKind {
    Value,
    Map
}

pub trait AgentContext {

    fn add_lane(
        &self,
        name: &str,
        uplink_kind: UplinkKind,
    ) -> BoxFuture<'static, Result<(ByteReader, ByteWriter), AgentRuntimeError>>;

    fn open_downlink(
        &self,
        config: DownlinkConfig,
        downlink: Box<dyn Downlink>) ->  BoxFuture<'static, Result<(), AgentRuntimeError>>;

}

#[derive(Debug, Clone, Copy)]
pub struct AgentConfig {
    //TODO Add parameters.
}

pub type AgentTask<'a> = BoxFuture<'a, Result<(), AgentTaskError>>;
pub type AgentInitResult<'a> = Result<AgentTask<'a>, AgentInitError>;

pub trait Agent {

    fn run<'a>(&self,
        route: RelativeUri,
        config: AgentConfig,
        context: &'a dyn AgentContext,
        ) -> BoxFuture<'a, AgentInitResult<'a>>;

}

static_assertions::assert_obj_safe!(AgentContext, Agent);