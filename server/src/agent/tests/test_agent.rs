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

use crate::agent;
use crate::agent::lane::lifecycle::{
    ActionLaneLifecycle, StatefulLaneLifecycle, StatefulLaneLifecycleBase,
};
use crate::agent::lane::model::action::{ActionLane, CommandLane};
use crate::agent::lane::model::map::{MapLane, MapLaneEvent};
use crate::agent::lane::model::value::ValueLane;
use crate::agent::lane::strategy::Queue;
use crate::agent::lane::tests::ExactlyOnce;
use crate::agent::lifecycle::AgentLifecycle;
use crate::agent::{AgentContext, LaneTasks, SwimAgent};
use futures::future::{ready, Ready};
use futures_util::future::BoxFuture;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;
use stm::stm::Stm;
use stm::transaction::atomically;
use tokio::sync::{mpsc, Mutex};

/// An agent for use in tests of the agent execution loop. All events that occur in the lifecycle
/// vents of the agent and its lanes are reported on an MPSC channel. When the agent starts it
/// creates a periodic schedule that fires every second. For each event of the schedule, an entry
/// will be inserted into the the `data` lane with keys "Name0", "Name1" and so forth. For each
/// entry inserted, the value of the `total` lane will be incremented by the inserted value.
#[derive(Debug)]
pub struct TestAgent {
    data: MapLane<String, i32>,
    total: ValueLane<i32>,
    action: CommandLane<String>,
}

/// Type of the events that will be reported by the agent.
#[derive(Debug, PartialEq, Eq)]
pub enum TestAgentEvent {
    AgentStart,
    Command(String),
    TransactionFailed,
    DataEvent(MapLaneEvent<String, i32>),
    TotalEvent(i32),
}

/// Collects the events from the agent life-cycles.
#[derive(Debug)]
pub struct EventCollector {
    events: mpsc::Sender<TestAgentEvent>,
}

impl EventCollector {
    pub fn new(events: mpsc::Sender<TestAgentEvent>) -> Self {
        EventCollector { events }
    }
}

#[derive(Clone, Debug)]
struct TestLifecycleInner(Arc<Mutex<EventCollector>>);

impl TestLifecycleInner {
    /// Push an event into the channel.
    async fn push(&self, event: TestAgentEvent) {
        self.0
            .lock()
            .await
            .events
            .send(event)
            .await
            .expect("Event receiver was dropped.")
    }
}

#[derive(Debug)]
struct TestAgentLifecycle {
    inner: TestLifecycleInner,
}
#[derive(Debug)]
struct DataLifecycle {
    inner: TestLifecycleInner,
}
#[derive(Debug)]
struct TotalLifecycle {
    inner: TestLifecycleInner,
}
#[derive(Debug)]
struct ActionLifecycle {
    inner: TestLifecycleInner,
}

impl AgentLifecycle<TestAgent> for TestAgentLifecycle {
    fn on_start<'a, C: AgentContext<TestAgent>>(&'a self, context: &'a C) -> BoxFuture<'a, ()>
    where
        C: AgentContext<TestAgent> + Send + Sync + 'a,
    {
        Box::pin(async move {
            self.inner.push(TestAgentEvent::AgentStart).await;

            let mut count = 0;
            let cmd = context.agent().action.clone();

            context
                .periodically(
                    move || {
                        let index = count;
                        count += 1;

                        let key = format!("Name{}", index);
                        let mut commander = cmd.commander();

                        Box::pin(async move {
                            commander.command(key).await;
                        })
                    },
                    Duration::from_secs(1),
                    None,
                )
                .await;
        })
    }
}

impl<'a> ActionLaneLifecycle<'a, String, (), TestAgent> for ActionLifecycle {
    type ResponseFuture = BoxFuture<'a, ()>;

    fn on_command<C>(
        &'a self,
        command: String,
        _model: &'a ActionLane<String, ()>,
        context: &'a C,
    ) -> Self::ResponseFuture
    where
        C: AgentContext<TestAgent> + Send + Sync + 'static,
    {
        Box::pin(async move {
            self.inner
                .push(TestAgentEvent::Command(command.clone()))
                .await;
            if context
                .agent()
                .data
                .update_direct(command, 1.into())
                .apply(ExactlyOnce)
                .await
                .is_err()
            {
                self.inner.push(TestAgentEvent::TransactionFailed).await;
            }
        })
    }
}

impl StatefulLaneLifecycleBase for DataLifecycle {
    type WatchStrategy = Queue;

    fn create_strategy(&self) -> Self::WatchStrategy {
        Queue::default()
    }
}

impl<'a> StatefulLaneLifecycle<'a, MapLane<String, i32>, TestAgent> for DataLifecycle {
    type StartFuture = Ready<()>;
    type EventFuture = BoxFuture<'a, ()>;

    fn on_start<C>(&'a self, _model: &'a MapLane<String, i32>, _context: &'a C) -> Self::StartFuture
    where
        C: AgentContext<TestAgent> + Send + Sync + 'a,
    {
        ready(())
    }

    fn on_event<C>(
        &'a self,
        event: &'a MapLaneEvent<String, i32>,
        _model: &'a MapLane<String, i32>,
        context: &'a C,
    ) -> Self::EventFuture
    where
        C: AgentContext<TestAgent> + Send + Sync + 'static,
    {
        Box::pin(async move {
            self.inner
                .push(TestAgentEvent::DataEvent(event.clone()))
                .await;
            if let MapLaneEvent::Update(_, v) = event {
                let i = **v;

                let total = &context.agent().total;

                let add = total.get().and_then(move |n| total.set(*n + i));

                if atomically(&add, ExactlyOnce).await.is_err() {
                    self.inner.push(TestAgentEvent::TransactionFailed).await;
                }
            }
        })
    }
}

impl StatefulLaneLifecycleBase for TotalLifecycle {
    type WatchStrategy = Queue;

    fn create_strategy(&self) -> Self::WatchStrategy {
        Queue::default()
    }
}

impl<'a> StatefulLaneLifecycle<'a, ValueLane<i32>, TestAgent> for TotalLifecycle {
    type StartFuture = Ready<()>;
    type EventFuture = BoxFuture<'a, ()>;

    fn on_start<C>(&'a self, _model: &'a ValueLane<i32>, _context: &'a C) -> Self::StartFuture
    where
        C: AgentContext<TestAgent> + Send + Sync + 'a,
    {
        ready(())
    }

    fn on_event<C>(
        &'a self,
        event: &Arc<i32>,
        _model: &'a ValueLane<i32>,
        _context: &'a C,
    ) -> Self::EventFuture
    where
        C: AgentContext<TestAgent> + Send + Sync + 'static,
    {
        let n = **event;
        Box::pin(async move {
            self.inner.push(TestAgentEvent::TotalEvent(n)).await;
        })
    }
}

/// The event reporter is injected into the agent as ersatz configuration.
#[derive(Debug)]
pub struct TestAgentConfig {
    collector: Arc<Mutex<EventCollector>>,
    command_buffer_size: NonZeroUsize,
}

impl TestAgentConfig {
    pub fn new(sender: mpsc::Sender<TestAgentEvent>) -> Self {
        TestAgentConfig {
            collector: Arc::new(Mutex::new(EventCollector::new(sender))),
            command_buffer_size: NonZeroUsize::new(5).unwrap(),
        }
    }
}

impl SwimAgent<TestAgentConfig> for TestAgent {
    fn instantiate<Context: AgentContext<Self>>(
        configuration: &TestAgentConfig,
    ) -> (Self, Vec<Box<dyn LaneTasks<Self, Context>>>)
    where
        Context: AgentContext<Self> + Send + Sync + 'static,
    {
        let TestAgentConfig {
            collector,
            command_buffer_size,
        } = configuration;

        let inner = TestLifecycleInner(collector.clone());

        let (data, data_tasks) = agent::make_map_lane(
            DataLifecycle {
                inner: inner.clone(),
            },
            |agent: &TestAgent| &agent.data,
        );

        let (total, total_tasks) = agent::make_value_lane(
            0,
            TotalLifecycle {
                inner: inner.clone(),
            },
            |agent: &TestAgent| &agent.total,
        );

        let (action, action_tasks) = agent::make_command_lane(
            ActionLifecycle {
                inner: inner.clone(),
            },
            |agent: &TestAgent| &agent.action,
            *command_buffer_size,
        );

        let agent = TestAgent {
            data,
            total,
            action,
        };

        let tasks = vec![
            data_tasks.boxed(),
            total_tasks.boxed(),
            action_tasks.boxed(),
        ];
        (agent, tasks)
    }
}
