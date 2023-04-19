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
use swim::{
    agent::agent_model::AgentModel,
    route::RoutePattern,
    server::{Server, ServerBuilder},
};

use crate::{unit_agent::{ExampleLifecycle, UnitAgent}, ui::run_web_server};

mod unit_agent;
mod ui;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    example_logging()?;

    let route = RoutePattern::parse_str("/example/:name}")?;

    let lifecycle_fn = || ExampleLifecycle::new(200).into_lifecycle();
    let agent = AgentModel::from_fn(UnitAgent::default, lifecycle_fn);

    let server = ServerBuilder::with_plane_name("Example Plane")
        .add_route(route, agent)
        .update_config(|config| {
            config.agent_runtime.inactive_timeout = Duration::from_secs(5 * 60);
        })
        .build()
        .await?;

    let (task, handle) = server.run();

    let shutdown = manage_handle(handle);

    let ui = run_web_server(shutdown);

    let (ui_result, result) = tokio::join!(ui, task);

    result?;
    ui_result?;
    println!("Server stopped successfully.");
    Ok(())
}
