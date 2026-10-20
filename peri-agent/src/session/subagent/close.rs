use std::{collections::HashMap, future::Future, sync::Arc};

use futures::FutureExt;
use peri_acp_types::tasks::{BgTaskKind, TaskManager as _};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::session::Session;

type CloseResult = Result<(), String>;
type CloseReceiver = watch::Receiver<Option<CloseResult>>;

tokio::task_local! {
    static CURRENT_SYNC_WORKER: Arc<SubagentCloseState>;
}

#[derive(Default)]
pub struct SubagentCloseState {
    attempt: parking_lot::Mutex<Option<CloseReceiver>>,
    pub(super) sync_workers: tokio_util::task::TaskTracker,
    sync_delivery: parking_lot::Mutex<HashMap<String, Option<String>>>,
    sync_delivery_changed: tokio::sync::Notify,
    closing_cancel: CancellationToken,
}

impl SubagentCloseState {
    pub fn is_closing(&self) -> bool {
        self.attempt.lock().is_some()
    }

    pub(super) fn spawn_sync_worker<F>(
        self: &Arc<Self>,
        task_id: String,
        worker: F,
    ) -> Result<tokio::task::JoinHandle<F::Output>, String>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let attempt = self.attempt.lock();
        if attempt.is_some() {
            return Err(
                "Incomplete: sync child worker registered after session close began".into(),
            );
        }
        self.sync_delivery.lock().insert(task_id, None);
        let handle = self
            .sync_workers
            .spawn(CURRENT_SYNC_WORKER.scope(Arc::clone(self), worker));
        drop(attempt);
        Ok(handle)
    }

    pub(super) fn record_sync_delivery_failure(&self, task_id: &str, error: &str) {
        self.sync_delivery.lock().insert(
            task_id.to_owned(),
            Some(format!("Incomplete: sync child delivery failed: {error}")),
        );
        self.sync_delivery_changed.notify_waiters();
    }

    pub(super) fn settle_sync_delivery(&self, task_id: &str) {
        self.sync_delivery.lock().remove(task_id);
    }

    #[cfg(test)]
    pub(super) fn sync_delivery_errors(&self) -> Vec<String> {
        self.sync_delivery
            .lock()
            .values()
            .filter_map(|error| error.clone())
            .collect()
    }

    async fn wait_sync_workers(&self) -> CloseResult {
        loop {
            let changed = self.sync_delivery_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let errors = {
                let failures = self.sync_delivery.lock();
                failures
                    .iter()
                    .filter_map(|(thread_id, error)| {
                        error.as_ref().map(|error| format!("{thread_id}: {error}"))
                    })
                    .collect::<Vec<_>>()
            };
            if !errors.is_empty() {
                return Err(errors.join("; "));
            }
            tokio::select! {
                _ = self.sync_workers.wait() => {
                    let pending = self.sync_delivery.lock();
                    if pending.is_empty() {
                        return Ok(());
                    }
                    return Err(pending.iter().map(|(thread_id, error)| {
                        format!("{thread_id}: {}", error.as_deref().unwrap_or(
                            "Incomplete: sync child worker ended without delivery settlement"))
                    }).collect::<Vec<_>>().join("; "));
                }
                _ = changed => {}
            }
        }
    }
}

/// close 已关闭真实资源、但因未确认交接/执行保留 owner 的错误。
/// 此时调用方仍可先结清认领与持久终态，再报告关闭错误。
///
/// 仅限“settlement/handoff unconfirmed”两类：真实资源关闭失败和
/// `child history persistence unconfirmed` 属于持久化/资源未确认，必须保持
/// Active，不能借取消认领写入 Cancelled。
pub(crate) fn close_failure_allows_terminal_finalization(error: &str) -> bool {
    error.starts_with("Incomplete: child handoff is unconfirmed")
        || error.starts_with("Incomplete: child settlement is unconfirmed")
}

pub async fn close_subagent_session_scope(session: Arc<Session>) -> CloseResult {
    let host = session
        .subagent_host()
        .ok_or("Incomplete: child session host unavailable")?;
    let mut receiver = {
        let mut attempt = host.close_state.attempt.lock();
        let existing = attempt
            .as_ref()
            .filter(|receiver| !matches!(receiver.borrow().as_ref(), Some(Err(_))))
            .cloned();
        if let Some(receiver) = existing {
            receiver
        } else {
            host.close_state.sync_workers.close();
            host.close_state.closing_cancel.cancel();
            let (sender, receiver) = watch::channel(None);
            *attempt = Some(receiver.clone());
            let worker_session = session.clone();
            tokio::spawn(async move {
                let result = std::panic::AssertUnwindSafe(drain_child_scope(worker_session))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| {
                        Err("Incomplete: child resource close worker panicked".into())
                    });
                sender.send_replace(Some(result));
            });
            receiver
        }
    };
    let resource_result = loop {
        if let Some(result) = receiver.borrow_and_update().clone() {
            break result;
        }
        receiver
            .changed()
            .await
            .map_err(|_| "Incomplete: child close worker lost".to_owned())?;
    };
    resource_result?;
    if CURRENT_SYNC_WORKER
        .try_with(|worker| Arc::ptr_eq(worker, &host.close_state))
        .unwrap_or(false)
    {
        return Ok(());
    }
    host.close_state.wait_sync_workers().await
}

async fn drain_child_scope(session: Arc<Session>) -> CloseResult {
    let host = session
        .subagent_host()
        .ok_or("Incomplete: child session host unavailable")?;
    if let Some(owner) = &host.child_owner {
        return owner.close().await;
    }
    let manager = host
        .task_manager
        .as_ref()
        .ok_or("Incomplete: child task directory unavailable")?;
    manager.begin_session_close();
    session.config().cancel_token.cancel();
    let mut failures = Vec::new();
    if let Some(pool) = &host.mcp_pool {
        let session_id = session
            .store()
            .thread_id
            .as_deref()
            .ok_or("Incomplete: child session identity unavailable")?;
        if let Err(error) = pool.clone().close_agent_session_scope(session_id).await {
            failures.push(error);
        }
    }
    let external: std::collections::HashSet<_> = manager.external_task_ids().into_iter().collect();
    for task in manager.snapshot().tasks {
        if task.kind == BgTaskKind::Shell
            && task.status == "running"
            && !external.contains(&task.task_id)
        {
            if let Err(error) = manager.cancel_async(&task.task_id).await {
                failures.push(format!(
                    "Incomplete: child shell cancellation failed: {error}"
                ));
            }
        }
    }
    if !manager.wait_session_close().await {
        failures.push(
            "Incomplete: child execution resources or independent delegations remain unsettled"
                .into(),
        );
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

pub(super) async fn settle_explicit_close(
    session: &Arc<Session>,
    interrupted: bool,
) -> CloseResult {
    let closing = session
        .subagent_host()
        .is_some_and(|host| host.close_state.is_closing());
    if interrupted || session.config().cancel_token.is_cancelled() || closing {
        close_subagent_session_scope(session.clone()).await
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "close_test.rs"]
mod tests;
