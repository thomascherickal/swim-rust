// Copyright 2015-2020 SWIM.AI inc.
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
use swim_common::form::FormErr;

pub mod channels;
pub mod lifecycle;
pub mod model;
pub mod strategy;

#[cfg(test)]
pub mod tests;

/// Trait for any lane that can belong to a [`SwimAgent`].
pub trait LaneModel {
    /// The type of events generated by the lane.
    type Event;

    /// Determine if two lane instances are the same lane.
    fn same_lane(this: &Self, other: &Self) -> bool;
}

/// An error generated by an lane when a [`Form`] implementation is found to be inconsistent.
/// Particularly, converting from the type type to [`Value`] and back again results in an
/// error.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InvalidForm(FormErr);

impl Display for InvalidForm {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Lane form implementation is inconsistent: {}", self.0)
    }
}

impl Error for InvalidForm {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.0)
    }
}
