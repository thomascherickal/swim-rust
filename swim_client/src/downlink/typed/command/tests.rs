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

use crate::configuration::OnInvalidMessage;
use crate::downlink::typed::command::{CommandViewError, TypedCommandDownlink};
use crate::downlink::Command;
use crate::downlink::DownlinkConfig;
use std::num::NonZeroUsize;
use std::sync::Arc;
use swim_common::form::ValidatedForm;
use swim_common::model::schema::StandardSchema;
use swim_common::model::Value;
use swim_common::sink::item::ItemSender;
use tokio::sync::mpsc;

struct Components<T> {
    downlink: TypedCommandDownlink<T>,
    command_rx: mpsc::Receiver<Command<Value>>,
}

fn make_command_downlink<T: ValidatedForm>() -> Components<T> {
    let (command_tx, command_rx) = mpsc::channel(8);
    let sender = swim_common::sink::item::for_mpsc_sender(command_tx).map_err_into();

    let dl = crate::downlink::command_downlink(
        T::schema(),
        sender,
        DownlinkConfig {
            buffer_size: NonZeroUsize::new(8).unwrap(),
            yield_after: NonZeroUsize::new(2048).unwrap(),
            on_invalid: OnInvalidMessage::Terminate,
        },
    );
    let downlink = TypedCommandDownlink::new(Arc::new(dl));

    Components {
        downlink,
        command_rx,
    }
}

#[tokio::test]
async fn sender_contravariant_view() {
    let Components {
        downlink,
        command_rx: _command_rx,
    } = make_command_downlink::<i32>();

    assert!(downlink.contravariant_view::<i64>().is_ok());
    assert!(downlink.contravariant_view::<i32>().is_ok());
    assert!(downlink.contravariant_view::<String>().is_err());
}

#[test]
fn command_view_error_display() {
    let err = CommandViewError {
        existing: StandardSchema::Nothing,
        requested: StandardSchema::Anything,
    };
    let str = err.to_string();

    assert_eq!(str, format!("A Write Only view of a command downlink with schema {} was requested but the original command downlink is running with schema {}.", StandardSchema::Anything, StandardSchema::Nothing));
}
