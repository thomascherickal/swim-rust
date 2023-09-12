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

use std::{error::Error, time::Duration};

use example_util::{example_logging, manage_handle};
use swim::{server::{Server, ServerBuilder}, route::RoutePattern, agent::agent_model::AgentModel};

use crate::{buses_api::BusesApi, agents::{AgencyAgentLifecycle, AgencyAgent}};

mod agents;
mod buses_api;
mod model;

const POLL_DELAY: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    example_logging()?;

    let api = BusesApi::default();

    let agencies = model::agency::agencies();
    let mut builder = ServerBuilder::with_plane_name("Transit Plane");

    for agency in agencies {
        let uri = agency.uri();
        let route = RoutePattern::parse_str(&uri)?;
        let lifecycle = AgencyAgentLifecycle::new(api.clone(), agency, POLL_DELAY);
        let agent = AgentModel::new(AgencyAgent::default, lifecycle.into_lifecycle());
        builder = builder.add_route(route, agent);
    }

    let server = builder
        .update_config(|config| {
            config.agent_runtime.inactive_timeout = Duration::from_secs(5 * 60);
        })
        .build()
        .await?;

    let (task, handle) = server.run();

    let shutdown = manage_handle(handle);

    let (_, result) = tokio::join!(shutdown, task);

    result?;
    println!("Server stopped successfully.");
    Ok(())
}
