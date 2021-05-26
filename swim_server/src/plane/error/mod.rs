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

use std::error::Error;
use std::fmt::{Display, Formatter};
use utilities::route_pattern::RoutePattern;

#[cfg(test)]
mod tests;

/// Indicates that ambiguous routes were specified when defining a plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousRoutes(RoutePattern, RoutePattern);

impl AmbiguousRoutes {
    pub fn new(first: RoutePattern, second: RoutePattern) -> Self {
        AmbiguousRoutes(first, second)
    }
}

impl Display for AmbiguousRoutes {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Routes '{}' and '{}' are ambiguous.", &self.0, &self.1)
    }
}

impl Error for AmbiguousRoutes {}
