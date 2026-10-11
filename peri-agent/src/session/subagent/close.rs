use std::sync::Arc;

use futures::FutureExt;
use peri_acp_types::tasks::{BgTaskKind, TaskManager as _};
use tokio::sync::watch;

use crate::session::Session;

type CloseResult = Result<(), String>;
type CloseReceiver = watch::Receiver<Option<CloseResult>>;

#[derive(Default)]
pub struct SubagentCloseState {
    attempt: parking_lot::Mutex<Option<CloseReceiver>>,
}

impl SubagentCloseState {
    pub fn is_closing(&self) -> bool {
        self.attempt.lock().is_some()
    }
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
    loop {
        if let Some(result) = receiver.borrow_and_update().clone() {
            return result;
        }
        receiver
            .changed()
            .await
            .map_err(|_| "Incomplete: child close worker lost".to_owned())?;
    }
}

async fn drain_child_scope(session: Arc<Session>) -> CloseResult {
    let host = session
        .subagent_host()
        .ok_or("Incomplete: child session host unavailable")?;
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
        if task.kind == BgTaskKind::Shell && !external.contains(&task.task_id) {
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
