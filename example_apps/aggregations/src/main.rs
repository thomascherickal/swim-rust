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

use std::error::Error;
use std::str::FromStr;
use std::time::Duration;

use crate::{
    aggregate::{AggregateAgent, AggregateLifecycle},
    area::{AreaAgent, AreaLifecycle},
    car::CarAgent,
    car::CarLifecycle,
};
use example_util::{example_logging, manage_handle};
use swimos::route::RouteUri;
use swimos::{
    agent::agent_model::AgentModel,
    route::RoutePattern,
    server::{Server, ServerBuilder},
};

mod aggregate;
mod area;
mod car;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    example_logging()?;

    let car_agent = AgentModel::new(CarAgent::default, CarLifecycle::default().into_lifecycle());
    let area_agent = AgentModel::new(
        AreaAgent::default,
        AreaLifecycle::default().into_lifecycle(),
    );
    let aggregate_agent = AgentModel::new(
        AggregateAgent::default,
        AggregateLifecycle::default().into_lifecycle(),
    );

    let server = ServerBuilder::with_plane_name("Example Plane")
        .add_route(RoutePattern::parse_str("/cars/:car_id")?, car_agent)
        .add_route(RoutePattern::parse_str("/area/:area")?, area_agent)
        .add_route(RoutePattern::parse_str("/aggregate")?, aggregate_agent)
        .update_config(|config| {
            config.agent_runtime.inactive_timeout = Duration::from_secs(5 * 60);
        })
        .build()
        .await?;

    let (task, handle) = server.run();
    let _task = tokio::spawn(task);

    for i in 0..1000 {
        let route = format!("/cars/{i}");
        handle
            .start_agent(RouteUri::from_str(route.as_str())?)
            .await
            .expect("Failed to start agent");
    }

    manage_handle(handle).await;
    println!("Server stopped successfully.");

    Ok(())
}
