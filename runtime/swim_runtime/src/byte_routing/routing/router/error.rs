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

use crate::error::ResolutionError;
use std::error::Error;

#[derive(Debug)]
pub struct RouterError {
    kind: RouterErrorKind,
    cause: Option<Box<dyn Error + Send + Sync + 'static>>,
}

impl RouterError {
    pub fn new(kind: RouterErrorKind) -> RouterError {
        RouterError { kind, cause: None }
    }

    pub fn with_cause<E>(kind: RouterErrorKind, cause: E) -> RouterError
    where
        E: Error + Send + Sync + 'static,
    {
        RouterError {
            kind,
            cause: Some(Box::new(cause)),
        }
    }

    pub fn is_resolution(&self) -> bool {
        matches!(
            self.kind,
            RouterErrorKind::NoAgentAtRoute | RouterErrorKind::Resolution
        )
    }
}

#[derive(Debug)]
pub enum RouterErrorKind {
    NoAgentAtRoute,
    ConnectionFailure,
    RouterDropped,
    Resolution,
}

impl From<ResolutionError> for RouterError {
    fn from(e: ResolutionError) -> Self {
        RouterError::with_cause(RouterErrorKind::Resolution, e)
    }
}
