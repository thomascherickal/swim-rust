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

use common::model::{Attr, Item, Value};
use form_derive::*;
use swim_form::_deserialize::FormDeserializeErr;
use swim_form::*;

fn main() {
    #[form(Value)]
    #[derive(PartialEq, Debug)]
    struct Parent {
        #[form(bigint)]
        a: BigInt,
        #[form(biguint)]
        b: BigUint,
    }

    let record = Value::Record(
        vec![Attr::from("Parent")],
        vec![
            Item::from(("a", Value::BooleanValue(true))),
            Item::from(("b", Value::Extant)),
        ],
    );

    let result = Parent::try_from_value(&record);

    assert_eq!(
        result,
        Err(FormDeserializeErr::Message(String::from(
            "invalid type: boolean `true`, expected a valid Big Integer"
        )))
    )
}
