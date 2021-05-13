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

pub mod keystore;

use crate::plane::store::{PlaneStore, SwimPlaneStore};
use serde::{Deserialize, Serialize};
use std::fmt::{Debug, Formatter};
use std::io;
use std::marker::PhantomData;
use std::path::PathBuf;
use store::keyspaces::{KeyType, Keyspaces};
use store::{Store, StoreError};
use utilities::fs::Dir;

/// A Swim server store which will create plane stores on demand.
///
/// When a new plane store is requested, then the implementor is expected to either load the plane
/// from a delegate database or create a new database on demand.
pub trait SwimStore {
    /// The type of plane stores that are created.
    type PlaneStore: PlaneStore;

    /// Create a plane store with `plane_name`.
    ///
    /// # Errors
    /// Errors if the delegate database could not be created.
    fn plane_store<I>(&mut self, plane_name: I) -> Result<Self::PlaneStore, StoreError>
    where
        I: ToString;
}

/// A Swim server store that will open plane stores on request.
pub struct ServerStore<D: Store> {
    /// The directory that this store is operating from.
    dir: Dir,
    /// Database environment open options
    db_opts: D::Opts,
    /// The keyspaces that all stores will be opened with.
    keyspaces: Keyspaces<D>,
    _delegate_pd: PhantomData<D>,
}

impl<D: Store> Debug for ServerStore<D> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerStore")
            .field("directory", &self.dir.path())
            .finish()
    }
}

impl<D: Store> ServerStore<D> {
    /// Constructs a new server store that will open stores using `opts` and will use the directory
    /// `base_path` for opening all new stores.
    ///
    /// # Panics
    /// Panics if the directory cannot be created.
    pub fn new(
        db_opts: D::Opts,
        keyspaces: Keyspaces<D>,
        base_path: PathBuf,
    ) -> io::Result<ServerStore<D>> {
        Ok(ServerStore {
            dir: Dir::persistent(base_path)?,
            db_opts,
            keyspaces,
            _delegate_pd: Default::default(),
        })
    }

    /// Constructs a new transient server store that will clear the directory (prefixed by `prefix`
    /// when dropped and open stores using `opts`.
    ///
    /// # Panics
    /// Panics if the directory cannot be created.
    pub fn transient(
        db_opts: D::Opts,
        keyspaces: Keyspaces<D>,
        prefix: &str,
    ) -> io::Result<ServerStore<D>> {
        Ok(ServerStore {
            dir: Dir::transient(prefix)?,
            db_opts,
            keyspaces,
            _delegate_pd: Default::default(),
        })
    }
}

impl<D: Store> SwimStore for ServerStore<D> {
    type PlaneStore = SwimPlaneStore<D>;

    fn plane_store<I: ToString>(&mut self, plane_name: I) -> Result<Self::PlaneStore, StoreError> {
        let ServerStore {
            db_opts,
            keyspaces,
            dir,
            ..
        } = self;
        let plane_name = plane_name.to_string();

        SwimPlaneStore::open(dir.path(), &plane_name, db_opts, keyspaces)
    }
}

/// A lane key that is either a map lane key or a value lane key.
#[derive(Serialize, Deserialize, Clone, Debug, PartialOrd, PartialEq)]
pub enum StoreKey {
    /// A map lane key.
    ///
    /// Within plane stores, map lane keys are defined in the format of `/lane_id/key` where `key`
    /// is the key of a lane's map data structure.
    Map {
        /// The lane ID.
        lane_id: KeyType,
        /// An optional, serialized, key. This is optional as ranged snapshots to not require the
        /// key.
        #[serde(skip_serializing_if = "Option::is_none")]
        key: Option<Vec<u8>>,
    },
    /// A value lane key.
    Value {
        /// The lane ID.
        lane_id: KeyType,
    },
}

pub trait StoreEngine {
    /// Put a key-value pair into the delegate store.
    fn put(&self, key: StoreKey, value: &[u8]) -> Result<(), StoreError>;

    /// Get a value keyed by a store key from the delegate store.
    fn get(&self, key: StoreKey) -> Result<Option<Vec<u8>>, StoreError>;

    /// Delete a key-value pair by its store key from the delegate store.
    fn delete(&self, key: StoreKey) -> Result<(), StoreError>;
}
