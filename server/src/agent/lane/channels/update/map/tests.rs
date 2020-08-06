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

use crate::agent::lane::channels::update::map::MapLaneUpdateTask;
use crate::agent::lane::model::map;
use crate::agent::lane::model::map::{MapLaneEvent, MapUpdate};
use crate::agent::lane::strategy::Queue;
use crate::agent::lane::tests::ExactlyOnce;
use futures::future::{join, ready};
use futures::stream::once;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn update_task_map_lane_update() {
    let (lane, mut events) = map::make_lane_model::<i32, i32, Queue>(Queue::default());

    let task = MapLaneUpdateTask::new(lane, || ExactlyOnce);

    let updates = once(ready(MapUpdate::Update(4, Arc::new(7))));

    let update_task = task.run(updates);
    let receive_task = timeout(Duration::from_secs(10), events.next());

    let (upd_result, rec_result) = join(update_task, receive_task).await;

    assert!(matches!(upd_result, Ok(())));
    assert!(matches!(rec_result, Ok(Some(MapLaneEvent::Update(4, v))) if *v == 7));
}

#[tokio::test]
async fn update_task_map_lane_remove() {
    let (lane, mut events) = map::make_lane_model::<i32, i32, Queue>(Queue::default());

    //Insert a record to remove and consume the generated event.
    assert!(lane
        .update_direct(4, Arc::new(7))
        .apply(ExactlyOnce)
        .await
        .is_ok());
    assert!(events.next().await.is_some());

    let task = MapLaneUpdateTask::new(lane, || ExactlyOnce);

    let updates = once(ready(MapUpdate::Remove(4)));

    let update_task = task.run(updates);
    let receive_task = timeout(Duration::from_secs(10), events.next());

    let (upd_result, rec_result) = join(update_task, receive_task).await;

    assert!(matches!(upd_result, Ok(())));
    assert!(matches!(rec_result, Ok(Some(MapLaneEvent::Remove(4)))));
}

#[tokio::test]
async fn update_task_map_lane_clear() {
    let (lane, mut events) = map::make_lane_model::<i32, i32, Queue>(Queue::default());

    //Insert a record to remove and consume the generated event.
    assert!(lane
        .update_direct(4, Arc::new(7))
        .apply(ExactlyOnce)
        .await
        .is_ok());
    assert!(events.next().await.is_some());

    let task = MapLaneUpdateTask::new(lane, || ExactlyOnce);

    let updates = once(ready(MapUpdate::Clear));

    let update_task = task.run(updates);
    let receive_task = timeout(Duration::from_secs(10), events.next());

    let (upd_result, rec_result) = join(update_task, receive_task).await;

    assert!(matches!(upd_result, Ok(())));
    assert!(matches!(rec_result, Ok(Some(MapLaneEvent::Clear))));
}
