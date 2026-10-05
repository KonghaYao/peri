use std::sync::Arc;

use futures::FutureExt;
use peri_acp_types::session_resources::{
    ControlAction, ControlAttempt, ControlCommand, ControlDecision, ControlStatus, SessionResources,
};
use peri_acp_types::tasks::{BgTaskKind, TaskManager as _};
use tokio::sync::watch;

use crate::session::Session;

type CloseResult = Result<(), String>;
type CloseReceiver = watch::Receiver<Option<CloseResult>>;

#[derive(Default)]
pub struct SubagentCloseState {
    attempt: parking_lot::Mutex<Option<CloseReceiver>>,
    lifecycle: parking_lot::Mutex<Option<u64>>,
    intent: parking_lot::Mutex<Option<ControlCommand>>,
    finish: parking_lot::Mutex<Option<ControlCommand>>,
    stopped_attempt: parking_lot::Mutex<Option<ControlAttempt>>,
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

pub(super) async fn close_stopped_subagent_session_scope(
    session: Arc<Session>,
    stopped_attempt: ControlAttempt,
) -> CloseResult {
    let host = session
        .subagent_host()
        .ok_or("Incomplete: child session host unavailable")?;
    *host.close_state.stopped_attempt.lock() = Some(stopped_attempt);
    close_subagent_session_scope(session).await
}

async fn drain_child_scope(session: Arc<Session>) -> CloseResult {
    let host = session
        .subagent_host()
        .ok_or("Incomplete: child session host unavailable")?;
    let manager = host
        .task_manager
        .as_ref()
        .ok_or("Incomplete: child task directory unavailable")?;
    if let Some(resources) = &host.session_resources {
        let session_id = session
            .store()
            .thread_id
            .as_deref()
            .ok_or("Incomplete: child session identity unavailable")?;
        persist_close_control(resources.as_ref(), session_id, &host.close_state, false).await?;
    }
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
        if let Some(resources) = &host.session_resources {
            let session_id = session
                .store()
                .thread_id
                .as_deref()
                .ok_or("Incomplete: child session identity unavailable")?;
            let stopped_attempt = host.close_state.stopped_attempt.lock().clone();
            if let Some(stopped_attempt) = stopped_attempt {
                super::factory::clear_stopped_attempt(
                    resources.as_ref(),
                    &session_id.to_owned(),
                    &stopped_attempt,
                )
                .await?;
            }
            persist_close_control(resources.as_ref(), session_id, &host.close_state, true).await?;
        }
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

async fn persist_close_control(
    resources: &dyn SessionResources,
    session_id: &str,
    close: &SubagentCloseState,
    finish: bool,
) -> CloseResult {
    let session_id = session_id.to_owned();
    let current = resources
        .load_session_control(&session_id)
        .await
        .map_err(|error| format!("Incomplete: child close control unavailable: {error}"))?;
    {
        let mut lifecycle = close.lifecycle.lock();
        if lifecycle.is_some_and(|expected| expected != current.lifecycle) {
            return Err("Incomplete: child close lifecycle changed".into());
        }
        *lifecycle = Some(current.lifecycle);
    }
    if current.status == ControlStatus::Closed {
        return Ok(());
    }
    if !finish && current.status == ControlStatus::Closing {
        return Ok(());
    }
    if finish && (current.status != ControlStatus::Closing || current.attempt.is_some()) {
        return Err("Incomplete: child model execution quiescence unconfirmed".into());
    }
    let stored = if finish { &close.finish } else { &close.intent };
    let command = stored
        .lock()
        .get_or_insert_with(|| ControlCommand {
            session_id: session_id.clone(),
            command_id: format!(
                "child-close-{}:{}:{}",
                if finish { "finish" } else { "intent" },
                session_id,
                current.lifecycle
            ),
            expected_lifecycle: current.lifecycle,
            expected_revision: current.revision,
            expected_control_generation: current.control_generation,
            action: if finish {
                ControlAction::FinishClose
            } else {
                ControlAction::Close
            },
        })
        .clone();
    let receipt = resources
        .apply_session_control(&command)
        .await
        .map_err(|error| {
            format!("Incomplete: child close control mutation unconfirmed: {error}")
        })?;
    if receipt.decision != ControlDecision::Accepted {
        return Err("Incomplete: child close control mutation rejected".into());
    }
    Ok(())
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
