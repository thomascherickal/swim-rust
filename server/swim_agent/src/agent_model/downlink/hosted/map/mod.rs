// Copyright 2015-2023 Swim Inc.
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
    cell::RefCell,
    collections::{BTreeSet, HashMap},
    pin::pin,
    sync::{atomic::AtomicU8, Arc},
};

use futures::{
    future::{select, BoxFuture, Either, OptionFuture},
    stream::unfold,
    FutureExt, SinkExt, Stream, StreamExt,
};
use std::hash::Hash;
use swim_api::{
    downlink::DownlinkKind,
    error::{AgentRuntimeError, FrameIoError},
    protocol::{
        downlink::{DownlinkNotification, MapNotificationDecoder},
        map::{MapMessage, MapOperation, MapOperationEncoder},
    },
};
use swim_form::{
    structural::{read::recognizer::RecognizerReadable, write::StructuralWritable},
    Form,
};
use swim_model::{address::Address, Text};
use swim_utilities::{
    io::byte_channel::{ByteReader, ByteWriter},
    trigger,
};
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::{debug, error, info, trace};

use crate::{
    agent_model::downlink::handlers::{
        BoxDownlinkChannel2, DownlinkChannel, DownlinkChannel2, DownlinkChannelError,
        DownlinkChannelEvent, DownlinkFailed,
    },
    config::MapDownlinkConfig,
    downlink_lifecycle::map::MapDownlinkLifecycle,
    event_handler::{BoxEventHandler, HandlerActionExt, Sequentially},
    event_queue::EventQueue,
};

use super::{DlState, DlStateObserver, DlStateTracker};

#[cfg(test)]
mod tests;

/// Internal state of a map downlink. For most purposes this uses the hashmap (for constant time
/// accesses). To support the (infrequently used) take and drop operations, it will generate a
/// separate ordered set of the keys which will then be kept up to date with the map.
#[derive(Debug)]
pub struct MapDlState<K, V> {
    map: HashMap<K, V>,
    order: Option<BTreeSet<K>>,
}

impl<K, V> Default for MapDlState<K, V> {
    fn default() -> Self {
        Self {
            map: Default::default(),
            order: Default::default(),
        }
    }
}

/// Operations that need to be supported by the state store of a map downlink. The intention
/// of this trait is to abstract over a self contained store a store contained within the field
/// of an agent. In both cases, the store itself will a [`RefCell`] containing a [`MapDlState`].
trait MapDlStateOps<K, V, Context>: Send {
    fn clear(&self, context: &Context) -> HashMap<K, V>;

    // Perform an operation in a context with access to the state.
    fn with<G, T>(&self, context: &Context, f: G) -> T
    where
        G: FnOnce(&mut MapDlState<K, V>) -> T;

    fn update<'a, LC>(
        &self,
        context: &Context,
        key: K,
        value: V,
        lifecycle: Option<&'a LC>,
    ) -> Option<BoxEventHandler<'a, Context>>
    where
        K: Eq + Hash + Clone + Ord,
        LC: MapDownlinkLifecycle<K, V, Context>,
    {
        self.with(context, move |MapDlState { map, order }| {
            let old = map.insert(key.clone(), value);
            match order {
                Some(ord) if old.is_some() => {
                    ord.insert(key.clone());
                }
                _ => {}
            }
            let new_value = &map[&key];
            lifecycle.map(|lifecycle| lifecycle.on_update(key, &*map, old, new_value).boxed())
        })
    }

    fn remove<'a, LC>(
        &self,
        context: &Context,
        key: K,
        lifecycle: Option<&'a LC>,
    ) -> Option<BoxEventHandler<'a, Context>>
    where
        K: Eq + Hash + Ord,
        LC: MapDownlinkLifecycle<K, V, Context>,
    {
        self.with(context, move |MapDlState { map, order }| {
            map.remove(&key).and_then(move |old| {
                if let Some(ord) = order {
                    ord.remove(&key);
                }
                lifecycle.map(|lifecycle| lifecycle.on_remove(key, &*map, old).boxed())
            })
        })
    }

    fn drop<'a, LC>(
        &self,
        context: &Context,
        n: usize,
        lifecycle: Option<&'a LC>,
    ) -> Option<BoxEventHandler<'a, Context>>
    where
        K: Eq + Hash + Ord + Clone,
        LC: MapDownlinkLifecycle<K, V, Context>,
    {
        self.with(context, move |MapDlState { map, order }| {
            if n >= map.len() {
                *order = None;
                let old = std::mem::take(map);
                lifecycle.map(move |lifecycle| lifecycle.on_clear(old).boxed())
            } else {
                let ord = order.get_or_insert_with(|| map.keys().cloned().collect());

                //Decompose the take into a sequence of removals.
                let to_remove: Vec<_> = ord.iter().take(n).cloned().collect();
                if let Some(lifecycle) = lifecycle {
                    let expected = n.min(map.len());
                    let mut removed = Vec::with_capacity(expected);

                    for k in to_remove {
                        ord.remove(&k);
                        if let Some(v) = map.remove(&k) {
                            removed.push(lifecycle.on_remove(k, map, v));
                        }
                    }
                    if removed.is_empty() {
                        None
                    } else {
                        Some(Sequentially::new(removed).boxed())
                    }
                } else {
                    for k in to_remove {
                        ord.remove(&k);
                        map.remove(&k);
                    }
                    None
                }
            }
        })
    }

    fn take<'a, LC>(
        &self,
        context: &Context,
        n: usize,
        lifecycle: Option<&'a LC>,
    ) -> Option<BoxEventHandler<'a, Context>>
    where
        K: Eq + Hash + Ord + Clone,
        LC: MapDownlinkLifecycle<K, V, Context>,
    {
        self.with(context, move |MapDlState { map, order }| {
            let to_drop = map.len().saturating_sub(n);
            if to_drop > 0 {
                let ord = order.get_or_insert_with(|| map.keys().cloned().collect());

                //Decompose the drop into a sequence of removals.
                let to_remove: Vec<_> = ord.iter().rev().take(to_drop).cloned().collect();
                if let Some(lifecycle) = lifecycle {
                    let mut removed = Vec::with_capacity(to_drop);

                    for k in to_remove.into_iter().rev() {
                        ord.remove(&k);
                        if let Some(v) = map.remove(&k) {
                            removed.push(lifecycle.on_remove(k, map, v));
                        }
                    }
                    if removed.is_empty() {
                        None
                    } else {
                        Some(Sequentially::new(removed).boxed())
                    }
                } else {
                    for k in to_remove {
                        ord.remove(&k);
                        map.remove(&k);
                    }
                    None
                }
            } else {
                None
            }
        })
    }
}

impl<K, V, Context> MapDlStateOps<K, V, Context> for RefCell<MapDlState<K, V>>
where
    K: Send,
    V: Send,
{
    fn clear(&self, _context: &Context) -> HashMap<K, V> {
        self.replace(MapDlState::default()).map
    }

    fn with<F, T>(&self, _context: &Context, f: F) -> T
    where
        F: FnOnce(&mut MapDlState<K, V>) -> T,
    {
        f(&mut *self.borrow_mut())
    }
}

impl<K, V, Context, F> MapDlStateOps<K, V, Context> for F
where
    F: for<'a> Fn(&'a Context) -> &'a RefCell<MapDlState<K, V>> + Send,
{
    fn clear(&self, context: &Context) -> HashMap<K, V> {
        self(context).replace(MapDlState::default()).map
    }

    fn with<G, T>(&self, context: &Context, f: G) -> T
    where
        G: FnOnce(&mut MapDlState<K, V>) -> T,
    {
        f(&mut *self(context).borrow_mut())
    }
}

pub struct HostedMapDownlinkFactory<K, V, LC, State> {
    address: Address<Text>,
    state: State,
    lifecycle: LC,
    config: MapDownlinkConfig,
    dl_state: Arc<AtomicU8>,
    stop_rx: trigger::Receiver,
    op_rx: mpsc::UnboundedReceiver<MapOperation<K, V>>,
}

impl<K, V, LC, State> HostedMapDownlinkFactory<K, V, LC, State>
where
    K: Hash + Eq + Ord + Clone + Form + Send + 'static,
    V: Form + Send + 'static,
    K::Rec: Send,
    V::Rec: Send,
{
    pub fn new(
        address: Address<Text>,
        lifecycle: LC,
        state: State,
        config: MapDownlinkConfig,
        stop_rx: trigger::Receiver,
        op_rx: mpsc::UnboundedReceiver<MapOperation<K, V>>,
    ) -> Self {
        HostedMapDownlinkFactory {
            address,
            state,
            lifecycle,
            config,
            dl_state: Default::default(),
            stop_rx,
            op_rx,
        }
    }

    fn create<Context>(self, io: (ByteWriter, ByteReader)) -> BoxDownlinkChannel2<Context>
    where
        State: MapDlStateOps<K, V, Context> + 'static,
        LC: MapDownlinkLifecycle<K, V, Context> + 'static,
    {
        let HostedMapDownlinkFactory {
            address,
            state,
            lifecycle,
            config,
            dl_state,
            stop_rx,
            op_rx,
        } = self;
        let (sender, receiver) = io;
        let write_stream = map_dl_write_stream(sender, op_rx).boxed();
        let chan = HostedMapDownlink2 {
            address,
            receiver: Some(FramedRead::new(receiver, Default::default())),
            write_stream: Some(write_stream),
            state,
            next: None,
            lifecycle,
            config,
            dl_state: DlStateTracker::new(dl_state),
            stop_rx: Some(stop_rx),
        };
        Box::new(chan)
    }
}

/// An implementation of [`DownlinkChannel`] to allow a map downlink to be driven by an agent
/// task.
pub struct HostedMapDownlinkChannel<K: RecognizerReadable, V: RecognizerReadable, LC, State> {
    address: Address<Text>,
    receiver: Option<FramedRead<ByteReader, MapNotificationDecoder<K, V>>>,
    state: State,
    next: Option<Result<DownlinkNotification<MapMessage<K, V>>, FrameIoError>>,
    lifecycle: LC,
    config: MapDownlinkConfig,
    dl_state: DlStateTracker,
    stop_rx: Option<trigger::Receiver>,
}

pub struct HostedMapDownlink2<K: RecognizerReadable, V: RecognizerReadable, LC, State, Writes> {
    address: Address<Text>,
    receiver: Option<FramedRead<ByteReader, MapNotificationDecoder<K, V>>>,
    write_stream: Option<Writes>,
    state: State,
    next: Option<Result<DownlinkNotification<MapMessage<K, V>>, FrameIoError>>,
    lifecycle: LC,
    config: MapDownlinkConfig,
    dl_state: DlStateTracker,
    stop_rx: Option<trigger::Receiver>,
}

impl<K, V, LC, State, Writes> HostedMapDownlink2<K, V, LC, State, Writes>
where
    K: Hash + Eq + Ord + Clone + RecognizerReadable + Send + 'static,
    V: RecognizerReadable + Send + 'static,
    K::Rec: Send,
    V::Rec: Send,
    Writes: Stream<Item = Result<(), std::io::Error>> + Send + Unpin + 'static,
{
    async fn select_next(&mut self) -> Option<Result<DownlinkChannelEvent, DownlinkChannelError>> {
        let HostedMapDownlink2 {
            address,
            receiver,
            next,
            stop_rx,
            write_stream,
            ..
        } = self;
        let select_next = pin!(async {
            tokio::select! {
                maybe_result = OptionFuture::from(receiver.as_mut().map(|rx| rx.next())), if receiver.is_some() => {
                    match maybe_result.flatten() {
                        r@Some(Ok(_)) => {
                            *next = r;
                            Some(Ok(DownlinkChannelEvent::HandlerReady))
                        }
                        r@Some(Err(_)) => {
                            *next = r;
                            *receiver = None;
                            error!(address = %address, "Downlink input channel failed.");
                            Some(Err(DownlinkChannelError::ReadFailed))
                        }
                        _ => {
                            *receiver = None;
                            None
                        }
                    }
                },
                maybe_result = OptionFuture::from(write_stream.as_mut().map(|str| str.next())), if write_stream.is_some() => {
                    match maybe_result.flatten() {
                        Some(Ok(_)) => Some(Ok(DownlinkChannelEvent::WriteCompleted)),
                        Some(Err(e)) => Some(Err(DownlinkChannelError::WriteFailed(e))),
                        _ => {
                            *write_stream = None;
                            Some(Ok(DownlinkChannelEvent::WriteStreamTerminated))
                        }
                    }
                }
                else => {
                    info!(address = %address, "Downlink terminated normally.");
                    None
                },
            }
        });
        if let Some(stop_signal) = stop_rx.as_mut() {
            match futures::future::select(stop_signal, select_next).await {
                Either::Left((triggered_result, select_next)) => {
                    *stop_rx = None;
                    if triggered_result.is_ok() {
                        *receiver = None;
                        *write_stream = None;
                        *next = Some(Ok(DownlinkNotification::Unlinked));
                        Some(Ok(DownlinkChannelEvent::HandlerReady))
                    } else {
                        select_next.await
                    }
                }
                Either::Right((result, _)) => result,
            }
        } else {
            select_next.await
        }
    }
}

impl<K, V, LC, Context, State, Writes> DownlinkChannel2<Context>
    for HostedMapDownlink2<K, V, LC, State, Writes>
where
    State: MapDlStateOps<K, V, Context>,
    K: Hash + Eq + Ord + Clone + RecognizerReadable + Send + 'static,
    V: RecognizerReadable + Send + 'static,
    K::Rec: Send,
    V::Rec: Send,
    LC: MapDownlinkLifecycle<K, V, Context> + 'static,
    Writes: Stream<Item = Result<(), std::io::Error>> + Send + Unpin + 'static,
{
    fn kind(&self) -> DownlinkKind {
        DownlinkKind::Map
    }

    fn await_ready(
        &mut self,
    ) -> BoxFuture<'_, Option<Result<DownlinkChannelEvent, DownlinkChannelError>>> {
        self.select_next().boxed()
    }

    fn next_event(&mut self, context: &Context) -> Option<BoxEventHandler<'_, Context>> {
        let HostedMapDownlink2 {
            address,
            receiver,
            state,
            next,
            lifecycle,
            dl_state,
            config:
                MapDownlinkConfig {
                    events_when_not_synced,
                    terminate_on_unlinked,
                    ..
                },
            ..
        } = self;
        if let Some(notification) = next.take() {
            match notification {
                Ok(DownlinkNotification::Linked) => {
                    debug!(address = %address, "Downlink linked.");
                    if dl_state.get() == DlState::Unlinked {
                        dl_state.set(DlState::Linked);
                    }
                    Some(lifecycle.on_linked().boxed())
                }
                Ok(DownlinkNotification::Synced) => {
                    debug!(address = %address, "Downlink synced.");
                    dl_state.set(DlState::Synced);
                    Some(state.with(context, |map| lifecycle.on_synced(&map.map).boxed()))
                }
                Ok(DownlinkNotification::Event { body }) => {
                    let maybe_lifecycle =
                        if dl_state.get() == DlState::Synced || *events_when_not_synced {
                            Some(&*lifecycle)
                        } else {
                            None
                        };
                    trace!(address = %address, "Event received for downlink.");

                    match body {
                        MapMessage::Update { key, value } => {
                            trace!("Updating an entry.");
                            state.update(context, key, value, maybe_lifecycle)
                        }
                        MapMessage::Remove { key } => {
                            trace!("Removing an entry.");
                            state.remove(context, key, maybe_lifecycle)
                        }
                        MapMessage::Clear => {
                            trace!("Clearing the map.");
                            let old_map = state.clear(context);
                            maybe_lifecycle.map(|lifecycle| lifecycle.on_clear(old_map).boxed())
                        }
                        MapMessage::Take(n) => {
                            trace!("Retaining the first {} items.", n);
                            state.take(
                                context,
                                n.try_into()
                                    .expect("number to take does not fit into usize"),
                                maybe_lifecycle,
                            )
                        }
                        MapMessage::Drop(n) => {
                            trace!("Dropping the first {} items.", n);
                            state.drop(
                                context,
                                n.try_into()
                                    .expect("number to drop does not fit into usize"),
                                maybe_lifecycle,
                            )
                        }
                    }
                }
                Ok(DownlinkNotification::Unlinked) => {
                    debug!(address = %address, "Downlink unlinked.");
                    if *terminate_on_unlinked {
                        *receiver = None;
                        dl_state.set(DlState::Stopped);
                    } else {
                        dl_state.set(DlState::Unlinked);
                    }
                    state.clear(context);
                    Some(lifecycle.on_unlinked().boxed())
                }
                Err(_) => {
                    debug!(address = %address, "Downlink failed.");
                    if *terminate_on_unlinked {
                        *receiver = None;
                        dl_state.set(DlState::Stopped);
                    } else {
                        dl_state.set(DlState::Unlinked);
                    }
                    state.clear(context);
                    Some(lifecycle.on_failed().boxed())
                }
            }
        } else {
            None
        }
    }
}

impl<K: RecognizerReadable, V: RecognizerReadable, LC, State>
    HostedMapDownlinkChannel<K, V, LC, State>
{
    pub fn new(
        address: Address<Text>,
        receiver: ByteReader,
        lifecycle: LC,
        state: State,
        config: MapDownlinkConfig,
        stop_rx: trigger::Receiver,
        dl_state: Arc<AtomicU8>,
    ) -> Self {
        HostedMapDownlinkChannel {
            address,
            receiver: Some(FramedRead::new(receiver, Default::default())),
            state,
            next: None,
            lifecycle,
            config,
            dl_state: DlStateTracker::new(dl_state),
            stop_rx: Some(stop_rx),
        }
    }
}

impl<K, V, LC, State, Context> DownlinkChannel<Context>
    for HostedMapDownlinkChannel<K, V, LC, State>
where
    State: MapDlStateOps<K, V, Context>,
    K: Hash + Eq + Ord + Clone + RecognizerReadable + Send + 'static,
    V: RecognizerReadable + Send + 'static,
    K::Rec: Send,
    V::Rec: Send,
    LC: MapDownlinkLifecycle<K, V, Context> + 'static,
{
    fn await_ready(&mut self) -> BoxFuture<'_, Option<Result<(), DownlinkFailed>>> {
        let HostedMapDownlinkChannel {
            address,
            receiver,
            next,
            stop_rx,
            ..
        } = self;
        async move {
            let result = if let Some(rx) = receiver {
                if let Some(stop_signal) = stop_rx.as_mut() {
                    tokio::select! {
                        biased;
                        triggered_result = stop_signal => {
                            *stop_rx = None;
                            if triggered_result.is_ok() {
                                *receiver = None;
                                Some(Ok(DownlinkNotification::Unlinked))
                            } else {
                                rx.next().await
                            }
                        }
                        result = rx.next() => result,
                    }
                } else {
                    rx.next().await
                }
            } else {
                return None;
            };
            match result {
                Some(Ok(notification)) => {
                    *next = Some(Ok(notification));
                    Some(Ok(()))
                }
                Some(Err(e)) => {
                    error!(address = %address, "Downlink input channel failed.");
                    *next = Some(Err(e));
                    *receiver = None;
                    Some(Err(DownlinkFailed))
                }
                _ => {
                    info!(address = %address, "Downlink terminated normally.");
                    *receiver = None;
                    None
                }
            }
        }
        .boxed()
    }

    fn next_event(&mut self, context: &Context) -> Option<BoxEventHandler<'_, Context>> {
        let HostedMapDownlinkChannel {
            address,
            receiver,
            state,
            next,
            lifecycle,
            dl_state,
            config:
                MapDownlinkConfig {
                    events_when_not_synced,
                    terminate_on_unlinked,
                    ..
                },
            ..
        } = self;
        if let Some(notification) = next.take() {
            match notification {
                Ok(DownlinkNotification::Linked) => {
                    debug!(address = %address, "Downlink linked.");
                    if dl_state.get() == DlState::Unlinked {
                        dl_state.set(DlState::Linked);
                    }
                    Some(lifecycle.on_linked().boxed())
                }
                Ok(DownlinkNotification::Synced) => {
                    debug!(address = %address, "Downlink synced.");
                    dl_state.set(DlState::Synced);
                    Some(state.with(context, |map| lifecycle.on_synced(&map.map).boxed()))
                }
                Ok(DownlinkNotification::Event { body }) => {
                    let maybe_lifecycle =
                        if dl_state.get() == DlState::Synced || *events_when_not_synced {
                            Some(&*lifecycle)
                        } else {
                            None
                        };
                    trace!(address = %address, "Event received for downlink.");

                    match body {
                        MapMessage::Update { key, value } => {
                            trace!("Updating an entry.");
                            state.update(context, key, value, maybe_lifecycle)
                        }
                        MapMessage::Remove { key } => {
                            trace!("Removing an entry.");
                            state.remove(context, key, maybe_lifecycle)
                        }
                        MapMessage::Clear => {
                            trace!("Clearing the map.");
                            let old_map = state.clear(context);
                            maybe_lifecycle.map(|lifecycle| lifecycle.on_clear(old_map).boxed())
                        }
                        MapMessage::Take(n) => {
                            trace!("Retaining the first {} items.", n);
                            state.take(
                                context,
                                n.try_into()
                                    .expect("number to take does not fit into usize"),
                                maybe_lifecycle,
                            )
                        }
                        MapMessage::Drop(n) => {
                            trace!("Dropping the first {} items.", n);
                            state.drop(
                                context,
                                n.try_into()
                                    .expect("number to drop does not fit into usize"),
                                maybe_lifecycle,
                            )
                        }
                    }
                }
                Ok(DownlinkNotification::Unlinked) => {
                    debug!(address = %address, "Downlink unlinked.");
                    if *terminate_on_unlinked {
                        *receiver = None;
                        dl_state.set(DlState::Stopped);
                    } else {
                        dl_state.set(DlState::Unlinked);
                    }
                    state.clear(context);
                    Some(lifecycle.on_unlinked().boxed())
                }
                Err(_) => {
                    debug!(address = %address, "Downlink failed.");
                    if *terminate_on_unlinked {
                        *receiver = None;
                        dl_state.set(DlState::Stopped);
                    } else {
                        dl_state.set(DlState::Unlinked);
                    }
                    state.clear(context);
                    Some(lifecycle.on_failed().boxed())
                }
            }
        } else {
            None
        }
    }

    fn kind(&self) -> DownlinkKind {
        DownlinkKind::Map
    }
}

/// A handle which can be used to modify the state of a map lane through a downlink.
#[derive(Debug)]
pub struct MapDownlinkHandle<K, V> {
    address: Address<Text>,
    sender: mpsc::UnboundedSender<MapOperation<K, V>>,
    stop_tx: Option<trigger::Sender>,
    observer: DlStateObserver,
}

impl<K, V> MapDownlinkHandle<K, V> {
    pub fn new(
        address: Address<Text>,
        sender: mpsc::UnboundedSender<MapOperation<K, V>>,
        stop_tx: trigger::Sender,
        state: &Arc<AtomicU8>,
    ) -> Self {
        MapDownlinkHandle {
            address,
            sender,
            stop_tx: Some(stop_tx),
            observer: DlStateObserver::new(state),
        }
    }

    /// Instruct the downlink to stop.
    pub fn stop(&mut self) {
        trace!(address = %self.address, "Stopping a map downlink.");
        if let Some(tx) = self.stop_tx.take() {
            tx.trigger();
        }
    }

    /// True if the downlink has stopped (regardless of whether it stopped cleanly or failed.)
    pub fn is_stopped(&self) -> bool {
        self.observer.get() == DlState::Stopped
    }

    /// True if the downlink is running and linked.
    pub fn is_linked(&self) -> bool {
        matches!(self.observer.get(), DlState::Linked | DlState::Synced)
    }
}

impl<K, V> MapDownlinkHandle<K, V>
where
    K: Send + 'static,
    V: Send + 'static,
{
    pub fn update(&self, key: K, value: V) -> Result<(), AgentRuntimeError> {
        trace!(address = %self.address, "Updating an entry on a map downlink.");
        self.sender.send(MapOperation::Update { key, value })?;
        Ok(())
    }

    pub fn remove(&self, key: K) -> Result<(), AgentRuntimeError> {
        trace!(address = %self.address, "Removing an entry on a map downlink.");
        self.sender.send(MapOperation::Remove { key })?;
        Ok(())
    }

    pub fn clear(&self) -> Result<(), AgentRuntimeError> {
        trace!(address = %self.address, "Clearing a map downlink.");
        self.sender.send(MapOperation::Clear)?;
        Ok(())
    }
}

/// The internal state of the [`unfold`] operation used to describe the map downlink writer.
struct WriteStreamState<K, V> {
    rx: mpsc::UnboundedReceiver<MapOperation<K, V>>,
    write: Option<FramedWrite<ByteWriter, MapOperationEncoder>>,
    queue: EventQueue<K, V>,
}

impl<K, V> WriteStreamState<K, V> {
    fn new(writer: ByteWriter, rx: mpsc::UnboundedReceiver<MapOperation<K, V>>) -> Self {
        WriteStreamState {
            rx,
            write: Some(FramedWrite::new(writer, Default::default())),
            queue: Default::default(),
        }
    }
}

/// Task to write the values sent by a map downlink handle to an outgoing channel.
pub fn map_dl_write_stream<K, V>(
    writer: ByteWriter,
    rx: mpsc::UnboundedReceiver<MapOperation<K, V>>,
) -> impl Stream<Item = Result<(), std::io::Error>> + Send + 'static
where
    K: Clone + Eq + Hash + StructuralWritable + Send + 'static,
    V: StructuralWritable + Send + 'static,
{
    let state = WriteStreamState::<K, V>::new(writer, rx);

    unfold(state, |mut state| async move {
        let WriteStreamState { rx, write, queue } = &mut state;
        if let Some(writer) = write {
            let first = if let Some(op) = queue.pop() {
                op
            } else if let Some(op) = rx.recv().await {
                op
            } else {
                *write = None;
                return None;
            };
            trace!("Writing a value to a value downlink.");
            let mut write_fut = pin!(writer.send(first));
            let result = loop {
                let recv = pin!(rx.recv());
                match select(write_fut.as_mut(), recv).await {
                    Either::Left((Ok(_), _)) => break Some(Ok(())),
                    Either::Left((Err(e), _)) => {
                        *write = None;
                        break Some(Err(e));
                    }
                    Either::Right((Some(op), _)) => {
                        trace!("Pushing an event into the queue as the writer is busy.");
                        queue.push(op);
                    }
                    _ => {
                        *write = None;
                        break None;
                    }
                }
            };
            result.map(move |r| (r, state))
        } else {
            None
        }
    })
}
