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

use crate::extensions::{Extension, ExtensionProvider};
use crate::Request;
use httparse::Response;
use thiserror::Error;

pub struct DeflateHandshake;

#[derive(Error, Debug)]
#[error("Err")]
pub struct DeflateError;

impl ExtensionProvider for DeflateHandshake {
    type Extension = Deflate;
    type Error = DeflateError;

    fn apply_headers(&self, _request: &mut Request) {
        todo!()
    }

    fn negotiate(&self, _response: &Response) -> Result<Self::Extension, Self::Error> {
        Ok(Deflate)
    }
}

#[derive(Debug, Clone)]
pub struct Deflate;
impl Extension for Deflate {
    fn encode(&mut self) {
        todo!()
    }

    fn decode(&mut self) {
        todo!()
    }
}
