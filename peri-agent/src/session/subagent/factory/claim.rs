//! Ownership of resume status from its initial claim through sync finalization.
//!
//! A worker owns the active write and its compensating write. Dropping the caller
//! requests preparation rollback or running cancellation; it never cancels an
//! in-flight database write.
//! Cleanup then completes asynchronously, provided the runtime and store remain
//! available. Explicit rollback awaits the same worker and reports its failure.

use std::sync::Arc;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::thread::{ThreadMeta, ThreadStore};

/// Cross-instance claim serialization; history loading and execution stay outside.
/// As before, this lock does not coordinate separate processes.
static RESUME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

enum ClaimDecision {
    Release,
    Finish(&'static str),
}

pub(in crate::session::subagent) struct ResumeClaim {
    release: Option<oneshot::Sender<ClaimDecision>>,
    running: bool,
    worker: JoinHandle<Result<(), String>>,
}

impl ResumeClaim {
    pub(super) async fn acquire(
        store: Arc<dyn ThreadStore>,
        thread_id: String,
    ) -> Result<(ThreadMeta, Self), Box<dyn std::error::Error + Send + Sync>> {
        let (meta_tx, meta_rx) = oneshot::channel();
        let (release, decision) = oneshot::channel();
        let worker = tokio::spawn(async move {
            let result = own_claim(&store, &thread_id, meta_tx, decision).await;
            if let Err(error) = &result {
                tracing::error!(%thread_id, %error, "resume status finalization failed");
            }
            result
        });
        // Created before the first await: even cancellation during the active
        // write leaves the worker with an unambiguous rollback decision.
        let owner = Self {
            release: Some(release),
            running: false,
            worker,
        };
        let meta = meta_rx.await.map_err(|error| {
            format!("resume_subagent: status claim worker ended before reporting: {error}")
        })??;
        Ok((meta, owner))
    }

    /// Successful background registration now owns terminal status.
    pub(super) fn release(mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(ClaimDecision::Release);
        }
    }

    pub(in crate::session::subagent) fn mark_running(&mut self) {
        self.running = true;
    }

    /// Normal sync Stop has run its hook; serialize the terminal write with the
    /// original active write. Dropping this await cannot cancel that write.
    pub(in crate::session::subagent) async fn finish(mut self, status: &'static str) {
        if let Some(release) = self.release.take() {
            let _ = release.send(ClaimDecision::Finish(status));
        }
        // Preserve the existing best-effort Stop persistence contract. The
        // worker reports store errors even if this await is cancelled.
        if let Err(error) = (&mut self.worker).await {
            tracing::error!(%error, "resume terminal status worker failed");
        }
    }

    pub(super) async fn rollback(mut self) -> Result<(), String> {
        drop(self.release.take());
        (&mut self.worker)
            .await
            .map_err(|error| format!("resume status rollback worker failed: {error}"))?
    }
}

impl Drop for ResumeClaim {
    fn drop(&mut self) {
        if self.running {
            if let Some(release) = self.release.take() {
                let _ = release.send(ClaimDecision::Finish("cancelled"));
            }
        }
        // A preparation drop closes the sender without a decision: restore the
        // previous status. Dropping JoinHandle detaches, never aborts, the worker.
    }
}

async fn own_claim(
    store: &Arc<dyn ThreadStore>,
    thread_id: &String,
    mut meta_tx: oneshot::Sender<Result<ThreadMeta, String>>,
    decision: oneshot::Receiver<ClaimDecision>,
) -> Result<(), String> {
    let guard = tokio::select! {
        biased;
        _ = meta_tx.closed() => return Ok(()),
        guard = RESUME_LOCK.lock() => guard,
    };
    // Validation is read-only and may be cancelled without compensation. Once
    // the active write starts below, the worker must drive it to completion.
    let validated = tokio::select! {
        biased;
        _ = meta_tx.closed() => return Ok(()),
        result = validate_thread(store, thread_id) => result,
    };
    let meta = match validated {
        Ok(meta) => meta,
        Err(error) => {
            let _ = meta_tx.send(Err(error));
            return Ok(());
        }
    };
    if meta_tx.is_closed() {
        return Ok(());
    }
    let previous_status = meta.agent_status;
    // Never select cancellation against this write: the store may commit before
    // returning Pending. Any rollback must be ordered after its completion.
    if let Err(error) = store.update_thread_status(thread_id, "active").await {
        let rollback = restore_status(store, thread_id, previous_status.as_str()).await;
        let mut message = format!(
            "resume_subagent: failed to mark thread {} active: {}",
            thread_id, error
        );
        if let Err(error) = &rollback {
            message.push_str(&format!("; {error}"));
        }
        let _ = meta_tx.send(Err(message));
        return rollback;
    }
    drop(guard);
    // If the receiver was dropped, its owner also closed `decision`. A success
    // arriving after cancellation therefore cannot strand an unowned claim.
    let _ = meta_tx.send(Ok(meta));
    let status = match decision.await {
        Ok(ClaimDecision::Release) => return Ok(()),
        Ok(ClaimDecision::Finish(status)) => status,
        Err(_) => previous_status.as_str(),
    };
    let _guard = RESUME_LOCK.lock().await;
    restore_status(store, thread_id, status).await
}

async fn validate_thread(
    store: &Arc<dyn ThreadStore>,
    thread_id: &String,
) -> Result<ThreadMeta, String> {
    if uuid::Uuid::parse_str(thread_id).is_err() {
        return Err(format!("resume_subagent: invalid thread id: {}", thread_id));
    }
    let meta = store
        .load_meta(thread_id)
        .await
        .map_err(|_| format!("resume_subagent: thread not found: {}", thread_id))?;
    if meta.agent_status.is_active() {
        return Err(format!(
            "resume_subagent: thread {} is still active \
            (thread 仍处于运行态: 可能仍在执行, 或上次异常退出未收尾; \
            若确认无执行中任务, 可改用 Agent(subagent_type: ...) 新建)",
            thread_id
        ));
    }
    Ok(meta)
}

async fn restore_status(
    store: &Arc<dyn ThreadStore>,
    thread_id: &String,
    previous_status: &str,
) -> Result<(), String> {
    store
        .update_thread_status(thread_id, previous_status)
        .await
        .map_err(|error| format!("failed to restore thread {thread_id} status: {error}"))
}
