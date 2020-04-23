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

use std::pin::Pin;

use futures::ready;
use futures::task::{Context, Poll};
use futures::{Future, FutureExt};
use tokio::time;

use pin_project::{pin_project, project};

use crate::future::retryable::strategy::RetryStrategy;

#[cfg(test)]
mod tests;

pub mod strategy;

#[pin_project]
pub struct Retry<F: RetryableFuture> {
    f: F,
    #[pin]
    state: RetryState<F>,
    ctx: RetryContext<F::Err>,
    strategy: RetryStrategy,
}

#[pin_project]
enum RetryState<F>
where
    F: RetryableFuture,
{
    NotStarted,
    Pending(#[pin] F::Future),
    Retrying(F::Err),
    Sleeping(time::Delay),
}

pub struct RetryContext<Err> {
    last_err: Option<Err>,
    #[allow(dead_code)]
    retries: usize,
}

impl<F> Retry<F>
where
    F: RetryableFuture,
{
    pub fn new(f: F, strategy: RetryStrategy) -> Retry<F> {
        Retry {
            f,
            state: RetryState::NotStarted,
            ctx: RetryContext {
                last_err: None,
                retries: 0,
            },
            strategy,
        }
    }
}

impl<F> Future for Retry<F>
where
    F: RetryableFuture,
{
    type Output = Result<F::Ok, F::Err>;

    #[project]
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.as_mut().project();
        loop {
            #[project]
            let next_state = match this.state.as_mut().project() {
                RetryState::NotStarted => {
                    let fut = this.f.future(this.ctx);
                    RetryState::Pending(fut)
                }
                RetryState::Pending(fut) => match ready!(fut.poll(cx)) {
                    Ok(r) => {
                        return Poll::Ready(Ok(r));
                    }
                    Err(e) => {
                        if this.f.retry(this.ctx) {
                            RetryState::Retrying(e)
                        } else {
                            return Poll::Ready(Err(e));
                        }
                    }
                },
                RetryState::Retrying(e) => match this.strategy.next() {
                    Some(duration) => {
                        this.ctx.last_err = Some(*e);

                        match duration {
                            Some(duration) => RetryState::Sleeping(time::delay_for(duration)),
                            None => RetryState::NotStarted,
                        }
                    }
                    None => {
                        return Poll::Ready(Err(*e));
                    }
                },
                RetryState::Sleeping(timer) => {
                    ready!(timer.poll_unpin(cx));
                    RetryState::NotStarted
                }
            };

            this.state.set(next_state);
        }
    }
}

pub trait RetryableFuture: Unpin + Sized {
    type Ok;
    type Err: Copy;
    type Future: Future<Output = Result<Self::Ok, Self::Err>> + Send + Unpin + 'static;

    fn future(&mut self, ctx: &mut RetryContext<Self::Err>) -> Self::Future;

    fn retry(&mut self, ctx: &mut RetryContext<Self::Err>) -> bool;
}
