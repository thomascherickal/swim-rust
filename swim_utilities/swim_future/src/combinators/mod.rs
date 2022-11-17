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

mod immediate_or;
#[cfg(test)]
mod tests;

use futures::never::Never;
use futures::stream::FusedStream;
use futures::task::{Context, Poll};
use futures::{ready, Future, Sink, Stream, TryStream};
use pin_project::pin_project;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Notify;

pub use immediate_or::{
    immediate_or_join, immediate_or_start, ImmediateOrJoin, ImmediateOrStart, SecondaryResult,
};

/// Transforms a stream of `T` into a stream of [`Result<T, Never>`].
#[pin_project]
#[derive(Debug)]
pub struct NeverErrorStream<Str>(#[pin] Str);

impl<T, Str> Stream for NeverErrorStream<Str>
where
    Str: Stream<Item = T>,
{
    type Item = Result<T, Never>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let projected = self.project();
        projected.0.poll_next(cx).map(|t| t.map(Ok))
    }
}

/// A trait that is essentially equivalent to [`FnOnce`] with a single variable. However, it is
/// possible to implement this directly for a named type.
pub trait TransformOnce<In> {
    type Out;

    /// Trans from the input, potentially using the contents of this transformer.
    fn transform(self, input: In) -> Self::Out;
}

/// A trait that is essentially equivalent to [`Fn`] with a single variable. However, it is
/// possible to implement this directly for a named type.
pub trait Transform<In> {
    type Out;

    /// Transform the input.
    fn transform(&self, input: In) -> Self::Out;
}

/// A trait that is essentially equivalent to [`FnMut`] with a single variable. However, it is
/// possible to implement this directly for a named type.
pub trait TransformMut<In> {
    type Out;

    /// Transform the input.
    fn transform(&mut self, input: In) -> Self::Out;
}

impl<In, F> TransformMut<In> for F
where
    F: Transform<In>,
{
    type Out = F::Out;

    fn transform(&mut self, input: In) -> Self::Out {
        Transform::transform(self, input)
    }
}

impl<In, F> TransformOnce<In> for F
where
    F: TransformMut<In>,
{
    type Out = F::Out;

    fn transform(mut self, input: In) -> Self::Out {
        TransformMut::transform(&mut self, input)
    }
}

/// A stream that transforms another stream using a [`Transform`].
#[pin_project]
#[derive(Debug)]
pub struct TransformedStream<Str, Trans> {
    #[pin]
    stream: Str,
    transform: Trans,
}

/// A stream that transforms another stream using a [`Transform`] that results in a future.
#[pin_project(project = TransformedStreamFutProj)]
#[derive(Debug)]
pub struct TransformedStreamFut<Str, Trans, Fut> {
    #[pin]
    stream: Str,
    transform: Trans,
    #[pin]
    current: Option<Fut>,
    done: bool,
}

#[pin_project(project = FlatmapStreamProj)]
#[derive(Debug)]
pub struct FlatmapStream<Str1: Stream, Trans: TransformMut<Str1::Item>> {
    #[pin]
    stream: Str1,
    transform: Trans,
    #[pin]
    current: Option<Trans::Out>,
    done: bool,
}

#[pin_project(project = OwningScanProj)]
#[derive(Debug)]
pub struct OwningScan<Str, State, F, Fut> {
    #[pin]
    stream: Str,
    scan_fun: F,
    state: Option<State>,
    #[pin]
    current: Option<Fut>,
    done: bool,
}

impl<Str, Trans> TransformedStream<Str, Trans>
where
    Str: Stream,
    Trans: TransformMut<Str::Item>,
{
    pub fn new(stream: Str, transform: Trans) -> Self {
        TransformedStream { stream, transform }
    }
}

impl<Str, Trans> TransformedStreamFut<Str, Trans, Trans::Out>
where
    Str: Stream,
    Trans: TransformMut<Str::Item>,
    Trans::Out: Future,
{
    pub fn new(stream: Str, transform: Trans) -> Self {
        TransformedStreamFut {
            stream,
            transform,
            current: None,
            done: false,
        }
    }
}

impl<Str, Trans> FlatmapStream<Str, Trans>
where
    Str: Stream,
    Trans: TransformMut<Str::Item>,
    Trans::Out: Stream,
{
    pub fn new(stream: Str, transform: Trans) -> Self {
        FlatmapStream {
            stream,
            transform,
            current: None,
            done: false,
        }
    }
}

impl<Str, Trans> Stream for TransformedStream<Str, Trans>
where
    Str: Stream,
    Trans: TransformMut<Str::Item>,
{
    type Item = Trans::Out;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let projected = self.project();
        let stream = projected.stream;
        let trans = projected.transform;
        stream
            .poll_next(cx)
            .map(|r| r.map(|item| trans.transform(item)))
    }
}

impl<Str, Trans> Stream for TransformedStreamFut<Str, Trans, Trans::Out>
where
    Str: Stream,
    Trans: TransformMut<Str::Item>,
    Trans::Out: Future,
{
    type Item = <Trans::Out as Future>::Output;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let TransformedStreamFutProj {
            mut stream,
            transform,
            mut current,
            done,
        } = self.project();
        if *done {
            return Poll::Ready(None);
        }
        loop {
            if let Some(inner) = current.as_mut().as_pin_mut() {
                let result = ready!(inner.poll(cx));
                current.as_mut().set(None);
                break Poll::Ready(Some(result));
            } else {
                let maybe_item = ready!(stream.as_mut().poll_next(cx));
                if let Some(item) = maybe_item {
                    current.as_mut().set(Some(transform.transform(item)));
                } else {
                    *done = true;
                    break Poll::Ready(None);
                }
            }
        }
    }
}

impl<Str, Trans> FusedStream for TransformedStreamFut<Str, Trans, Trans::Out>
where
    Str: Stream,
    Trans: TransformMut<Str::Item>,
    Trans::Out: Future,
{
    fn is_terminated(&self) -> bool {
        self.done
    }
}

impl<Str, Trans> Stream for FlatmapStream<Str, Trans>
where
    Str: Stream,
    Trans: TransformMut<Str::Item>,
    Trans::Out: Stream,
{
    type Item = <Trans::Out as Stream>::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let FlatmapStreamProj {
            mut stream,
            transform,
            mut current,
            done,
        } = self.project();
        if *done {
            return Poll::Ready(None);
        }
        loop {
            if let Some(inner) = current.as_mut().as_pin_mut() {
                let result = ready!(inner.poll_next(cx));
                if let Some(v) = result {
                    break Poll::Ready(Some(v));
                } else {
                    current.as_mut().set(None);
                }
            } else {
                let maybe_item = ready!(stream.as_mut().poll_next(cx));
                if let Some(item) = maybe_item {
                    current.as_mut().set(Some(transform.transform(item)));
                } else {
                    *done = true;
                    break Poll::Ready(None);
                }
            }
        }
    }
}

impl<Str, Trans> FusedStream for FlatmapStream<Str, Trans>
where
    Str: Stream,
    Trans: TransformMut<Str::Item>,
    Trans::Out: Stream,
{
    fn is_terminated(&self) -> bool {
        self.done
    }
}

impl<Str, State, F, Fut> OwningScan<Str, State, F, Fut>
where
    Str: Stream,
    F: FnMut(State, Str::Item) -> Fut,
{
    pub fn new(stream: Str, init: State, scan_fun: F) -> Self {
        OwningScan {
            stream,
            scan_fun,
            state: Some(init),
            current: None,
            done: false,
        }
    }
}

impl<Str, State, F, Fut, B> Stream for OwningScan<Str, State, F, Fut>
where
    Str: Stream,
    F: FnMut(State, Str::Item) -> Fut,
    Fut: Future<Output = Option<(State, B)>>,
{
    type Item = B;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let OwningScanProj {
            mut stream,
            scan_fun,
            state,
            mut current,
            done,
        } = self.project();
        loop {
            if *done {
                break Poll::Ready(None);
            }
            if let Some(fut) = current.as_mut().as_pin_mut() {
                let poll_result = if let Some((new_state, item)) = ready!(fut.poll(cx)) {
                    *state = Some(new_state);
                    Poll::Ready(Some(item))
                } else {
                    current.set(None);
                    *done = true;
                    Poll::Ready(None)
                };
                current.set(None);
                break poll_result;
            } else {
                let maybe_next_input = ready!(stream.as_mut().poll_next(cx));
                if let Some(next_input) = maybe_next_input {
                    let prev_state = state.take().expect("Owning scan stream in invalid state.");
                    let fut = scan_fun(prev_state, next_input);
                    current.set(Some(fut));
                } else {
                    *state = None;
                    *done = true;
                    break Poll::Ready(None);
                }
            }
        }
    }
}

impl<Str, State, F, Fut, B> FusedStream for OwningScan<Str, State, F, Fut>
where
    Str: Stream,
    F: FnMut(State, Str::Item) -> Fut,
    Fut: Future<Output = Option<(State, B)>>,
{
    fn is_terminated(&self) -> bool {
        self.done
    }
}

/// A stream that runs another stream of [`Result`]s until it produces an error and then
/// terminates.
#[pin_project]
#[derive(Debug)]
pub struct StopAfterError<Str> {
    #[pin]
    stream: Str,
    terminated: bool,
}

impl<Str> StopAfterError<Str> {
    fn new(stream: Str) -> Self {
        StopAfterError {
            stream,
            terminated: false,
        }
    }
}

impl<Str: TryStream> Stream for StopAfterError<Str> {
    type Item = Result<Str::Ok, Str::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let projected = self.project();
        if *projected.terminated {
            Poll::Ready(None)
        } else {
            let result = ready!(projected.stream.try_poll_next(cx));
            if matches!(result, Some(Err(_))) {
                *projected.terminated = true;
            }
            Poll::Ready(result)
        }
    }
}

/// A stream that runs another stream of [`Result`]s until an error is produces, yielding the
/// OK values.
#[pin_project]
#[derive(Debug)]
pub struct UntilFailure<Str, Trans> {
    #[pin]
    stream: Str,
    transform: Trans,
}

impl<Str, Trans> UntilFailure<Str, Trans>
where
    Str: Stream,
    Trans: TransformMut<Str::Item>,
{
    pub fn new(stream: Str, transform: Trans) -> Self {
        UntilFailure { stream, transform }
    }
}

impl<Str, Trans, T, E> Stream for UntilFailure<Str, Trans>
where
    Str: Stream,
    Trans: TransformMut<Str::Item, Out = Result<T, E>>,
{
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let projected = self.project();
        let stream = projected.stream;
        let trans = projected.transform;
        stream
            .poll_next(cx)
            .map(|r| r.and_then(|item| trans.transform(item).ok()))
    }
}

pub trait SwimStreamExt: Stream {
    /// Apply a transformation to the items of a stream.
    ///
    ///  # Examples
    /// ```
    /// use futures::executor::block_on;
    /// use futures::StreamExt;
    /// use futures::stream::iter;
    /// use swim_future::*;
    /// use std::ops::Add;
    /// use swim_future::{SwimFutureExt, SwimStreamExt};
    ///
    /// struct Plus(i32);
    ///
    /// impl TransformMut<i32> for Plus {
    ///     type Out = i32;
    ///
    ///     fn transform(&mut self, input: i32) -> Self::Out {
    ///         input + self.0
    ///     }
    /// }
    ///
    /// let inputs = iter((0..5).into_iter());
    ///
    /// let outputs: Vec<i32> = block_on(inputs.transform(Plus(3)).collect::<Vec<i32>>());
    /// assert_eq!(outputs, vec![3, 4, 5, 6, 7]);
    /// ```
    fn transform<Trans>(self, transform: Trans) -> TransformedStream<Self, Trans>
    where
        Self: Sized,
        Trans: TransformMut<Self::Item>,
    {
        TransformedStream::new(self, transform)
    }

    /// Apply a transformation, resulting in a future, to the items of a stream, evaluating each
    /// future to produce the elements of the new stream.
    ///
    ///  # Examples
    /// ```
    /// use futures::executor::block_on;
    /// use futures::StreamExt;
    /// use futures::future::{ready, Ready};
    /// use futures::stream::iter;
    /// use swim_future::*;
    /// use std::ops::Add;
    /// use swim_future::{SwimFutureExt, SwimStreamExt};
    ///
    /// struct Plus(i32);
    ///
    /// impl TransformMut<i32> for Plus {
    ///     type Out = Ready<i32>;
    ///
    ///     fn transform(&mut self, input: i32) -> Self::Out {
    ///         ready(input + self.0)
    ///     }
    /// }
    ///
    /// let inputs = iter((0..5).into_iter());
    ///
    /// let outputs: Vec<i32> = block_on(inputs.transform_fut(Plus(3)).collect::<Vec<i32>>());
    /// assert_eq!(outputs, vec![3, 4, 5, 6, 7]);
    /// ```
    fn transform_fut<Trans>(self, transform: Trans) -> TransformedStreamFut<Self, Trans, Trans::Out>
    where
        Self: Sized,
        Trans: TransformMut<Self::Item>,
        Trans::Out: Future,
    {
        TransformedStreamFut::new(self, transform)
    }

    /// Apply a transformation, resulting in a stream for each item, to the items of a stream,
    /// evaluating each stream to produce the elements of the new stream.
    ///
    ///  # Examples
    /// ```
    /// use futures::executor::block_on;
    /// use futures::StreamExt;
    /// use futures::future::{ready, Ready};
    /// use futures::stream::{iter, Iter};
    /// use swim_future::*;
    /// use std::ops::Add;
    /// use swim_future::{SwimFutureExt, SwimStreamExt};
    /// use std::iter::{Repeat, Take, repeat};
    ///
    /// struct RepeatItems(usize);
    ///
    /// impl TransformMut<i32> for RepeatItems {
    ///     type Out = Iter<Take<Repeat<i32>>>;
    ///
    ///     fn transform(&mut self, input: i32) -> Self::Out {
    ///         iter(repeat(input).take(self.0))
    ///     }
    /// }
    ///
    /// let inputs = iter((0..3).into_iter());
    ///
    /// let outputs: Vec<i32> = block_on(inputs.transform_flat_map(RepeatItems(2)).collect::<Vec<i32>>());
    /// assert_eq!(outputs, vec![0, 0, 1, 1, 2, 2]);
    /// ```
    fn transform_flat_map<Trans>(self, transform: Trans) -> FlatmapStream<Self, Trans>
    where
        Self: Sized,
        Trans: TransformMut<Self::Item>,
        Trans::Out: Stream,
    {
        FlatmapStream::new(self, transform)
    }

    /// Transform the items of a stream until an error is encountered, then terminate.
    ///  # Examples
    /// ```
    /// use futures::executor::block_on;
    /// use futures::StreamExt;
    /// use futures::stream::iter;
    /// use swim_future::*;
    /// use std::ops::Add;
    /// use swim_future::{SwimFutureExt, SwimStreamExt};
    /// struct PlusIfNonNeg(i32);
    ///
    /// impl TransformMut<i32> for PlusIfNonNeg {
    ///     type Out = Result<i32, ()>;
    ///
    ///     fn transform(&mut self, input: i32) -> Self::Out {
    ///         if input >= 0 {
    ///             Ok(input + self.0)
    ///         } else {
    ///             Err(())
    ///         }
    ///     }
    /// }
    ///
    /// let inputs = iter(vec![0, 1, 2, -3, 4].into_iter());
    /// let outputs: Vec<i32> = block_on(inputs.until_failure(PlusIfNonNeg(3)).collect::<Vec<i32>>());
    /// assert_eq!(outputs, vec![3, 4, 5]);
    /// ```
    fn until_failure<Trans, T, E>(self, transform: Trans) -> UntilFailure<Self, Trans>
    where
        Self: Sized,
        Trans: TransformMut<Self::Item, Out = Result<T, E>>,
    {
        UntilFailure::new(self, transform)
    }

    /// Run the stream until an error is encountered and then stop.
    ///
    /// #Examples
    /// ```
    /// use futures::executor::block_on;
    /// use futures::stream::iter;
    /// use futures::StreamExt;
    /// use swim_future::SwimStreamExt;
    ///
    /// let inputs = iter(vec![Ok(0), Ok(1), Ok(2), Err("Boom!"), Ok(4), Err("Boom!")].into_iter());
    /// let outputs: Vec<Result<i32, &'static str>> = block_on(inputs.stop_after_error().collect());
    ///
    /// assert_eq!(outputs, vec![Ok(0), Ok(1), Ok(2), Err("Boom!")]);
    ///
    /// ```
    fn stop_after_error(self) -> StopAfterError<Self>
    where
        Self: Sized,
    {
        StopAfterError::new(self)
    }

    /// Transform this stream into an infallible [`NeverErrorStream`].
    ///
    fn never_error(self) -> NeverErrorStream<Self>
    where
        Self: Sized,
    {
        NeverErrorStream(self)
    }

    /// Transform the items of a stream with a stateful operation. This differs from `scan` in
    /// [`futures::FutureExt`] in that ownership of the state is passed through the scan function
    /// rather than being maintained in the combinator.
    ///
    ///  # Examples
    /// ```
    /// use futures::executor::block_on;
    /// use futures::StreamExt;
    /// use futures::future::ready;
    /// use futures::stream::iter;
    /// use swim_future::*;
    /// use swim_future::SwimStreamExt;
    ///
    /// let inputs = iter(vec![1, 2, 3, 4].into_iter());
    /// let outputs: Vec<(i32, i32)> = block_on(inputs.owning_scan(0, |state, i| {
    ///     ready(Some((i, (state, i))))
    /// }).collect::<Vec<_>>());
    /// assert_eq!(outputs, vec![(0, 1), (1, 2), (2, 3), (3, 4)]);
    /// ```
    fn owning_scan<State, F, Fut, B>(
        self,
        initial_state: State,
        f: F,
    ) -> OwningScan<Self, State, F, Fut>
    where
        Self: Sized,
        F: FnMut(State, Self::Item) -> Fut,
        Fut: Future<Output = Option<(State, B)>>,
    {
        OwningScan::new(self, initial_state, f)
    }

    /// Wrap this in a future that will provide a notification each time it is blocked.
    fn notify_on_blocked(self, notify: Arc<Notify>) -> NotifyOnBlocked<Self>
    where
        Self: Sized,
    {
        NotifyOnBlocked::new(self, notify)
    }
}

#[pin_project]
#[derive(Debug)]
pub struct TransformedSink<S, Trans> {
    #[pin]
    inner: S,
    transformer: Trans,
}

impl<S, Trans> TransformedSink<S, Trans> {
    pub fn new(sink: S, transformer: Trans) -> TransformedSink<S, Trans> {
        TransformedSink {
            inner: sink,
            transformer,
        }
    }
}

impl<S, Trans, Item> Sink<Item> for TransformedSink<S, Trans>
where
    Trans: TransformMut<Item>,
    S: Sink<Trans::Out>,
{
    type Error = S::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Item) -> Result<(), Self::Error> {
        let this = self.project();
        let transformed = this.transformer.transform(item);

        this.inner.start_send(transformed)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_close(cx)
    }
}

pub trait SwimSinkExt<Item>: Sink<Item> {
    /// Applys a transformation to each element that is sent to the sink.
    fn transform<Trans, I>(self, transformer: Trans) -> TransformedSink<Self, Trans>
    where
        Self: Sized,
        Trans: TransformMut<I>,
    {
        TransformedSink::new(self, transformer)
    }
}

impl<S> SwimStreamExt for S where S: Stream {}

#[pin_project]
pub struct NotifyOnBlocked<F> {
    #[pin]
    inner: F,
    notify: Arc<Notify>,
}

impl<F> NotifyOnBlocked<F> {
    pub fn new(inner: F, notify: Arc<Notify>) -> NotifyOnBlocked<F> {
        NotifyOnBlocked { inner, notify }
    }
}

impl<F: Future> Future for NotifyOnBlocked<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let projected = self.project();
        let result = projected.inner.poll(cx);
        if result.is_pending() {
            projected.notify.notify_one();
        }
        result
    }
}

impl<S: Stream> Stream for NotifyOnBlocked<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let projected = self.project();
        let result = projected.inner.poll_next(cx);
        if result.is_pending() {
            projected.notify.notify_one();
        }
        result
    }
}
