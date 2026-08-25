//! Deployment-owned task lifecycle for work that may retain an MCP pool.

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, Weak,
    },
};

use tokio::{sync::Notify, task::AbortHandle};
use tokio_util::task::TaskTracker;

pub use peri_acp_types::ports::McpTaskShutdownReport;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum McpTaskKey {
    Initialize,
    OAuth(String),
    Reconnect(String),
    Subscription(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerPhase {
    Open,
    Closing,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskPhase {
    Running,
    Stopping,
    Finished,
}

struct Completion {
    done: AtomicBool,
    notify: Notify,
}

impl Completion {
    fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    fn finish(&self) {
        self.done.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        while !self.done.load(Ordering::Acquire) {
            let notified = self.notify.notified();
            if self.done.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
    }
}

struct TaskRecord {
    generation: u64,
    phase: TaskPhase,
    abort: AbortHandle,
    completion: Arc<Completion>,
}

struct McpTaskState {
    phase: OwnerPhase,
    next_generation: u64,
    records: HashMap<McpTaskKey, Vec<TaskRecord>>,
}

struct McpTaskInner {
    state: Mutex<McpTaskState>,
    tracker: TaskTracker,
}

/// Sole deployment-held owner. Intentionally not Clone.
pub struct McpTaskOwner {
    inner: Option<Arc<McpTaskInner>>,
}

/// Weak task admission handle held by the pool and callbacks.
#[derive(Clone)]
pub struct McpTaskSpawner {
    inner: Weak<McpTaskInner>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpTaskScopeClosed;

struct CompletionGuard {
    key: McpTaskKey,
    generation: u64,
    completion: Arc<Completion>,
    inner: Weak<McpTaskInner>,
}

impl Drop for CompletionGuard {
    fn drop(&mut self) {
        self.completion.finish();
        if let Some(inner) = self.inner.upgrade() {
            let mut state = inner.state.lock().expect("MCP task state poisoned");
            if let Some(records) = state.records.get_mut(&self.key) {
                if let Some(record) = records
                    .iter_mut()
                    .find(|record| record.generation == self.generation)
                {
                    record.phase = TaskPhase::Finished;
                }
            }
        }
    }
}

impl McpTaskOwner {
    pub fn new() -> (Self, McpTaskSpawner) {
        let inner = Arc::new(McpTaskInner {
            state: Mutex::new(McpTaskState {
                phase: OwnerPhase::Open,
                next_generation: 1,
                records: HashMap::new(),
            }),
            tracker: TaskTracker::new(),
        });
        (
            Self {
                inner: Some(inner.clone()),
            },
            McpTaskSpawner {
                inner: Arc::downgrade(&inner),
            },
        )
    }

    pub fn begin_shutdown(&self) {
        if let Some(inner) = self.inner.as_ref() {
            let mut state = inner.state.lock().expect("MCP task state poisoned");
            if state.phase == OwnerPhase::Open {
                state.phase = OwnerPhase::Closing;
                inner.tracker.close();
            }
        }
    }

    pub async fn shutdown(&mut self) -> McpTaskShutdownReport {
        let Some(inner) = self.inner.as_ref().cloned() else {
            return McpTaskShutdownReport::Complete;
        };
        self.begin_shutdown();
        abort_all(&inner);
        inner.tracker.wait().await;
        let mut state = inner.state.lock().expect("MCP task state poisoned");
        state.phase = OwnerPhase::Closed;
        state.records.clear();
        McpTaskShutdownReport::Complete
    }

    #[cfg(test)]
    pub(crate) fn active_count(&self) -> usize {
        self.inner
            .as_ref()
            .map(|inner| {
                inner
                    .state
                    .lock()
                    .expect("MCP task state poisoned")
                    .records
                    .values()
                    .flatten()
                    .filter(|record| record.phase != TaskPhase::Finished)
                    .count()
            })
            .unwrap_or_default()
    }
}

impl Drop for McpTaskOwner {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            {
                let mut state = inner.state.lock().expect("MCP task state poisoned");
                if state.phase == OwnerPhase::Open {
                    state.phase = OwnerPhase::Closing;
                    inner.tracker.close();
                }
            }
            abort_all(&inner);
        }
    }
}

#[async_trait::async_trait]
impl peri_acp_types::ports::McpTaskOwnerPort for McpTaskOwner {
    fn begin_shutdown(&self) {
        McpTaskOwner::begin_shutdown(self);
    }

    async fn shutdown(&mut self) -> McpTaskShutdownReport {
        McpTaskOwner::shutdown(self).await
    }
}

impl McpTaskSpawner {
    pub(crate) fn closed() -> Self {
        Self { inner: Weak::new() }
    }

    pub fn spawn<F>(&self, key: McpTaskKey, future: F) -> Result<(), McpTaskScopeClosed>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let inner = self.inner.upgrade().ok_or(McpTaskScopeClosed)?;
        let mut state = inner.state.lock().expect("MCP task state poisoned");
        if state.phase != OwnerPhase::Open {
            return Err(McpTaskScopeClosed);
        }
        compact_finished(&mut state, &key);
        if state.records.get(&key).is_some_and(|records| {
            records
                .iter()
                .any(|record| record.phase != TaskPhase::Finished)
        }) {
            return Err(McpTaskScopeClosed);
        }
        let generation = state.next_generation;
        state.next_generation = state.next_generation.wrapping_add(1).max(1);
        let completion = Arc::new(Completion::new());
        // Constructed synchronously before spawn and captured initialized. An
        // abort before first poll still drops it and settles completion.
        let guard = CompletionGuard {
            key: key.clone(),
            generation,
            completion: completion.clone(),
            inner: Arc::downgrade(&inner),
        };
        let join = inner.tracker.spawn(async move {
            let _completion_guard = guard;
            future.await;
        });
        state.records.entry(key).or_default().push(TaskRecord {
            generation,
            phase: TaskPhase::Running,
            abort: join.abort_handle(),
            completion,
        });
        drop(join);
        Ok(())
    }

    pub async fn stop_key(&self, key: &McpTaskKey) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let controls: Vec<_> = {
            let mut state = inner.state.lock().expect("MCP task state poisoned");
            let Some(records) = state.records.get_mut(key) else {
                return;
            };
            records
                .iter_mut()
                .filter(|record| record.phase != TaskPhase::Finished)
                .map(|record| {
                    record.phase = TaskPhase::Stopping;
                    (record.abort.clone(), record.completion.clone())
                })
                .collect()
        };
        for (abort, _) in &controls {
            abort.abort();
        }
        for (_, completion) in controls {
            completion.wait().await;
        }
        let mut state = inner.state.lock().expect("MCP task state poisoned");
        compact_finished(&mut state, key);
    }
}

fn compact_finished(state: &mut McpTaskState, key: &McpTaskKey) {
    if let Some(records) = state.records.get_mut(key) {
        records.retain(|record| record.phase != TaskPhase::Finished);
        if records.is_empty() {
            state.records.remove(key);
        }
    }
}

fn abort_all(inner: &McpTaskInner) {
    let aborts: Vec<_> = inner
        .state
        .lock()
        .expect("MCP task state poisoned")
        .records
        .values_mut()
        .flatten()
        .filter(|record| record.phase != TaskPhase::Finished)
        .map(|record| {
            record.phase = TaskPhase::Stopping;
            record.abort.clone()
        })
        .collect();
    for abort in aborts {
        abort.abort();
    }
}

#[cfg(test)]
#[path = "task_scope_test.rs"]
mod tests;
