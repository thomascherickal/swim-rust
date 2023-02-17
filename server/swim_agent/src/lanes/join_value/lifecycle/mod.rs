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

use std::{borrow::Borrow, marker::PhantomData};

use swim_api::handlers::{BorrowHandler, FnHandler, NoHandler};
use swim_model::address::Address;

use crate::{
    agent_lifecycle::utility::HandlerContext,
    event_handler::{EventHandler, HandlerAction},
    lifecycle_fn::{LiftShared, WithHandlerContext, WithHandlerContextBorrow},
};

use self::{
    on_failed::{OnJoinValueFailed, OnJoinValueFailedShared},
    on_linked::{OnJoinValueLinked, OnJoinValueLinkedShared},
    on_synced::{OnJoinValueSynced, OnJoinValueSyncedShared},
    on_unlinked::{OnJoinValueUnlinked, OnJoinValueUnlinkedShared},
};

pub mod on_failed;
pub mod on_linked;
pub mod on_synced;
pub mod on_unlinked;

pub trait JoinValueLaneLifecycle<K, V, Context>:
    OnJoinValueLinked<K, Context>
    + OnJoinValueSynced<K, V, Context>
    + OnJoinValueUnlinked<K, Context>
    + OnJoinValueFailed<K, Context>
{
}

impl<K, V, Context, L> JoinValueLaneLifecycle<K, V, Context> for L where
    L: OnJoinValueLinked<K, Context>
        + OnJoinValueSynced<K, V, Context>
        + OnJoinValueUnlinked<K, Context>
        + OnJoinValueFailed<K, Context>
{
}

pub trait JoinValueLaneLifecycleShared<K, V, Context, Shared>:
    OnJoinValueLinkedShared<K, Context, Shared>
    + OnJoinValueSyncedShared<K, V, Context, Shared>
    + OnJoinValueUnlinkedShared<K, Context, Shared>
    + OnJoinValueFailedShared<K, Context, Shared>
{
}

impl<K, V, Context, Shared, L> JoinValueLaneLifecycleShared<K, V, Context, Shared> for L where
    L: OnJoinValueLinkedShared<K, Context, Shared>
        + OnJoinValueSyncedShared<K, V, Context, Shared>
        + OnJoinValueUnlinkedShared<K, Context, Shared>
        + OnJoinValueFailedShared<K, Context, Shared>
{
}

pub trait StatelessJoinValueLifecycle<Context, K, V>:
    JoinValueLaneLifecycle<K, V, Context>
{
    type WithOnLinked<H>: StatelessJoinValueLifecycle<Context, K, V>
    where
        H: OnJoinValueLinked<K, Context>;

    type WithOnSynced<H>: StatelessJoinValueLifecycle<Context, K, V>
    where
        H: OnJoinValueSynced<K, V, Context>;

    type WithOnUnlinked<H>: StatelessJoinValueLifecycle<Context, K, V>
    where
        H: OnJoinValueUnlinked<K, Context>;

    type WithOnFailed<H>: StatelessJoinValueLifecycle<Context, K, V>
    where
        H: OnJoinValueFailed<K, Context>;

    type WithShared<Shared>: StatefulJoinValueLifecycle<K, V, Context, Shared>
    where
        Shared: Send;

    fn on_linked<F>(self, handler: F) -> Self::WithOnLinked<WithHandlerContext<F>>
    where
        WithHandlerContext<F>: OnJoinValueLinked<K, Context>;

    fn on_synced<F, B>(self, handler: F) -> Self::WithOnSynced<WithHandlerContextBorrow<F, B>>
    where
        B: ?Sized,
        V: Borrow<B>,
        WithHandlerContextBorrow<F, B>: OnJoinValueSynced<K, V, Context>;

    fn on_unlinked<F>(self, handler: F) -> Self::WithOnUnlinked<WithHandlerContext<F>>
    where
        WithHandlerContext<F>: OnJoinValueUnlinked<K, Context>;

    fn on_failed<F>(self, handler: F) -> Self::WithOnFailed<WithHandlerContext<F>>
    where
        WithHandlerContext<F>: OnJoinValueFailed<K, Context>;

    fn with_shared_state<Shared: Send>(self, shared: Shared) -> Self::WithShared<Shared>;
}

pub trait StatefulJoinValueLifecycle<K, V, Context, Shared> {
    type WithOnLinked<H>: StatefulJoinValueLifecycle<K, V, Context, Shared>
    where
        H: OnJoinValueLinkedShared<K, Context, Shared>;

    type WithOnSynced<H>: StatefulJoinValueLifecycle<K, V, Context, Shared>
    where
        H: OnJoinValueSyncedShared<K, V, Context, Shared>;

    type WithOnUnlinked<H>: StatefulJoinValueLifecycle<K, V, Context, Shared>
    where
        H: OnJoinValueUnlinkedShared<K, Context, Shared>;

    type WithOnFailed<H>: StatefulJoinValueLifecycle<K, V, Context, Shared>
    where
        H: OnJoinValueFailedShared<K, Context, Shared>;

    fn on_linked<F>(self, handler: F) -> Self::WithOnLinked<FnHandler<F>>
    where
        FnHandler<F>: OnJoinValueLinkedShared<K, Context, Shared>;

    fn on_synced<F, B>(self, handler: F) -> Self::WithOnSynced<BorrowHandler<F, B>>
    where
        B: ?Sized,
        V: Borrow<B>,
        BorrowHandler<F, B>: OnJoinValueSyncedShared<K, V, Context, Shared>;

    fn on_unlinked<F>(self, handler: F) -> Self::WithOnUnlinked<FnHandler<F>>
    where
        FnHandler<F>: OnJoinValueUnlinkedShared<K, Context, Shared>;

    fn on_failed<F>(self, handler: F) -> Self::WithOnFailed<FnHandler<F>>
    where
        FnHandler<F>: OnJoinValueFailedShared<K, Context, Shared>;
}

pub struct StatelessJoinValueLaneLifecycle<
    Context,
    K,
    V,
    FLinked = NoHandler,
    FSynced = NoHandler,
    FUnlinked = NoHandler,
    FFailed = NoHandler,
> {
    _type: PhantomData<fn(&Context, K, V)>,
    on_linked: FLinked,
    on_synced: FSynced,
    on_unlinked: FUnlinked,
    on_failed: FFailed,
}

pub struct StatefulJoinValueLaneLifecycle<
    Context,
    State,
    K,
    V,
    FLinked = NoHandler,
    FSynced = NoHandler,
    FUnlinked = NoHandler,
    FFailed = NoHandler,
> {
    _type: PhantomData<fn(K, V)>,
    state: State,
    handler_context: HandlerContext<Context>,
    on_linked: FLinked,
    on_synced: FSynced,
    on_unlinked: FUnlinked,
    on_failed: FFailed,
}

impl<Context, K, V> Default for StatelessJoinValueLaneLifecycle<Context, K, V> {
    fn default() -> Self {
        Self {
            _type: Default::default(),
            on_linked: Default::default(),
            on_synced: Default::default(),
            on_unlinked: Default::default(),
            on_failed: Default::default(),
        }
    }
}

impl<Context, State, K, V> StatefulJoinValueLaneLifecycle<Context, State, K, V> {
    pub fn new(state: State) -> Self {
        StatefulJoinValueLaneLifecycle {
            _type: PhantomData,
            state,
            handler_context: Default::default(),
            on_linked: Default::default(),
            on_synced: Default::default(),
            on_unlinked: Default::default(),
            on_failed: Default::default(),
        }
    }
}

impl<Context, State, K, V, FLinked, FSynced, FUnlinked, FFailed> Clone
    for StatefulJoinValueLaneLifecycle<Context, State, K, V, FLinked, FSynced, FUnlinked, FFailed>
where
    State: Clone,
    FLinked: Clone,
    FSynced: Clone,
    FUnlinked: Clone,
    FFailed: Clone,
{
    fn clone(&self) -> Self {
        Self {
            _type: PhantomData,
            state: self.state.clone(),
            handler_context: Default::default(),
            on_linked: self.on_linked.clone(),
            on_synced: self.on_synced.clone(),
            on_unlinked: self.on_unlinked.clone(),
            on_failed: self.on_failed.clone(),
        }
    }
}

impl<Context, K, V, FLinked, FSynced, FUnlinked, FFailed> Clone
    for StatelessJoinValueLaneLifecycle<Context, K, V, FLinked, FSynced, FUnlinked, FFailed>
where
    FLinked: Clone,
    FSynced: Clone,
    FUnlinked: Clone,
    FFailed: Clone,
{
    fn clone(&self) -> Self {
        Self {
            _type: PhantomData,
            on_linked: self.on_linked.clone(),
            on_synced: self.on_synced.clone(),
            on_unlinked: self.on_unlinked.clone(),
            on_failed: self.on_failed.clone(),
        }
    }
}

pub trait JoinValueHandlerFn0<'a, Context, Shared, K, Out> {
    type Handler: HandlerAction<Context, Completion = Out> + 'a;

    fn make_handler(
        &'a self,
        shared: &'a Shared,
        handler_context: HandlerContext<Context>,
        key: K,
        remote: Address<&str>,
    ) -> Self::Handler;
}

impl<'a, Context, Shared, K, F, H, Out> JoinValueHandlerFn0<'a, Context, Shared, K, Out> for F
where
    F: Fn(&'a Shared, HandlerContext<Context>, K, Address<&str>) -> H,
    H: HandlerAction<Context, Completion = Out> + 'a,
{
    type Handler = H;

    fn make_handler(
        &'a self,
        shared: &'a Shared,
        handler_context: HandlerContext<Context>,
        key: K,
        remote: Address<&str>,
    ) -> Self::Handler {
        (*self)(shared, handler_context, key, remote)
    }
}

pub trait JoinValueSyncFn<'a, Context, Shared, K, V: ?Sized> {
    type Handler: EventHandler<Context> + 'a;

    fn make_handler(
        &'a self,
        shared: &'a Shared,
        handler_context: HandlerContext<Context>,
        key: K,
        remote: Address<&str>,
        value: Option<&V>,
    ) -> Self::Handler;
}

impl<'a, Context, Shared, K, V, F, H> JoinValueSyncFn<'a, Context, Shared, K, V> for F
where
    V: ?Sized,
    F: Fn(&'a Shared, HandlerContext<Context>, K, Address<&str>, Option<&V>) -> H,
    H: EventHandler<Context> + 'a,
{
    type Handler = H;

    fn make_handler(
        &'a self,
        shared: &'a Shared,
        handler_context: HandlerContext<Context>,
        key: K,
        remote: Address<&str>,
        value: Option<&V>,
    ) -> Self::Handler {
        (*self)(shared, handler_context, key, remote, value)
    }
}

impl<Context, State, K, V, FLinked, FSynced, FUnlinked, FFailed> OnJoinValueLinked<K, Context>
    for StatefulJoinValueLaneLifecycle<Context, State, K, V, FLinked, FSynced, FUnlinked, FFailed>
where
    State: Send,
    FLinked: OnJoinValueLinkedShared<K, Context, State>,
    FSynced: Send,
    FUnlinked: Send,
    FFailed: Send,
{
    type OnJoinValueLinkedHandler<'a> = FLinked::OnJoinValueLinkedHandler<'a>
    where
        Self: 'a;

    fn on_linked<'a>(
        &'a self,
        key: K,
        remote: Address<&str>,
    ) -> Self::OnJoinValueLinkedHandler<'a> {
        let StatefulJoinValueLaneLifecycle {
            on_linked,
            state,
            handler_context,
            ..
        } = self;
        on_linked.on_linked(state, *handler_context, key, remote)
    }
}

impl<Context, State, K, V, FLinked, FSynced, FUnlinked, FFailed> OnJoinValueSynced<K, V, Context>
    for StatefulJoinValueLaneLifecycle<Context, State, K, V, FLinked, FSynced, FUnlinked, FFailed>
where
    State: Send,
    FLinked: Send,
    FSynced: OnJoinValueSyncedShared<K, V, Context, State>,
    FUnlinked: Send,
    FFailed: Send,
{
    type OnJoinValueSyncedHandler<'a> = FSynced::OnJoinValueSyncedHandler<'a>
    where
        Self: 'a;

    fn on_synced<'a>(
        &'a self,
        key: K,
        remote: Address<&str>,
        value: Option<&V>,
    ) -> Self::OnJoinValueSyncedHandler<'a> {
        let StatefulJoinValueLaneLifecycle {
            on_synced,
            state,
            handler_context,
            ..
        } = self;
        on_synced.on_synced(state, *handler_context, key, remote, value)
    }
}

impl<Context, State, K, V, FLinked, FSynced, FUnlinked, FFailed> OnJoinValueUnlinked<K, Context>
    for StatefulJoinValueLaneLifecycle<Context, State, K, V, FLinked, FSynced, FUnlinked, FFailed>
where
    State: Send,
    FLinked: Send,
    FSynced: Send,
    FUnlinked: OnJoinValueUnlinkedShared<K, Context, State>,
    FFailed: Send,
{
    type OnJoinValueUnlinkedHandler<'a> = FUnlinked::OnJoinValueUnlinkedHandler<'a>
    where
        Self: 'a;

    fn on_unlinked<'a>(
        &'a self,
        key: K,
        remote: Address<&str>,
    ) -> Self::OnJoinValueUnlinkedHandler<'a> {
        let StatefulJoinValueLaneLifecycle {
            on_unlinked,
            state,
            handler_context,
            ..
        } = self;
        on_unlinked.on_unlinked(state, *handler_context, key, remote)
    }
}

impl<Context, State, K, V, FLinked, FSynced, FUnlinked, FFailed> OnJoinValueFailed<K, Context>
    for StatefulJoinValueLaneLifecycle<Context, State, K, V, FLinked, FSynced, FUnlinked, FFailed>
where
    State: Send,
    FLinked: Send,
    FSynced: Send,
    FUnlinked: Send,
    FFailed: OnJoinValueFailedShared<K, Context, State>,
{
    type OnJoinValueFailedHandler<'a> = FFailed::OnJoinValueFailedHandler<'a>
    where
        Self: 'a;

    fn on_failed<'a>(
        &'a self,
        key: K,
        remote: Address<&str>,
    ) -> Self::OnJoinValueFailedHandler<'a> {
        let StatefulJoinValueLaneLifecycle {
            on_failed,
            state,
            handler_context,
            ..
        } = self;
        on_failed.on_failed(state, *handler_context, key, remote)
    }
}

impl<Context, State, K, V, FLinked, FSynced, FUnlinked, FFailed>
    StatefulJoinValueLifecycle<K, V, Context, State>
    for StatefulJoinValueLaneLifecycle<Context, State, K, V, FLinked, FSynced, FUnlinked, FFailed>
where
    State: Send,
    FLinked: OnJoinValueLinkedShared<K, Context, State>,
    FSynced: OnJoinValueSyncedShared<K, V, Context, State>,
    FUnlinked: OnJoinValueUnlinkedShared<K, Context, State>,
    FFailed: OnJoinValueFailedShared<K, Context, State>,
{
    type WithOnLinked<H> = StatefulJoinValueLaneLifecycle<Context, State, K, V, H, FSynced, FUnlinked, FFailed>
    where
        H: OnJoinValueLinkedShared<K, Context, State>;

    type WithOnSynced<H> = StatefulJoinValueLaneLifecycle<Context, State, K, V, FLinked, H, FUnlinked, FFailed>
    where
        H: OnJoinValueSyncedShared<K, V, Context, State>;

    type WithOnUnlinked<H> = StatefulJoinValueLaneLifecycle<Context, State, K, V, FLinked, FSynced, H, FFailed>
    where
        H: OnJoinValueUnlinkedShared<K, Context, State>;

    type WithOnFailed<H> = StatefulJoinValueLaneLifecycle<Context, State, K, V, FLinked, FSynced, FUnlinked, H>
    where
        H: OnJoinValueFailedShared<K, Context, State>;

    fn on_linked<F>(self, handler: F) -> Self::WithOnLinked<FnHandler<F>>
    where
        FnHandler<F>: OnJoinValueLinkedShared<K, Context, State>,
    {
        StatefulJoinValueLaneLifecycle {
            _type: PhantomData,
            state: self.state,
            handler_context: self.handler_context,
            on_linked: FnHandler(handler),
            on_synced: self.on_synced,
            on_unlinked: self.on_unlinked,
            on_failed: self.on_failed,
        }
    }

    fn on_synced<F, B>(self, handler: F) -> Self::WithOnSynced<BorrowHandler<F, B>>
    where
        B: ?Sized,
        V: Borrow<B>,
        BorrowHandler<F, B>: OnJoinValueSyncedShared<K, V, Context, State>,
    {
        StatefulJoinValueLaneLifecycle {
            _type: PhantomData,
            state: self.state,
            handler_context: self.handler_context,
            on_linked: self.on_linked,
            on_synced: BorrowHandler::new(handler),
            on_unlinked: self.on_unlinked,
            on_failed: self.on_failed,
        }
    }

    fn on_unlinked<F>(self, handler: F) -> Self::WithOnUnlinked<FnHandler<F>>
    where
        FnHandler<F>: OnJoinValueUnlinkedShared<K, Context, State>,
    {
        StatefulJoinValueLaneLifecycle {
            _type: PhantomData,
            state: self.state,
            handler_context: self.handler_context,
            on_linked: self.on_linked,
            on_synced: self.on_synced,
            on_unlinked: FnHandler(handler),
            on_failed: self.on_failed,
        }
    }

    fn on_failed<F>(self, handler: F) -> Self::WithOnFailed<FnHandler<F>>
    where
        FnHandler<F>: OnJoinValueFailedShared<K, Context, State>,
    {
        StatefulJoinValueLaneLifecycle {
            _type: PhantomData,
            state: self.state,
            handler_context: self.handler_context,
            on_linked: self.on_linked,
            on_synced: self.on_synced,
            on_unlinked: self.on_unlinked,
            on_failed: FnHandler(handler),
        }
    }
}

impl<Context, K, V, FLinked, FSynced, FUnlinked, FFailed> OnJoinValueLinked<K, Context>
    for StatelessJoinValueLaneLifecycle<Context, K, V, FLinked, FSynced, FUnlinked, FFailed>
where
    FLinked: OnJoinValueLinked<K, Context>,
    FSynced: Send,
    FUnlinked: Send,
    FFailed: Send,
{
    type OnJoinValueLinkedHandler<'a> = FLinked::OnJoinValueLinkedHandler<'a>
    where
        Self: 'a;

    fn on_linked<'a>(
        &'a self,
        key: K,
        remote: Address<&str>,
    ) -> Self::OnJoinValueLinkedHandler<'a> {
        let StatelessJoinValueLaneLifecycle { on_linked, .. } = self;
        on_linked.on_linked(key, remote)
    }
}

impl<Context, K, V, FLinked, FSynced, FUnlinked, FFailed> OnJoinValueSynced<K, V, Context>
    for StatelessJoinValueLaneLifecycle<Context, K, V, FLinked, FSynced, FUnlinked, FFailed>
where
    FLinked: Send,
    FSynced: OnJoinValueSynced<K, V, Context>,
    FUnlinked: Send,
    FFailed: Send,
{
    type OnJoinValueSyncedHandler<'a> = FSynced::OnJoinValueSyncedHandler<'a>
    where
        Self: 'a;

    fn on_synced<'a>(
        &'a self,
        key: K,
        remote: Address<&str>,
        value: Option<&V>,
    ) -> Self::OnJoinValueSyncedHandler<'a> {
        let StatelessJoinValueLaneLifecycle { on_synced, .. } = self;
        on_synced.on_synced(key, remote, value)
    }
}

impl<Context, K, V, FLinked, FSynced, FUnlinked, FFailed> OnJoinValueUnlinked<K, Context>
    for StatelessJoinValueLaneLifecycle<Context, K, V, FLinked, FSynced, FUnlinked, FFailed>
where
    FLinked: Send,
    FSynced: Send,
    FUnlinked: OnJoinValueUnlinked<K, Context>,
    FFailed: Send,
{
    type OnJoinValueUnlinkedHandler<'a> = FUnlinked::OnJoinValueUnlinkedHandler<'a>
    where
        Self: 'a;

    fn on_unlinked<'a>(
        &'a self,
        key: K,
        remote: Address<&str>,
    ) -> Self::OnJoinValueUnlinkedHandler<'a> {
        let StatelessJoinValueLaneLifecycle { on_unlinked, .. } = self;
        on_unlinked.on_unlinked(key, remote)
    }
}

impl<Context, K, V, FLinked, FSynced, FUnlinked, FFailed> OnJoinValueFailed<K, Context>
    for StatelessJoinValueLaneLifecycle<Context, K, V, FLinked, FSynced, FUnlinked, FFailed>
where
    FLinked: Send,
    FSynced: Send,
    FUnlinked: Send,
    FFailed: OnJoinValueFailed<K, Context>,
{
    type OnJoinValueFailedHandler<'a> = FFailed::OnJoinValueFailedHandler<'a>
    where
        Self: 'a;

    fn on_failed<'a>(
        &'a self,
        key: K,
        remote: Address<&str>,
    ) -> Self::OnJoinValueFailedHandler<'a> {
        let StatelessJoinValueLaneLifecycle { on_failed, .. } = self;
        on_failed.on_failed(key, remote)
    }
}

impl<Context, K, V, FLinked, FSynced, FUnlinked, FFailed> StatelessJoinValueLifecycle<Context, K, V>
    for StatelessJoinValueLaneLifecycle<Context, K, V, FLinked, FSynced, FUnlinked, FFailed>
where
    FLinked: OnJoinValueLinked<K, Context>,
    FSynced: OnJoinValueSynced<K, V, Context>,
    FUnlinked: OnJoinValueUnlinked<K, Context>,
    FFailed: OnJoinValueFailed<K, Context>,
{
    type WithOnLinked<H> = StatelessJoinValueLaneLifecycle<Context, K, V, H, FSynced, FUnlinked, FFailed>
    where
        H: OnJoinValueLinked<K, Context>;

    type WithOnSynced<H> = StatelessJoinValueLaneLifecycle<Context, K, V, FLinked, H, FUnlinked, FFailed>
    where
        H: OnJoinValueSynced<K, V, Context>;

    type WithOnUnlinked<H> = StatelessJoinValueLaneLifecycle<Context, K, V, FLinked, FSynced, H, FFailed>
    where
        H: OnJoinValueUnlinked<K, Context>;

    type WithOnFailed<H> = StatelessJoinValueLaneLifecycle<Context, K, V, FLinked, FSynced, FUnlinked, H>
    where
        H: OnJoinValueFailed<K, Context>;

    type WithShared<Shared> = StatefulJoinValueLaneLifecycle<
        Context,
        Shared,
        K,
        V,
        LiftShared<FLinked, Shared>,
        LiftShared<FSynced, Shared>,
        LiftShared<FUnlinked, Shared>,
        LiftShared<FFailed, Shared>>
    where
        Shared: Send;

    fn on_linked<F>(self, handler: F) -> Self::WithOnLinked<WithHandlerContext<F>>
    where
        WithHandlerContext<F>: OnJoinValueLinked<K, Context>,
    {
        StatelessJoinValueLaneLifecycle {
            _type: PhantomData,
            on_linked: WithHandlerContext::new(handler),
            on_synced: self.on_synced,
            on_unlinked: self.on_unlinked,
            on_failed: self.on_failed,
        }
    }

    fn on_synced<F, B>(self, handler: F) -> Self::WithOnSynced<WithHandlerContextBorrow<F, B>>
    where
        B: ?Sized,
        V: Borrow<B>,
        WithHandlerContextBorrow<F, B>: OnJoinValueSynced<K, V, Context>,
    {
        StatelessJoinValueLaneLifecycle {
            _type: PhantomData,
            on_linked: self.on_linked,
            on_synced: WithHandlerContextBorrow::new(handler),
            on_unlinked: self.on_unlinked,
            on_failed: self.on_failed,
        }
    }

    fn on_unlinked<F>(self, handler: F) -> Self::WithOnUnlinked<WithHandlerContext<F>>
    where
        WithHandlerContext<F>: OnJoinValueUnlinked<K, Context>,
    {
        StatelessJoinValueLaneLifecycle {
            _type: PhantomData,
            on_linked: self.on_linked,
            on_synced: self.on_synced,
            on_unlinked: WithHandlerContext::new(handler),
            on_failed: self.on_failed,
        }
    }

    fn on_failed<F>(self, handler: F) -> Self::WithOnFailed<WithHandlerContext<F>>
    where
        WithHandlerContext<F>: OnJoinValueFailed<K, Context>,
    {
        StatelessJoinValueLaneLifecycle {
            _type: PhantomData,
            on_linked: self.on_linked,
            on_synced: self.on_synced,
            on_unlinked: self.on_unlinked,
            on_failed: WithHandlerContext::new(handler),
        }
    }

    fn with_shared_state<Shared: Send>(self, shared: Shared) -> Self::WithShared<Shared> {
        StatefulJoinValueLaneLifecycle {
            _type: PhantomData,
            state: shared,
            handler_context: Default::default(),
            on_linked: LiftShared::new(self.on_linked),
            on_synced: LiftShared::new(self.on_synced),
            on_unlinked: LiftShared::new(self.on_unlinked),
            on_failed: LiftShared::new(self.on_failed),
        }
    }
}
