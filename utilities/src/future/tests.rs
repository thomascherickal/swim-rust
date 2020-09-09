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

use super::{SwimFutureExt, SwimStreamExt, SwimTryFutureExt, TransformMut};
use crate::sync::trigger;
use futures::executor::block_on;
use futures::future::{ready, Ready};
use futures::stream::iter;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::{mpsc, Barrier};

#[test]
fn future_into() {
    let fut = ready(4);
    let n: i64 = block_on(fut.output_into());
    assert_eq!(n, 4);
}

#[test]
fn ok_into_ok_case() {
    let fut: Ready<Result<i32, String>> = ready(Ok(4));
    let r: Result<i64, String> = block_on(fut.ok_into());
    assert_eq!(r, Ok(4i64));
}

#[test]
fn ok_into_err_case() {
    let fut: Ready<Result<i32, String>> = ready(Err("hello".to_string()));
    let r: Result<i64, String> = block_on(fut.ok_into());
    assert_eq!(r, Err("hello".to_string()));
}

struct Plus(i32);

impl TransformMut<i32> for Plus {
    type Out = i32;

    fn transform(&mut self, input: i32) -> Self::Out {
        input + self.0
    }
}

#[test]
fn transform_future() {
    let fut = ready(2);
    let plus = Plus(3);
    let n = block_on(fut.transform(plus));
    assert_eq!(n, 5);
}

#[test]
fn transform_stream() {
    let inputs = iter((0..5).into_iter());

    let outputs: Vec<i32> = block_on(inputs.transform(Plus(3)).collect::<Vec<i32>>());

    assert_eq!(outputs, vec![3, 4, 5, 6, 7]);
}

struct PlusIfNonNeg(i32);

impl TransformMut<i32> for PlusIfNonNeg {
    type Out = Result<i32, ()>;

    fn transform(&mut self, input: i32) -> Self::Out {
        if input >= 0 {
            Ok(input + self.0)
        } else {
            Err(())
        }
    }
}

#[test]
fn until_failure() {
    let inputs = iter(vec![0, 1, 2, -3, 4].into_iter());
    let outputs: Vec<i32> = block_on(inputs.until_failure(PlusIfNonNeg(3)).collect::<Vec<i32>>());
    assert_eq!(outputs, vec![3, 4, 5]);
}

#[tokio::test(threaded_scheduler)]
async fn take_until() {
    let (mut tx, rx) = mpsc::channel(5);

    let (stop, stop_sig) = trigger::trigger();

    let barrier1 = Arc::new(Barrier::new(2));
    let barrier2 = barrier1.clone();

    let stream_task = swim_runtime::task::spawn(async move {
        let mut stream = rx.take_until_completes(stop_sig);
        assert_eq!(stream.next().await, Some(1));
        assert_eq!(stream.next().await, Some(2));
        assert_eq!(stream.next().await, Some(3));
        barrier1.wait().await;
        assert!(stream.next().await.is_none());
    });

    assert!(tx.send(1).await.is_ok());
    assert!(tx.send(2).await.is_ok());
    assert!(tx.send(3).await.is_ok());
    barrier2.wait().await;
    stop.trigger();
    //Further sends are not guaranteed to succeed but it doesn't matter if they fail.
    let _ = tx.send(4).await;
    let _ = tx.send(5).await;

    assert!(stream_task.await.is_ok());
}

#[tokio::test]
async fn unit_future() {
    let fut = async { 5 };

    assert_eq!(fut.unit().await, ());
}
