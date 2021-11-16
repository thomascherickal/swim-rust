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

use super::*;
use crate::downlink::Message;
use std::sync::Arc;

use swim_warp::envelope::ResponseEnvelope;

fn path() -> RelativePath {
    RelativePath::new("node", "lane")
}

#[test]
fn unlink_value_command_to_envelope() {
    let expected = RequestEnvelope::unlink("node", "lane");
    let envelope = value_envelope(path(), Command::Unlink);
    assert_eq!(envelope, expected);
}

#[test]
fn unlinked_value_message_from_envelope() {
    let env = ResponseEnvelope::unlinked("node", "lane");
    let result = value::from_envelope(env);
    assert_eq!(result, Message::Unlinked);
}

#[test]
fn sync_value_command_to_envelope() {
    let expected = RequestEnvelope::sync("node", "lane");
    let envelope = value_envelope(path(), Command::Sync);
    assert_eq!(envelope, expected);
}

#[test]
fn linked_value_message_from_envelope() {
    let env = ResponseEnvelope::linked("node", "lane");
    let result = value::from_envelope(env);
    assert_eq!(result, Message::Linked);
}

#[test]
fn synced_value_message_from_envelope() {
    let env = ResponseEnvelope::synced("node", "lane");
    let result = value::from_envelope(env);
    assert_eq!(result, Message::Synced);
}

#[test]
fn data_value_command_to_envelope() {
    let expected = RequestEnvelope::command("node", "lane", 5);
    let envelope = value_envelope(
        path(),
        Command::Action(SharedValue::new(Value::Int32Value(5))),
    );
    assert_eq!(envelope, expected);
}

#[test]
fn data_value_message_from_envelope() {
    let env = ResponseEnvelope::event("node", "lane", 7);
    let result = value::from_envelope(env);
    assert_eq!(result, Message::Action(Value::Int32Value(7)))
}

#[test]
fn unlink_map_command_to_envelope() {
    let expected = RequestEnvelope::unlink("node", "lane");
    let envelope = map_envelope(path(), Command::Unlink);
    assert_eq!(envelope, expected);
}

#[test]
fn sync_map_command_to_envelope() {
    let expected = RequestEnvelope::sync("node", "lane");
    let envelope = map_envelope(path(), Command::Sync);
    assert_eq!(envelope, expected);
}

#[test]
fn clear_map_command_to_envelope() {
    let action: UntypedMapModification<Value> = UntypedMapModification::Clear;

    let expected = RequestEnvelope::command("node", "lane", action);
    let envelope = map_envelope(path(), Command::Action(UntypedMapModification::Clear));
    assert_eq!(envelope, expected);
}

#[test]
fn take_map_command_to_envelope() {
    let action: UntypedMapModification<Value> = UntypedMapModification::Take(7);

    let expected = RequestEnvelope::command("node", "lane", action);
    let envelope = map_envelope(path(), Command::Action(UntypedMapModification::Take(7)));
    assert_eq!(envelope, expected);
}

#[test]
fn skip_map_command_to_envelope() {
    let action: UntypedMapModification<Value> = UntypedMapModification::Drop(7);

    let expected = RequestEnvelope::command("node", "lane", action);
    let envelope = map_envelope(path(), Command::Action(UntypedMapModification::Drop(7)));
    assert_eq!(envelope, expected);
}

#[test]
fn remove_map_command_to_envelope() {
    let action = UntypedMapModification::<Value>::Remove(Value::text("key"));

    let expected = RequestEnvelope::command("node", "lane", action);
    let envelope = map_envelope(
        path(),
        Command::Action(UntypedMapModification::Remove(Value::text("key"))),
    );
    assert_eq!(envelope, expected);
}

#[test]
fn insert_map_command_to_envelope() {
    let action = UntypedMapModification::Update(Value::text("key"), Arc::new(Value::text("value")));

    let expected = RequestEnvelope::command("node", "lane", action);

    let arc_action =
        UntypedMapModification::Update(Value::text("key"), Arc::new(Value::text("value")));

    let envelope = map_envelope(path(), Command::Action(arc_action));
    assert_eq!(envelope, expected);
}

#[test]
fn unlinked_map_message_from_envelope() {
    let env = ResponseEnvelope::unlinked("node", "lane");
    let result = map::from_envelope(env);
    assert_eq!(result, Message::Unlinked);
}

#[test]
fn linked_map_message_from_envelope() {
    let env = ResponseEnvelope::linked("node", "lane");
    let result = map::from_envelope(env);
    assert_eq!(result, Message::Linked);
}

#[test]
fn synced_map_message_from_envelope() {
    let env = ResponseEnvelope::synced("node", "lane");
    let result = map::from_envelope(env);
    assert_eq!(result, Message::Synced);
}

#[test]
fn clear_map_message_from_envelope() {
    let action: UntypedMapModification<Value> = UntypedMapModification::Clear;
    let env = ResponseEnvelope::event("node", "lane", action);
    let result = map::from_envelope(env);
    assert_eq!(result, Message::Action(UntypedMapModification::Clear))
}

#[test]
fn take_map_message_from_envelope() {
    let action: UntypedMapModification<Value> = UntypedMapModification::Take(14);
    let env = ResponseEnvelope::event("node", "lane", action);
    let result = map::from_envelope(env);
    assert_eq!(result, Message::Action(UntypedMapModification::Take(14)))
}

#[test]
fn skip_map_message_from_envelope() {
    let action: UntypedMapModification<Value> = UntypedMapModification::Drop(1);
    let env = ResponseEnvelope::event("node", "lane", action);
    let result = map::from_envelope(env);
    assert_eq!(result, Message::Action(UntypedMapModification::Drop(1)))
}

#[test]
fn remove_map_message_from_envelope() {
    let action = UntypedMapModification::Remove(Value::text("key"));

    let env = ResponseEnvelope::event("node", "lane", action.clone());
    let result = map::from_envelope(env);
    assert_eq!(result, Message::Action(action))
}

#[test]
fn insert_map_message_from_envelope() {
    let action = UntypedMapModification::Update(Value::text("key"), Arc::new(Value::text("value")));

    let env = ResponseEnvelope::event("node", "lane", action.clone());
    let result = map::from_envelope(env);
    assert_eq!(result, Message::Action(action))
}
