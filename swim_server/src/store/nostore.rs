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

use crate::store::keystore::{KeyRequest, KeystoreTask};
use futures::future::BoxFuture;
use futures::{Stream, StreamExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use store::engines::{FromKeyspaces, KeyedSnapshot, RangedSnapshotLoad};
use store::iterator::{
    EngineIterOpts, EngineIterator, EnginePrefixIterator, EngineRefIterator, IteratorKey,
};
use store::keyspaces::{Keyspace, KeyspaceByteEngine, KeyspaceResolver, Keyspaces};
use store::{
    EngineInfo, EngineIterOpts, EngineIterator, EnginePrefixIterator, EngineRefIterator,
    IteratorKey, KvPair, Store, StoreBuilder, StoreError,
};

/// A store which will persist no data and exists purely to uphold the minimum contract required
/// between a lane and its store.
#[derive(Debug, Clone)]
pub struct NoStore {
    path: PathBuf,
    ks: (),
}

impl Store for NoStore {
    fn path(&self) -> &Path {
        &self.path
    }

    fn engine_info(&self) -> EngineInfo {
        EngineInfo {
            path: "no store".to_string(),
            kind: "no store".to_string(),
        }
    }
}

pub struct NoStoreIterator;
impl EngineIterator for NoStoreIterator {
    fn seek_to(&mut self, _key: IteratorKey) -> Result<bool, StoreError> {
        Ok(false)
    }

    fn seek_next(&mut self) {}

    fn key(&self) -> Option<&[u8]> {
        None
    }

    fn value(&self) -> Option<&[u8]> {
        None
    }

    fn valid(&self) -> Result<bool, StoreError> {
        Ok(false)
    }
}

impl EnginePrefixIterator for NoStoreIterator {
    fn next(&mut self) -> Option<KvBytes> {
        None
    }

    fn valid(&self) -> Result<bool, StoreError> {
        Ok(false)
    }
}

impl<'a: 'b, 'b> EngineRefIterator<'a, 'b> for NoStore {
    type EngineIterator = NoStoreIterator;
    type EnginePrefixIterator = NoStoreIterator;

    fn iterator_opt(
        &'a self,
        _space: &'b Self::ResolvedKeyspace,
        _opts: EngineIterOpts,
    ) -> Result<Self::EngineIterator, StoreError> {
        Ok(NoStoreIterator)
    }

    fn prefix_iterator_opt(
        &'a self,
        _space: &'b Self::ResolvedKeyspace,
        _opts: EngineIterOpts,
        _prefix: &'b [u8],
    ) -> Result<Self::EnginePrefixIterator, StoreError> {
        Ok(NoStoreIterator)
    }
}

#[derive(Default, Clone)]
pub struct NoStoreOpts;
impl StoreBuilder for NoStoreOpts {
    type Store = NoStore;

    fn build<I>(self, _path: I, _keyspaces: &Keyspaces<Self>) -> Result<Self::Store, StoreError>
    where
        I: AsRef<Path>,
    {
        Ok(NoStore {
            path: PathBuf::from("Transient".to_string()),
            ks: (),
        })
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
        _step: u64,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    fn get_prefix_range<F, K, V, S>(
        &self,
        _keyspace: S,
        _prefix: &[u8],
        _map_fn: F,
    ) -> Result<Option<Vec<(K, V)>>, StoreError>
    where
        F: for<'i> Fn(&'i [u8], &'i [u8]) -> Result<(K, V), StoreError>,
        S: Keyspace,
    {
        Ok(None)
    }
}

impl KeyspaceResolver for NoStore {
    type ResolvedKeyspace = ();

    fn resolve_keyspace<K: Keyspace>(&self, _space: &K) -> Option<&Self::ResolvedKeyspace> {
        Some(&self.ks)
    }
}

impl KeystoreTask for NoStore {
    fn run<DB, S>(_: Arc<DB>, events: S) -> BoxFuture<'static, Result<(), StoreError>>
    where
        DB: KeyspaceByteEngine,
        S: Stream<Item = KeyRequest> + Unpin + Send + 'static,
    {
        Box::pin(async move {
            let _ = events.for_each(|(_, responder)| async {
                let _ = responder.send(0);
            });
            Ok(())
        })
    }
}
