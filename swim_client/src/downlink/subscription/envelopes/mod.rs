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

use crate::downlink::model::map::UntypedMapModification;
use crate::downlink::model::value::SharedValue;
use crate::downlink::Command;
use swim_common::warp::envelope::{OutgoingHeader, OutgoingLinkMessage};
use swim_model::path::AbsolutePath;
use swim_model::Value;

#[cfg(test)]
mod tests;

/// Convert a downlink [`Command`] into a Warp [`OutgoingLinkMessage`].
fn envelope_for<T, F>(
    to_body: F,
    path: &AbsolutePath,
    command: Command<T>,
) -> (url::Url, OutgoingLinkMessage)
where
    F: Fn(T) -> Value,
{
    let (host, path) = path.clone().split();
    (
        host,
        match command {
            Command::Sync => OutgoingLinkMessage {
                header: OutgoingHeader::Sync(Default::default()),
                path,
                body: None,
            },
            Command::Link => OutgoingLinkMessage {
                header: OutgoingHeader::Link(Default::default()),
                path,
                body: None,
            },
            Command::Action(v) => OutgoingLinkMessage {
                header: OutgoingHeader::Command,
                path,
                body: Some(to_body(v)),
            },
            Command::Unlink => OutgoingLinkMessage {
                header: OutgoingHeader::Unlink,
                path,
                body: None,
            },
        },
    )
}

/// Convert a downlink [`Command`], from a value downlink, into a Warp [`OutgoingLinkMessage`].
pub fn value_envelope(
    path: &AbsolutePath,
    command: Command<SharedValue>,
) -> (url::Url, OutgoingLinkMessage) {
    envelope_for(value::envelope_body, path, command)
}

/// Convert a downlink [`Command`], from a map downlink, into a Warp [`OutgoingLinkMessage`].
pub fn map_envelope(
    path: &AbsolutePath,
    command: Command<UntypedMapModification<Value>>,
) -> (url::Url, OutgoingLinkMessage) {
    envelope_for(map::envelope_body, path, command)
}

/// Convert a downlink [`Command`], from a command downkink, into a Warp [`OutgoingLinkMessage`].
pub fn command_envelope(
    path: &AbsolutePath,
    command: Command<Value>,
) -> (url::Url, OutgoingLinkMessage) {
    envelope_for(|v| v, path, command)
}

/// Convert a downlink [`Command`], from a event downlink, into a Warp [`OutgoingLinkMessage`].
pub fn dummy_envelope(
    path: &AbsolutePath,
    command: Command<()>,
) -> (url::Url, OutgoingLinkMessage) {
    envelope_for(|_| Value::Extant, path, command)
}

pub(in crate::downlink) mod value {
    use crate::downlink::model::value::SharedValue;
    use crate::downlink::Message;
    use swim_common::warp::envelope::{IncomingHeader, IncomingLinkMessage};
    use swim_model::Value;

    pub(in crate::downlink) fn envelope_body(v: SharedValue) -> Value {
        (*v).clone()
    }

    pub(in crate::downlink) fn from_envelope(incoming: IncomingLinkMessage) -> Message<Value> {
        match incoming {
            IncomingLinkMessage {
                header: IncomingHeader::Linked(_),
                ..
            } => Message::Linked,
            IncomingLinkMessage {
                header: IncomingHeader::Synced,
                ..
            } => Message::Synced,
            IncomingLinkMessage {
                header: IncomingHeader::Unlinked,
                ..
            } => Message::Unlinked,
            IncomingLinkMessage {
                header: IncomingHeader::Event,
                body: Some(body),
                ..
            } => Message::Action(body),
            _ => Message::Action(Value::Extant),
        }
    }
}

pub(in crate::downlink) mod map {
    use crate::downlink::model::map::UntypedMapModification;
    use crate::downlink::Message;
    use swim_common::warp::envelope::{IncomingHeader, IncomingLinkMessage};
    use swim_form::Form;
    use swim_model::Value;
    use tracing::warn;

    pub(super) fn envelope_body(cmd: UntypedMapModification<Value>) -> Value {
        Form::into_value(cmd)
    }

    pub(in crate::downlink) fn from_envelope(
        incoming: IncomingLinkMessage,
    ) -> Message<UntypedMapModification<Value>> {
        match incoming {
            IncomingLinkMessage {
                header: IncomingHeader::Linked(_),
                ..
            } => Message::Linked,
            IncomingLinkMessage {
                header: IncomingHeader::Synced,
                ..
            } => Message::Synced,
            IncomingLinkMessage {
                header: IncomingHeader::Unlinked,
                ..
            } => Message::Unlinked,
            IncomingLinkMessage {
                header: IncomingHeader::Event,
                body: Some(body),
                ..
            } => match Form::try_convert(body) {
                Ok(modification) => Message::Action(modification),
                Err(e) => Message::BadEnvelope(format!("{}", e)),
            },
            _ => {
                warn!("Bad envelope: {:?}", incoming);
                Message::BadEnvelope("Event envelope had no body.".to_string())
            }
        }
    }
}
