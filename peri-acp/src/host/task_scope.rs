//! Structured ownership for background work admitted by the ACP host.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use peri_acp_types::{dynamic_mcp::DynamicMcpShutdownReport, ports::McpPoolShutdownReport};
use tokio::task::AbortHandle;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostTaskOwnerKind {
    Startup,
    Host,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostTaskKind {
    CronTick,
    PluginCleanup,
    OAuthConsumer,
    ContinuationScheduler,
    ContinuationTurn,
    Prompt,
    Prediction,
    LegacyCancelHook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    Open,
    Closing,
    Closed,
}

#[derive(Debug)]
struct TaskRecord {
    owner: HostTaskOwnerKind,
    kind: HostTaskKind,
    abort: AbortHandle,
}

#[derive(Debug)]
struct HostTaskState {
    admission: Admission,
    tasks: Vec<TaskRecord>,
}

struct HostTaskInner {
    state: Mutex<HostTaskState>,
    tracker: TaskTracker,
    shutdown: CancellationToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitPhase {
    Cooperative,
    AbortDrain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitOutcome {
    Drained,
    Expired,
}

type DrainFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
type WaitFuture<'a> = Pin<Box<dyn Future<Output = WaitOutcome> + Send + 'a>>;

trait HostWaitDriver: Send + Sync {
    fn race<'a>(
        &'a self,
        phase: WaitPhase,
        duration: Duration,
        drain: DrainFuture<'a>,
    ) -> WaitFuture<'a>;
}

struct TokioWaitDriver;

impl HostWaitDriver for TokioWaitDriver {
    fn race<'a>(
        &'a self,
        _phase: WaitPhase,
        duration: Duration,
        drain: DrainFuture<'a>,
    ) -> WaitFuture<'a> {
        Box::pin(async move {
            match tokio::time::timeout(duration, drain).await {
                Ok(()) => WaitOutcome::Drained,
                Err(_) => WaitOutcome::Expired,
            }
        })
    }
}

/// Sole strong owner of host-scoped tasks. This type is intentionally not Clone.
pub(crate) struct HostTaskOwner {
    inner: Option<Arc<HostTaskInner>>,
    cooperative_grace: Duration,
    abort_drain_guard: Duration,
    wait_driver: Arc<dyn HostWaitDriver>,
}

/// Non-owning task admission handle safe to capture from host tasks/configuration.
#[derive(Clone)]
pub(crate) struct HostTaskSpawner {
    inner: Weak<HostTaskInner>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostScopeClosed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostTerminalShutdownReport {
    Complete {
        host: HostShutdownReport,
        dynamic_mcp: DynamicMcpShutdownReport,
        mcp_pool: McpPoolShutdownReport,
        session_close_failures: usize,
    },
    Incomplete {
        host: HostShutdownReport,
        dynamic_mcp: DynamicMcpShutdownReport,
        mcp_pool: McpPoolShutdownReport,
        session_close_failures: usize,
    },
}

impl HostTerminalShutdownReport {
    pub(crate) fn aggregate(
        host: HostShutdownReport,
        dynamic_mcp: DynamicMcpShutdownReport,
        mcp_pool: McpPoolShutdownReport,
        session_close_failures: usize,
    ) -> Self {
        let complete = matches!(host, HostShutdownReport::Complete)
            && matches!(dynamic_mcp, DynamicMcpShutdownReport::Complete)
            && mcp_pool.is_complete()
            && session_close_failures == 0;
        if complete {
            Self::Complete {
                host,
                dynamic_mcp,
                mcp_pool,
                session_close_failures,
            }
        } else {
            Self::Incomplete {
                host,
                dynamic_mcp,
                mcp_pool,
                session_close_failures,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostShutdownReport {
    Complete,
    Incomplete { unfinished: usize },
}

impl HostTaskOwner {
    pub(crate) fn new() -> (Self, HostTaskSpawner) {
        Self::with_policy(Duration::from_secs(5), Duration::from_secs(5))
    }

    fn with_policy(
        cooperative_grace: Duration,
        abort_drain_guard: Duration,
    ) -> (Self, HostTaskSpawner) {
        Self::with_wait_driver(
            cooperative_grace,
            abort_drain_guard,
            Arc::new(TokioWaitDriver),
        )
    }

    fn with_wait_driver(
        cooperative_grace: Duration,
        abort_drain_guard: Duration,
        wait_driver: Arc<dyn HostWaitDriver>,
    ) -> (Self, HostTaskSpawner) {
        let inner = Arc::new(HostTaskInner {
            state: Mutex::new(HostTaskState {
                admission: Admission::Open,
                tasks: Vec::new(),
            }),
            tracker: TaskTracker::new(),
            shutdown: CancellationToken::new(),
        });
        let spawner = HostTaskSpawner {
            inner: Arc::downgrade(&inner),
        };
        (
            Self {
                inner: Some(inner),
                cooperative_grace,
                abort_drain_guard,
                wait_driver,
            },
            spawner,
        )
    }

    pub(crate) fn begin_shutdown(&self) {
        if let Some(inner) = self.inner.as_ref() {
            begin_shutdown(inner);
        }
    }

    pub(crate) async fn shutdown(&mut self) -> HostShutdownReport {
        let Some(inner) = self.inner.as_ref().cloned() else {
            return HostShutdownReport::Complete;
        };
        begin_shutdown(&inner);
        if self
            .wait_driver
            .race(
                WaitPhase::Cooperative,
                self.cooperative_grace,
                Box::pin(inner.tracker.wait()),
            )
            .await
            == WaitOutcome::Expired
        {
            abort_remaining(&inner);
            if self
                .wait_driver
                .race(
                    WaitPhase::AbortDrain,
                    self.abort_drain_guard,
                    Box::pin(inner.tracker.wait()),
                )
                .await
                == WaitOutcome::Expired
            {
                return HostShutdownReport::Incomplete {
                    unfinished: active_count(&inner),
                };
            }
        }
        let mut state = inner.state.lock().expect("host task state poisoned");
        state.admission = Admission::Closed;
        state.tasks.clear();
        HostShutdownReport::Complete
    }
}

impl Drop for HostTaskOwner {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            begin_shutdown(&inner);
            abort_remaining(&inner);
        }
    }
}

impl HostTaskSpawner {
    pub(crate) fn spawn<F>(
        &self,
        owner: HostTaskOwnerKind,
        kind: HostTaskKind,
        future: F,
    ) -> Result<(), HostScopeClosed>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let inner = self.inner.upgrade().ok_or(HostScopeClosed)?;
        let mut state = inner.state.lock().expect("host task state poisoned");
        if state.admission != Admission::Open {
            return Err(HostScopeClosed);
        }
        state.tasks.retain(|task| !task.abort.is_finished());
        let join = inner.tracker.spawn(future);
        state.tasks.push(TaskRecord {
            owner,
            kind,
            abort: join.abort_handle(),
        });
        drop(join);
        Ok(())
    }

    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.inner
            .upgrade()
            .map(|inner| inner.shutdown.clone())
            .unwrap_or_else(|| {
                let token = CancellationToken::new();
                token.cancel();
                token
            })
    }

    #[cfg(test)]
    fn snapshot(&self) -> Option<(Admission, usize)> {
        self.inner.upgrade().map(|inner| {
            let state = inner.state.lock().expect("host task state poisoned");
            (state.admission, state.tasks.len())
        })
    }
}

fn begin_shutdown(inner: &HostTaskInner) {
    let mut state = inner.state.lock().expect("host task state poisoned");
    if state.admission == Admission::Open {
        state.admission = Admission::Closing;
        inner.tracker.close();
        inner.shutdown.cancel();
    }
}

fn abort_remaining(inner: &HostTaskInner) {
    let handles: Vec<_> = inner
        .state
        .lock()
        .expect("host task state poisoned")
        .tasks
        .iter()
        .filter(|task| !task.abort.is_finished())
        .map(|task| {
            tracing::debug!(owner = ?task.owner, kind = ?task.kind, "aborting host-owned task");
            task.abort.clone()
        })
        .collect();
    for handle in handles {
        handle.abort();
    }
}

fn active_count(inner: &HostTaskInner) -> usize {
    inner
        .state
        .lock()
        .expect("host task state poisoned")
        .tasks
        .iter()
        .filter(|task| !task.abort.is_finished())
        .count()
}

#[cfg(test)]
#[path = "task_scope_test.rs"]
mod tests;
