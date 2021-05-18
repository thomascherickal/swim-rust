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

use crate::engines::{ByteEngine, KeyedSnapshot, RangedSnapshotLoad};
use crate::iterator::{
    EngineIterOpts, EngineIterator, EnginePrefixIterator, EngineRefIterator, IteratorKey,
};
use crate::keyspaces::{KeyType, Keyspace, KeyspaceByteEngine, KeyspaceResolver, Keyspaces};
use crate::{FromKeyspaces, KvBytes, Store, StoreError, StoreInfo};
use std::borrow::Borrow;
use std::path::{Path, PathBuf};

/// A delegate store database that does nothing.
#[derive(Debug, Clone)]
pub struct NoStore {
    /// A path stub that is the name of the plane that created it.
    pub(crate) path: PathBuf,
}

impl Store for NoStore {
    fn path(&self) -> &Path {
        self.path.borrow()
    }

    fn store_info(&self) -> StoreInfo {
        StoreInfo {
            path: self.path.to_string_lossy().to_string(),
            kind: "NoStore".to_string(),
        }
    }
}

impl KeyspaceResolver for NoStore {
    type ResolvedKeyspace = ();

    fn resolve_keyspace<K: Keyspace>(&self, _space: &K) -> Option<&Self::ResolvedKeyspace> {
        None
    }
}

impl KeyspaceByteEngine for NoStore {
    fn put_keyspace<K: Keyspace>(
        &self,
        _keyspace: K,
        _key: &[u8],
        _value: &[u8],
    ) -> Result<(), StoreError> {
        Ok(())
    }

    fn get_keyspace<K: Keyspace>(
        &self,
        _keyspace: K,
        _key: &[u8],
    ) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(None)
    }

    fn delete_keyspace<K: Keyspace>(&self, _keyspace: K, _key: &[u8]) -> Result<(), StoreError> {
        Ok(())
    }

    fn merge_keyspace<K: Keyspace>(
        &self,
        _keyspace: K,
        _key: &[u8],
        _value: KeyType,
    ) -> Result<(), StoreError> {
        Ok(())
    }
}

impl RangedSnapshotLoad for NoStore {
    fn load_ranged_snapshot<F, K, V, S>(
        &self,
        _keyspace: S,
        _prefix: &[u8],
        _map_fn: F,
    ) -> Result<Option<KeyedSnapshot<K, V>>, StoreError>
    where
        F: for<'i> Fn(&'i [u8], &'i [u8]) -> Result<(K, V), StoreError>,
        S: Keyspace,
    {
        Ok(None)
    }
}

#[derive(Default, Clone)]
pub struct NoStoreOpts;

impl FromKeyspaces for NoStore {
    type Opts = NoStoreOpts;

    fn from_keyspaces<I: AsRef<Path>>(
        path: I,
        _db_opts: &Self::Opts,
        _keyspaces: Keyspaces<Self>,
    ) -> Result<Self, StoreError> {
        Ok(NoStore {
            path: path.as_ref().to_path_buf(),
        })
    }
}

impl ByteEngine for NoStore {
    /// Put operation does nothing and that always succeeds.
    fn put(&self, _key: &[u8], _value: &[u8]) -> Result<(), StoreError> {
        Ok(())
    }

    /// Get operation does nothing and always returns `Ok(None)`.
    fn get(&self, _key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(None)
    }

    /// Delete operation does nothing and always returns `Ok(())`.
    fn delete(&self, _key: &[u8]) -> Result<(), StoreError> {
        Ok(())
    }
}

pub struct NoStoreEngineIterator;
impl EngineIterator for NoStoreEngineIterator {
    fn seek_to(&mut self, _key: IteratorKey) -> Result<bool, StoreError> {
        Ok(true)
    }

    fn seek_next(&mut self) {}

    fn key(&self) -> Option<&[u8]> {
        None
    }

    fn value(&self) -> Option<&[u8]> {
        None
    }

    fn valid(&self) -> Result<bool, StoreError> {
        Ok(true)
    }
}

pub struct NoStoreEnginePrefixIterator;
impl EnginePrefixIterator for NoStoreEnginePrefixIterator {
    fn next(&mut self) -> Option<KvBytes> {
        None
    }

    fn valid(&self) -> Result<bool, StoreError> {
        Ok(false)
    }
}

impl<'a: 'b, 'b> EngineRefIterator<'a, 'b> for NoStore {
    type EngineIterator = NoStoreEngineIterator;
    type EnginePrefixIterator = NoStoreEnginePrefixIterator;

    fn iterator_opt(
        &'a self,
        _space: &'b Self::ResolvedKeyspace,
        _pts: EngineIterOpts,
    ) -> Result<Self::EngineIterator, StoreError> {
        Ok(NoStoreEngineIterator)
    }

    fn prefix_iterator_opt(
        &'a self,
        _space: &'b Self::ResolvedKeyspace,
        _opts: EngineIterOpts,
        _prefix: &'b [u8],
    ) -> Result<Self::EnginePrefixIterator, StoreError> {
        Ok(NoStoreEnginePrefixIterator)
    }
}
