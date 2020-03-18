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

use std::future::Future;

use futures::future::{BoxFuture, Ready};
use futures::task::{Context, Poll};
use futures::{future, FutureExt};
use std::marker::PhantomData;
use std::pin::Pin;
use tokio::sync::{broadcast, mpsc, watch};

/// An alternative to the [`futures::Sink`] trait for sinks that can consume their inputs in a
/// single operation. This can simplify operations where one can guarantee that the target sink
/// is a queue and will not be performing IO directly (for example in lane models).
pub trait ItemSink<'a, T> {
    type Error;
    type SendFuture: Future<Output = Result<(), Self::Error>> + Send + 'a;

    /// Attempt to send an item into the sink.
    fn send_item(&'a mut self, value: T) -> Self::SendFuture;
}

pub trait ItemSender<T, E>: for<'a> ItemSink<'a, T, Error = E> {
    fn map_err_into<E2>(self) -> map_err::SenderErrInto<Self, E2>
    where
        Self: Sized,
        E2: From<E>,
    {
        map_err::SenderErrInto::new(self)
    }

    fn comap<S, F>(self, f: F) -> comap::ItemSenderComap<Self, F>
    where
        Self: Sized,
        F: FnMut(S) -> T,
    {
        comap::ItemSenderComap::new(self, f)
    }
}

impl<X, T, E> ItemSender<T, E> for X where X: for<'a> ItemSink<'a, T, Error = E> {}

#[derive(Clone, Debug)]
pub struct SendError {}

pub type BoxItemSink<T, E> =
    Box<dyn for<'a> ItemSink<'a, T, Error = E, SendFuture = BoxFuture<'a, Result<(), E>>>>;

pub fn boxed<T, E, Snk>(sink: Snk) -> BoxItemSink<T, E>
where
    T: 'static,
    Snk: for<'a> ItemSink<'a, T, Error = E> + 'static,
{
    let boxing_sink = BoxingSink(sink);
    let boxed: BoxItemSink<T, E> = Box::new(boxing_sink);
    boxed
}

pub type MpscErr<T> = mpsc::error::SendError<T>;
pub type WatchErr<T> = watch::error::SendError<T>;
pub type BroadcastErr<T> = broadcast::SendError<T>;

/// Wrap an [`mpsc::Sender`] as an item sink. It is not possible to implement the trait
/// directly as the `send` method returns an anonymous type.
pub fn for_mpsc_sender<T: Send + 'static, Err: From<MpscErr<T>> + Send + 'static>(
    sender: mpsc::Sender<T>,
) -> impl ItemSender<T, Err> {
    sender.map_err_into()
}

pub fn for_watch_sender<T: Clone + Send + 'static, Err: From<SendError> + Send + 'static>(
    sender: watch::Sender<Option<T>>,
) -> impl ItemSender<T, Err> {
    WatchSink(sender).map_err_into()
}

pub fn for_broadcast_sender<
    T: Clone + Send + 'static,
    Err: From<BroadcastErr<T>> + Send + 'static,
>(
    sender: broadcast::Sender<T>,
) -> impl ItemSender<T, Err> {
    sender.map_err_into()
}

pub struct MpscSend<'a, T, E> {
    sender: Pin<&'a mut mpsc::Sender<T>>,
    value: Option<T>,
    _err: PhantomData<E>,
}

impl<'a, T, E> Unpin for MpscSend<'a, T, E> {}

impl<'a, T, E> MpscSend<'a, T, E> {
    pub fn new(sender: &'a mut mpsc::Sender<T>, value: T) -> MpscSend<'a, T, E> {
        MpscSend {
            sender: Pin::new(sender),
            value: Some(value),
            _err: PhantomData,
        }
    }
}

impl<'a, T, E> Future for MpscSend<'a, T, E>
where
    E: From<mpsc::error::SendError<T>>,
{
    type Output = Result<(), E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let MpscSend { sender, value, .. } = self.get_mut();

        match sender.poll_ready(cx) {
            Poll::Ready(Ok(_)) => match value.take() {
                Some(t) => match sender.try_send(t) {
                    Ok(_) => Poll::Ready(Ok(())),
                    Err(mpsc::error::TrySendError::Closed(t)) => {
                        Poll::Ready(Err(mpsc::error::SendError(t).into()))
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => unreachable!(),
                },
                _ => panic!("Send future evaluated twice."),
            },
            Poll::Ready(Err(_)) => match value.take() {
                Some(t) => Poll::Ready(Err(mpsc::error::SendError(t).into())),
                _ => panic!("Send future evaluated twice."),
            },
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<'a, T: Send + 'a> ItemSink<'a, T> for mpsc::Sender<T> {
    type Error = mpsc::error::SendError<T>;
    type SendFuture = MpscSend<'a, T, mpsc::error::SendError<T>>;

    fn send_item(&'a mut self, value: T) -> Self::SendFuture {
        MpscSend {
            sender: Pin::new(self),
            value: Some(value),
            _err: PhantomData,
        }
    }
}

pub mod map_err {
    use crate::sink::item::ItemSink;
    use futures::future::ErrInto;
    use futures_util::future::TryFutureExt;
    use std::marker::PhantomData;

    pub struct SenderErrInto<Sender, E> {
        sender: Sender,
        _target: PhantomData<E>,
    }

    impl<Sender, E> SenderErrInto<Sender, E> {
        pub fn new(sender: Sender) -> SenderErrInto<Sender, E> {
            SenderErrInto {
                sender,
                _target: PhantomData,
            }
        }
    }

    impl<'a, T, E, Sender> super::ItemSink<'a, T> for SenderErrInto<Sender, E>
    where
        Sender: ItemSink<'a, T>,
        E: From<Sender::Error> + Send + 'a,
    {
        type Error = E;
        type SendFuture = ErrInto<Sender::SendFuture, E>;

        fn send_item(&'a mut self, value: T) -> Self::SendFuture {
            self.sender.send_item(value).err_into()
        }
    }
}

pub struct WatchSink<T>(watch::Sender<Option<T>>);

impl<T> WatchSink<T> {
    pub fn broadcast(&self, value: T) -> Result<(), SendError> {
        match self.0.broadcast(Some(value)) {
            Ok(_) => Ok(()),
            Err(_) => Err(SendError {}),
        }
    }
}

impl<'a, T: Send + 'a> ItemSink<'a, T> for WatchSink<T> {
    type Error = SendError;
    type SendFuture = Ready<Result<(), Self::Error>>;

    fn send_item(&'a mut self, value: T) -> Self::SendFuture {
        future::ready(self.broadcast(value))
    }
}

impl<'a, T: Send + 'a> ItemSink<'a, T> for watch::Sender<T> {
    type Error = WatchErr<T>;
    type SendFuture = Ready<Result<(), Self::Error>>;

    fn send_item(&'a mut self, value: T) -> Self::SendFuture {
        future::ready(self.broadcast(value))
    }
}

impl<'a, T: Send + 'a> ItemSink<'a, T> for broadcast::Sender<T> {
    type Error = BroadcastErr<T>;
    type SendFuture = Ready<Result<(), Self::Error>>;

    fn send_item(&'a mut self, value: T) -> Self::SendFuture {
        future::ready(self.send(value).map(|_| ()))
    }
}

struct BoxingSink<Snk>(Snk);

impl<'a, T, Snk> ItemSink<'a, T> for BoxingSink<Snk>
where
    Snk: ItemSink<'a, T> + 'a,
    T: 'a,
{
    type Error = Snk::Error;
    type SendFuture = BoxFuture<'a, Result<(), Self::Error>>;

    fn send_item(&'a mut self, value: T) -> Self::SendFuture {
        FutureExt::boxed(self.0.send_item(value))
    }
}

impl<'a, T, E: 'a> ItemSink<'a, T> for BoxItemSink<T, E> {
    type Error = E;
    type SendFuture = BoxFuture<'a, Result<(), Self::Error>>;

    fn send_item(&'a mut self, value: T) -> Self::SendFuture {
        (**self).send_item(value)
    }
}

pub mod comap {
    use super::ItemSink;

    pub struct ItemSenderComap<Sender, F> {
        sender: Sender,
        f: F,
    }

    impl<Sender, F> ItemSenderComap<Sender, F> {
        pub fn new(sender: Sender, f: F) -> ItemSenderComap<Sender, F> {
            ItemSenderComap { sender, f }
        }
    }

    impl<'a, S, T, Sender, F> ItemSink<'a, S> for ItemSenderComap<Sender, F>
    where
        Sender: ItemSink<'a, T>,
        F: FnMut(S) -> T,
    {
        type Error = Sender::Error;
        type SendFuture = Sender::SendFuture;

        fn send_item(&'a mut self, value: S) -> Self::SendFuture {
            let ItemSenderComap { sender, f } = self;
            sender.send_item(f(value))
        }
    }
}
