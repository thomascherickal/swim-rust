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
use std::task::{Context, Poll};

use futures::ready;
use futures::Future;

use pin_project::pin_project;

use crate::err::MaybeTransientErr;
use crate::future::retryable::ResettableFuture;

pub type SendResult<Sender, T, Err> = Result<(T, Option<Sender>), (Err, Option<Sender>)>;

/// A retryable request using something such as an [`tokio::sync::mpsc::Sender`] to execute the
/// requests. Between failed retries, both the [`Sender`] and payload must be returned to successfuly
/// execute another request. The payload can either be cloned manually or retrieved by the [`SendError`]
/// returned by Tokio.
#[pin_project]
pub struct RetryableRequest<Sender, Fut, Fac, Unwrapper, Err> {
    /// The sender used to execute the requests.
    sender: Option<Sender>,
    /// A factory that accepts a sender, a payload, and a boolean indicating whether or not the current
    /// request is a retry. If the factory is returning a chained future, be mindful that the payload
    /// mut be returned by the last future. If it does not, then the [`unwrapper`] will fail to extract
    /// it.
    fac: Fac,
    /// A future returned by the factory.
    #[pin]
    f: Fut,
    /// An error returned by the last request. This error must implement the [`MaybeTransientErr`]
    /// which is used to determine whether or not to retry the request again. If the error is transient,
    /// then the request is attempted again.
    last_error: Option<Err>,
    /// A function that will unwrap the payload from the last error.
    unwrapper: Unwrapper,
}

impl<Sender, Fac, Fut, In, T, Unwrapper, Err> RetryableRequest<Sender, Fut, Fac, Unwrapper, Err>
where
    Fac: FnMut(Sender, In, bool) -> Fut,
    Fut: Future<Output = SendResult<Sender, T, Err>>,
    Unwrapper: FnMut(Err) -> In,
{
    pub fn new(sender: Sender, data: In, mut fac: Fac, unwrapper: Unwrapper) -> Self {
        let f = fac(sender, data, false);

        RetryableRequest {
            sender: None,
            fac,
            f,
            last_error: None,
            unwrapper,
        }
    }
}

impl<Sender, Fac, Fut, In, T, Unwrapper, Err> ResettableFuture
    for RetryableRequest<Sender, Fut, Fac, Unwrapper, Err>
where
    Fac: FnMut(Sender, In, bool) -> Fut,
    Fut: Future<Output = SendResult<Sender, T, Err>>,
    Unwrapper: FnMut(Err) -> In,
    Err: MaybeTransientErr,
{
    fn reset(self: Pin<&mut Self>) -> bool {
        let mut projected = self.project();
        let fac = projected.fac;

        if let Some(sender) = projected.sender.take() {
            if let Some(e) = projected.last_error.take() {
                if e.is_transient() {
                    let unwrapper = projected.unwrapper;
                    let data = unwrapper(e);
                    let f = fac(sender, data, true);
                    projected.f.set(f);

                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        }
    }
}

impl<Sender, Fac, Fut, In, T, Unwrapper, Err> Future
    for RetryableRequest<Sender, Fut, Fac, Unwrapper, Err>
where
    Fac: FnMut(Sender, In, bool) -> Fut,
    Fut: Future<Output = SendResult<Sender, T, Err>>,
    Unwrapper: FnMut(Err) -> In,
    Err: MaybeTransientErr,
{
    type Output = Result<T, Err>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let projected = self.project();
        let result = ready!(projected.f.poll(cx));
        match result {
            Ok((result, _sender)) => Poll::Ready(Ok(result)),
            Err((err, sender)) => {
                let failed = err.permanent();
                *projected.last_error = Some(err);
                *projected.sender = sender;

                Poll::Ready(Err(failed))
            }
        }
    }
}
