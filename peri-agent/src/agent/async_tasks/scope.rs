use std::future::Future;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use peri_acp_types::tasks::ExternalExecutionGuard;
use tokio_util::sync::CancellationToken;
use tokio_util::task::{task_tracker::TaskTrackerToken, TaskTracker};

/// Owns completion evidence separately from the user-visible task registry.
pub(super) struct ExecutionScope {
    open: parking_lot::Mutex<bool>,
    tracker: TaskTracker,
    uncertain: AtomicBool,
    cancel: CancellationToken,
}

impl ExecutionScope {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            open: parking_lot::Mutex::new(true),
            tracker: TaskTracker::new(),
            uncertain: AtomicBool::new(false),
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
    ) -> Result<Box<dyn ExternalExecutionGuard>, String> {
        let _admission = self.admit()?;
        Ok(Box::new(ExternalGuard {
            scope: Arc::clone(self),
            _token: self.tracker.token(),
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
            scope: Arc::clone(self),
            _token: self.tracker.token(),
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
        if tokio::time::timeout(std::time::Duration::from_secs(5), self.tracker.wait())
            .await
            .is_err()
        {
            return false;
        }
        !self.uncertain.load(Ordering::Acquire)
    }

    pub(super) fn is_idle(&self) -> bool {
        self.tracker.is_empty() && !self.uncertain.load(Ordering::Acquire)
    }
}

struct ExternalGuard {
    scope: Arc<ExecutionScope>,
    _token: TaskTrackerToken,
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
            self.scope.uncertain.store(true, Ordering::Release);
        }
    }
}
