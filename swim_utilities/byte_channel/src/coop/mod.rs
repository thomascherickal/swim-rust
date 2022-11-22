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

use std::{
    cell::Cell,
    num::NonZeroUsize,
    pin::Pin,
    task::{Context, Poll},
};

use futures::Future;
use pin_project::pin_project;

const DEFAULT_START_BUDGET: NonZeroUsize = unsafe { NonZeroUsize::new_unchecked(64) };

thread_local! {
    static TASK_BUDGET: Cell<Option<usize>> = Cell::new(None);
}

#[inline]
pub fn consume_budget(context: &mut Context<'_>) -> Poll<()> {
    TASK_BUDGET.with(|budget| match budget.get() {
        Some(mut b) => {
            b = b.saturating_sub(1);
            if b == 0 {
                budget.set(None);
                context.waker().wake_by_ref();
                Poll::Pending
            } else {
                budget.set(Some(b));
                Poll::Ready(())
            }
        }
        None => {
            budget.set(Some(DEFAULT_START_BUDGET.get()));
            Poll::Ready(())
        }
    })
}

#[inline]
pub fn track_progress<T>(poll: Poll<T>) -> Poll<T> {
    if poll.is_pending() {
        TASK_BUDGET.with(|budget| {
            if let Some(mut b) = budget.get() {
                b = b.saturating_add(1);
                budget.set(Some(b));
            }
        })
    }
    poll
}

fn set_budget(n: usize) {
    TASK_BUDGET.with(|budget| {
        budget.set(Some(n));
    })
}

#[pin_project]
pub struct WithBudget<F> {
    budget: NonZeroUsize,
    #[pin]
    fut: F,
}

impl<F> WithBudget<F> {
    pub fn new(fut: F) -> Self {
        WithBudget {
            budget: DEFAULT_START_BUDGET,
            fut,
        }
    }

    pub fn with_budget(budget: NonZeroUsize, fut: F) -> Self {
        WithBudget { budget, fut }
    }
}

pub trait BudgetedFuture: Sized + Future {
    fn budgeted(self) -> WithBudget<Self> {
        WithBudget::new(self)
    }

    fn with_budget(self, budget: NonZeroUsize) -> WithBudget<Self> {
        WithBudget::with_budget(budget, self)
    }

    fn with_budget_or_default(self, budget: Option<NonZeroUsize>) -> WithBudget<Self> {
        WithBudget::with_budget(budget.unwrap_or(DEFAULT_START_BUDGET), self)
    }
}

impl<F: Future> BudgetedFuture for F {}

impl<F: Default> Default for WithBudget<F> {
    fn default() -> Self {
        Self {
            budget: DEFAULT_START_BUDGET,
            fut: Default::default(),
        }
    }
}

impl<F: Future> Future for WithBudget<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let projected = self.project();
        set_budget(projected.budget.get());
        projected.fut.poll(cx)
    }
}
