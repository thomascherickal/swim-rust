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

use crate::var::Contents;
use futures::future::BoxFuture;
use futures::FutureExt;
use futures_util::future::ready;
use std::any::Any;
use std::sync::Arc;
use tokio::sync::oneshot::error::TryRecvError;
use tokio::sync::{broadcast, mpsc, oneshot};

/// Type erased observer to be passed into [`TVarInner`].
pub(super) trait RawObserver {
    fn notify_raw(&mut self, value: Contents) -> BoxFuture<()>;
}

pub(super) type DynObserver = Box<dyn RawObserver + Send + Sync + 'static>;

impl<T: Any + Send + Sync> RawObserver for Observer<T> {
    fn notify_raw(&mut self, value: Contents) -> BoxFuture<'_, ()> {
        match value.downcast::<T>() {
            Ok(t) => self.notify(t).boxed(),
            Err(_) => ready(()).boxed(),
        }
    }
}

pub enum ObsSender<T> {
    Mpsc(mpsc::Sender<Arc<T>>),
    Broadcast(broadcast::Sender<Arc<T>>),
}

struct SingleObs<T> {
    sender: ObsSender<T>,
    is_dead: bool,
}

impl<T> SingleObs<T> {
    fn new(sender: ObsSender<T>) -> Self {
        SingleObs {
            sender,
            is_dead: false,
        }
    }
}

impl<T> SingleObs<T> {
    pub async fn notify(&mut self, value: Arc<T>) {
        let SingleObs { sender, is_dead } = self;
        if !*is_dead {
            match sender {
                ObsSender::Mpsc(tx) => {
                    if tx.send(value).await.is_err() {
                        *is_dead = true;
                    }
                }
                ObsSender::Broadcast(tx) => {
                    if tx.send(value).is_err() {
                        *is_dead = true;
                    }
                }
            }
        }
    }
}

enum DeferredObserver<T> {
    Empty,
    Waiting(oneshot::Receiver<ObsSender<T>>),
    Initialized(SingleObs<T>),
}

pub struct Observer<T> {
    primary: SingleObs<T>,
    deferred: DeferredObserver<T>,
}

impl<T> From<mpsc::Sender<Arc<T>>> for ObsSender<T> {
    fn from(tx: mpsc::Sender<Arc<T>>) -> Self {
        ObsSender::Mpsc(tx)
    }
}

impl<T> From<broadcast::Sender<Arc<T>>> for ObsSender<T> {
    fn from(tx: broadcast::Sender<Arc<T>>) -> Self {
        ObsSender::Broadcast(tx)
    }
}

impl<T, S> From<S> for Observer<T>
where
    S: Into<ObsSender<T>>,
{
    fn from(tx: S) -> Self {
        Observer::new(tx.into())
    }
}

impl<T> Observer<T> {
    pub fn new(sender: ObsSender<T>) -> Self {
        Observer {
            primary: SingleObs::new(sender),
            deferred: DeferredObserver::Empty,
        }
    }

    pub fn new_with_deferred(
        sender: ObsSender<T>,
        deferred: oneshot::Receiver<ObsSender<T>>,
    ) -> Self {
        Observer {
            primary: SingleObs::new(sender),
            deferred: DeferredObserver::Waiting(deferred),
        }
    }

    pub async fn notify(&mut self, value: Arc<T>) {
        let Observer { primary, deferred } = self;
        match deferred {
            DeferredObserver::Waiting(rx) => match rx.try_recv() {
                Ok(tx) => {
                    let mut sender = SingleObs::new(tx);
                    sender.notify(value.clone()).await;
                    *deferred = DeferredObserver::Initialized(sender);
                }
                Err(TryRecvError::Closed) => {
                    *deferred = DeferredObserver::Empty;
                }
                _ => {}
            },
            DeferredObserver::Initialized(sender) => {
                sender.notify(value.clone()).await;
            }
            _ => {}
        }
        primary.notify(value).await;
    }
}
