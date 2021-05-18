// Copyright 2015-2021 SWIM.AI inc.
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

use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use store::{serialize, StoreError, StoreInfo};
use swim_common::model::text::Text;

use crate::plane::store::PlaneStore;
use crate::store::{StoreEngine, StoreKey};
use futures::future::BoxFuture;
use futures::FutureExt;
use store::engines::{KeyedSnapshot, RangedSnapshotLoad};
use store::keyspaces::KeyType;

pub mod mock;
#[cfg(test)]
mod tests;

/// A trait for defining store engines which open stores for nodes.
///
/// Node stores are responsible for ensuring that the data models that they open for their lanes
/// correctly map their keys to their delegate store engines.
///
/// # Data models
/// Data models may be either persistent or transient. Persistent data models are backed by a store
/// in which operations on persistent data models are mapped to the correct node and delegated to
/// the store; providing that the top-level server store is also persistent.
///
/// Transient data models will live in memory for the duration that a handle to the model exists.
pub trait NodeStore:
    StoreEngine + RangedSnapshotLoad + Send + Sync + Clone + Debug + 'static
{
    type Delegate: PlaneStore;

    /// Returns information about the delegate store
    fn store_info(&self) -> StoreInfo;

    fn lane_id_of<I>(&self, lane: I) -> BoxFuture<KeyType>
    where
        I: Into<String>;
}

/// A node store which is used to open value and map lane data models.
pub struct SwimNodeStore<D> {
    /// The plane store that value and map data models will delegate their store engine operations
    /// to.
    delegate: Arc<D>,
    /// The node URI that this store represents.
    node_uri: Text,
}

impl<D> Debug for SwimNodeStore<D> {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        f.debug_struct("SwimNodeStore")
            .field("node_uri", &self.node_uri)
            .finish()
    }
}

impl<D> Clone for SwimNodeStore<D> {
    fn clone(&self) -> Self {
        SwimNodeStore {
            delegate: self.delegate.clone(),
            node_uri: self.node_uri.clone(),
        }
    }
}

impl<D: PlaneStore> SwimNodeStore<D> {
    /// Create a new Swim node store which will delegate its engine operations to `delegate` and
    /// represents a node at `node_uri`.
    pub fn new<I: Into<Text>>(delegate: D, node_uri: I) -> SwimNodeStore<D> {
        SwimNodeStore {
            delegate: Arc::new(delegate),
            node_uri: node_uri.into(),
        }
    }
}

impl<D: PlaneStore> RangedSnapshotLoad for SwimNodeStore<D> {
    type Prefix = StoreKey;

    fn load_ranged_snapshot<F, K, V>(
        &self,
        prefix: Self::Prefix,
        map_fn: F,
    ) -> Result<Option<KeyedSnapshot<K, V>>, StoreError>
    where
        F: for<'i> Fn(&'i [u8], &'i [u8]) -> Result<(K, V), StoreError>,
    {
        let keyspace = prefix.keyspace_name();
        let prefix = serialize(&prefix)?;
        self.delegate
            .keyspace_load_ranged_snapshot(&keyspace, prefix.as_slice(), map_fn)
    }
}

impl<D: PlaneStore> StoreEngine for SwimNodeStore<D> {
    fn put(&self, key: StoreKey, value: &[u8]) -> Result<(), StoreError> {
        self.delegate.put(key, value)
    }

    fn get(&self, key: StoreKey) -> Result<Option<Vec<u8>>, StoreError> {
        self.delegate.get(key)
    }

    fn delete(&self, key: StoreKey) -> Result<(), StoreError> {
        self.delegate.delete(key)
    }
}

impl<D: PlaneStore> NodeStore for SwimNodeStore<D> {
    type Delegate = D;

    fn store_info(&self) -> StoreInfo {
        self.delegate.store_info()
    }

    fn lane_id_of<I>(&self, lane: I) -> BoxFuture<'_, u64>
    where
        I: Into<String>,
    {
        self.delegate.lane_id_of(lane).boxed()
    }
}
