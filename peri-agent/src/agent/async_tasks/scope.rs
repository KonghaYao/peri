use std::future::Future;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use peri_acp_types::tasks::ExternalExecutionGuard;
use tokio_util::sync::CancellationToken;
use tokio_util::task::{task_tracker::TaskTrackerToken, TaskTracker};

/// Owns completion evidence separately from the user-visible task registry.
///
/// Uncertainty is recorded per external execution scope (the MCP owner the call
/// went to, or `workspace` for injected shell execution), not as a single
/// permanent flag: a cancelled or timed-out call marks only its own scope, and
/// a conclusive reconciliation of that scope clears the record. Without such
/// evidence the scope stays non-idle (design §5: missing evidence must not be
/// reported as complete).
pub(super) struct ExecutionScope {
    open: parking_lot::Mutex<bool>,
    tracker: TaskTracker,
    uncertain: parking_lot::Mutex<std::collections::HashMap<u64, String>>,
    next_guard_id: AtomicU64,
    cancel: CancellationToken,
}

impl ExecutionScope {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            open: parking_lot::Mutex::new(true),
            tracker: TaskTracker::new(),
            uncertain: parking_lot::Mutex::new(std::collections::HashMap::new()),
            next_guard_id: AtomicU64::new(1),
            cancel: CancellationToken::new(),
        })
    }

    pub(super) fn admit(&self) -> Result<parking_lot::MutexGuard<'_, bool>, String> {
        let guard = self.open.lock();
        if !*guard {
            return Err("session execution scope is closing".into());
        }
        Ok(guard)
    }

    pub(super) fn begin_external(
        self: &Arc<Self>,
        scope: &str,
    ) -> Result<Box<dyn ExternalExecutionGuard>, String> {
        let _admission = self.admit()?;
        Ok(Box::new(ExternalGuard {
            scope_owner: Arc::clone(self),
            _token: self.tracker.token(),
            id: self.next_guard_id.fetch_add(1, Ordering::Relaxed),
            scope: scope.to_owned(),
            stopped: false,
        }))
    }

    pub(super) fn spawn(
        self: &Arc<Self>,
        task: impl Future<Output = ()> + Send + 'static,
    ) -> Result<tokio::task::JoinHandle<()>, String> {
        let _admission = self.admit()?;
        Ok(self.spawn_admitted(task))
    }

    /// Used under admission or while draining an execution already owned by this scope.
    pub(super) fn spawn_admitted(
        self: &Arc<Self>,
        task: impl Future<Output = ()> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let mut completion = ExternalGuard {
            scope_owner: Arc::clone(self),
            _token: self.tracker.token(),
            id: self.next_guard_id.fetch_add(1, Ordering::Relaxed),
            scope: "owned".into(),
            stopped: false,
        };
        tokio::spawn(async move {
            task.await;
            completion.confirm_stopped();
        })
    }

    pub(super) fn close(&self) {
        *self.open.lock() = false;
        self.tracker.close();
        self.cancel.cancel();
    }

    pub(super) fn cancel_token(&self) -> CancellationToken {
        self.cancel.child_token()
    }

    pub(super) async fn wait(&self) -> bool {
        if peri_time::timeout(std::time::Duration::from_secs(5), self.tracker.wait())
            .await
            .is_err()
        {
            return false;
        }
        self.uncertain.lock().is_empty()
    }

    pub(super) fn is_idle(&self) -> bool {
        self.tracker.is_empty() && self.uncertain.lock().is_empty()
    }

    pub(super) fn is_closed(&self) -> bool {
        !*self.open.lock()
    }

    /// Conclusive reconciliation for one external scope clears its uncertainty.
    ///
    /// Callers must hold evidence that the scope has no live execution (for
    /// example a fully applied Workspace task snapshot for that owner); an empty
    /// local directory is not evidence.
    pub(super) fn resolve_external_evidence(&self, scope: &str) -> usize {
        let mut uncertain = self.uncertain.lock();
        let before = uncertain.len();
        uncertain.retain(|_, recorded| recorded != scope);
        before - uncertain.len()
    }
}

struct ExternalGuard {
    scope_owner: Arc<ExecutionScope>,
    _token: TaskTrackerToken,
    id: u64,
    scope: String,
    stopped: bool,
}

impl ExternalExecutionGuard for ExternalGuard {
    fn confirm_stopped(&mut self) {
        self.stopped = true;
    }
}

impl Drop for ExternalGuard {
    fn drop(&mut self) {
        if !self.stopped {
            self.scope_owner
                .uncertain
                .lock()
                .insert(self.id, self.scope.clone());
        }
    }
}
