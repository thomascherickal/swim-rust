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

use std::{
    error::Error,
    fmt::{Display, Formatter},
};

use swim_utilities::{format::comma_sep, routing::route_pattern::RoutePattern};

#[derive(Debug)]
pub struct AmbiguousRoutes {
    routes: Vec<RoutePattern>,
}

impl AmbiguousRoutes {
    pub fn new(routes: Vec<RoutePattern>) -> Self {
        AmbiguousRoutes { routes }
    }
}

impl Display for AmbiguousRoutes {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Agent route patterns are ambiguous: [{}]",
            comma_sep(&self.routes)
        )
    }
}

impl Error for AmbiguousRoutes {}
