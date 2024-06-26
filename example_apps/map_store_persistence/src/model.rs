// Copyright 2015-2024 Swim Inc.
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

#[derive(Clone, Form, Debug)]
pub enum Instruction {
    #[form(tag = "wake")]
    Wake,
    #[form(tag = "set_value")]
    SetValue {
        #[form(header_body)]
        key: String,
        #[form(body)]
        value: i32,
    },
    #[form(tag = "set_temp")]
    SetTemp {
        #[form(header_body)]
        key: String,
        #[form(body)]
        value: i32,
    },
    #[form(tag = "stop")]
    Stop,
}
