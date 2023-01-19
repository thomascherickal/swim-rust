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

use std::{collections::HashMap, borrow::Borrow, hash::Hash};

use swim_api::protocol::map::MapOperation;

use crate::lanes::map::MapLaneEvent;

#[derive(Debug)]
pub struct MapStoreInner<K, V, Q> {
    content: HashMap<K, V>,
    previous: Option<MapLaneEvent<K, V>>,
    queue: Q,
}

pub trait MapEventQueue<K, V>: Default {

    type Output<'a>
    where
        K:'a,
        V: 'a,
        Self: 'a;

    fn push(&mut self, action: MapOperation<K, ()>);
    fn is_empty(&self) -> bool;

    fn pop<'a>(&mut self, content: &'a HashMap<K, V>) -> Option<Self::Output<'a>>;
}

impl<K, V, Q: Default> MapStoreInner<K, V, Q> {

    pub fn new(content: HashMap<K, V>) -> Self {
        MapStoreInner { content, previous: Default::default(), queue: Default::default() }
    }

}

impl<K, V, Q> MapStoreInner<K, V, Q>
where
    K: Eq + Hash + Clone,
    Q: MapEventQueue<K, V>,
{

    pub fn init(&mut self, map: HashMap<K, V>) {
        self.content = map;
    }

    pub fn update(&mut self, key: K, value: V) {
        let MapStoreInner {
            content,
            previous,
            queue,
        } = self;
        let prev = content.insert(key.clone(), value);
        *previous = Some(MapLaneEvent::Update(key.clone(), prev));
        queue.push(MapOperation::Update { key, value: () });
    }

    pub fn remove(&mut self, key: &K) {
        let MapStoreInner {
            content,
            previous,
            queue,
        } = self;
        let prev = content.remove(key);
        if let Some(prev) = prev {
            *previous = Some(MapLaneEvent::Remove(key.clone(), prev));
            queue.push(MapOperation::Remove { key: key.clone() });
        }
    }

    pub fn clear(&mut self) {
        let MapStoreInner {
            content,
            previous,
            queue,
        } = self;
        *previous = Some(MapLaneEvent::Clear(std::mem::take(content)));
        queue.push(MapOperation::Clear);
    }

    pub fn get<B, F, R>(&self, key: &B, f: F) -> R
    where
        K: Borrow<B>,
        B: Hash + Eq,
        F: FnOnce(Option<&V>) -> R,
    {
        let MapStoreInner { content, .. } = self;
        f(content.get(key))
    }

    pub fn get_map<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&HashMap<K, V>) -> R,
    {
        let MapStoreInner { content, .. } = self;
        f(content)
    }

    pub fn read_with_prev<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(Option<MapLaneEvent<K, V>>, &HashMap<K, V>) -> R,
    {
        let MapStoreInner {
            content, previous, ..
        } = self;
        f(previous.take(), content)
    }

    pub fn queue(&mut self) -> &mut Q {
        &mut self.queue
    }

    pub fn pop_operation(&mut self) -> Option<Q::Output<'_>> {
        let MapStoreInner { content, queue, .. } = self;
        queue.pop(content)
    }

}

