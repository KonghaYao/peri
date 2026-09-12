//! Child futures belong to one run and must finish before its external owner settles.
use std::future::Future;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(super) struct RunScope {
    cancelled: CancellationToken,
    tasks: TaskTracker,
}

impl RunScope {
    pub(super) fn new() -> Self {
        Self {
            cancelled: CancellationToken::new(),
            tasks: TaskTracker::new(),
        }
    }

    pub(super) fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        let cancelled = self.cancelled.clone();
        self.tasks.spawn(async move {
            tokio::select! {
                biased;
                _ = cancelled.cancelled() => {}
                _ = task => {}
            }
        });
    }

    pub(super) fn cancel(&self) {
        self.cancelled.cancel();
    }

    pub(super) async fn drain(&self) {
        self.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}

impl Drop for RunScope {
    fn drop(&mut self) {
        self.cancel();
    }
}
