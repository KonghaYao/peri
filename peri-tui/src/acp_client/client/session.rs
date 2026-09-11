//! Session transitions and queue-side load reservations under the lifecycle gate.

use std::sync::{Arc, Mutex};

use peri_acp::transport::{AcpTransport, types::AcpError};
use serde_json::json;
#[cfg(test)]
use tokio::sync::mpsc;
use tokio::sync::watch;
use tracing::debug;

use super::super::interaction_lifecycle::{PromptLease, TransitionKind};
use super::{AcpTuiClient, ClientProjectionMode};

struct StartupRestoreGuard<'a>(&'a watch::Sender<bool>);

impl Drop for StartupRestoreGuard<'_> {
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}

pub(super) struct SessionLoadReservationState {
    pub(super) pending: Mutex<usize>,
    pub(super) epoch_tx: watch::Sender<u64>,
}

/// Synchronous queue-side ownership for an ordinary session load.
///
/// The reservation is acquired before a load request enters its async consumer.
/// Dropping the guard releases exactly one queued/in-flight load and wakes prompt
/// waiters. It is deliberately not Clone: the pending count represents queue
/// ownership, not arbitrary client clones.
pub(crate) struct SessionLoadReservation {
    state: Arc<SessionLoadReservationState>,
}

impl Drop for SessionLoadReservation {
    fn drop(&mut self) {
        let mut pending = self.state.pending.lock().unwrap();
        debug_assert!(*pending > 0, "session load reservation underflow");
        *pending = pending.saturating_sub(1);
        drop(pending);
        self.state
            .epoch_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }
}

impl AcpTuiClient {
    /// Check whether a session has been created.
    pub fn has_session(&self) -> bool {
        self.lifecycle.has_session()
    }

    /// Get the current session ID, if any.
    pub fn current_session_id(&self) -> Option<String> {
        self.lifecycle.current_session_id()
    }

    /// Create a new agent session.
    ///
    /// Closes the previous session (if any) to release its history, AgentPool,
    /// and FrozenSessionData from the server-side sessions HashMap.
    pub async fn new_session(&self, cwd: &str, model: Option<&str>) -> Result<String, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.new_session_under_gate(cwd, model).await
    }

    async fn new_session_under_gate(
        &self,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, AcpError> {
        let start = self
            .lifecycle
            .begin_transition(TransitionKind::New, None)
            .map_err(|message| AcpError::new(-32603, message))?;
        let transition = self.lifecycle.arm_transition(start.generation);
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::session_boundary::project_session_boundary(None);
        }
        self.settle_claims_owned(start.claims).await;
        let old_id = start.from;
        if let Some(ref old_sid) = old_id {
            let params = json!({ "sessionId": old_sid });
            if let Err(e) = self.transport.send_request("session/close", params).await {
                debug!(error = %e, "Failed to close previous session (non-fatal)");
            }
        }

        let params = json!({ "cwd": cwd, "model": model });
        let result = match self.transport.send_request("session/new", params).await {
            Ok(result) => result,
            Err(error) => {
                self.lifecycle.fail_transition(start.generation);
                transition.disarm();
                if self.projection_mode == ClientProjectionMode::Interactive {
                    crate::kit::session_boundary::project_session_boundary(None);
                }
                return Err(error);
            }
        };
        // ACP protocol uses camelCase: {"sessionId": "..."}
        let session_id = result
            .get("sessionId")
            .or_else(|| result.get("session_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| AcpError::new(-32603, "no session_id in response"))?
            .to_string();
        #[cfg(test)]
        self.pause_before_transition_commit().await;
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::session_boundary::project_session_boundary(Some(&session_id));
        }
        let buffered = self
            .lifecycle
            .commit_stable(start.generation, session_id.clone());
        transition.disarm();
        self.initialize_user_inputs_under_gate(&session_id).await?;
        self.flush_buffered(buffered);
        Ok(session_id)
    }

    #[cfg(test)]
    pub(super) fn install_transition_commit_hook(
        &self,
        tx: mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>,
    ) {
        *self.transition_commit_hook.lock().unwrap() = Some(tx);
    }

    #[cfg(test)]
    async fn pause_before_transition_commit(&self) {
        let tx = self.transition_commit_hook.lock().unwrap().clone();
        if let Some(tx) = tx {
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            if tx.send(release_tx).is_ok() {
                let _ = release_rx.await;
            }
        }
    }

    /// Return the current stable session, creating it only if no startup restore
    /// owns the lifecycle decision.  Both the stable re-check and a possible
    /// `session/new` execute under the client operation gate.
    pub async fn ensure_session(&self, cwd: &str, model: Option<&str>) -> Result<String, AcpError> {
        let mut startup_restore_rx = self.startup_restore_tx.subscribe();
        let mut load_epoch_rx = self.session_load_reservations.epoch_tx.subscribe();
        loop {
            let operation = self.lifecycle.operation_gate().lock().await;
            let (pending_load, stable_session) = {
                let _ = *load_epoch_rx.borrow_and_update();
                let pending = self.session_load_reservations.pending.lock().unwrap();
                if *pending > 0 {
                    (true, None)
                } else {
                    // Hold the reservation mutex through Stable selection so a
                    // dispatcher linearizes strictly before or after this result.
                    (
                        false,
                        self.lifecycle
                            .stable_identity()
                            .map(|(session_id, _)| session_id),
                    )
                }
            };
            if pending_load {
                drop(operation);
                self.wait_for_session_load(&mut load_epoch_rx).await?;
                continue;
            }
            if let Some(session_id) = stable_session {
                return Ok(session_id);
            }
            if *startup_restore_rx.borrow_and_update() {
                drop(operation);
                startup_restore_rx.changed().await.map_err(|_| {
                    AcpError::new(-32603, "startup restore reservation closed unexpectedly")
                })?;
                continue;
            }
            return self.new_session_under_gate(cwd, model).await;
        }
    }

    /// Reserve an ordinary session load before handing it to an async consumer.
    /// This synchronous boundary closes the browser-select → consumer scheduling
    /// window where a fast submit could otherwise bind to the old Stable session.
    pub(crate) fn reserve_session_load(&self) -> SessionLoadReservation {
        let mut pending = self.session_load_reservations.pending.lock().unwrap();
        *pending = pending
            .checked_add(1)
            .expect("session load reservation count overflow");
        drop(pending);
        self.session_load_reservations
            .epoch_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
        SessionLoadReservation {
            state: Arc::clone(&self.session_load_reservations),
        }
    }

    #[cfg(test)]
    pub(crate) fn pending_session_load_count(&self) -> usize {
        *self.session_load_reservations.pending.lock().unwrap()
    }

    async fn wait_for_session_load(
        &self,
        epoch_rx: &mut watch::Receiver<u64>,
    ) -> Result<(), AcpError> {
        loop {
            let _ = *epoch_rx.borrow_and_update();
            if *self.session_load_reservations.pending.lock().unwrap() == 0 {
                return Ok(());
            }
            epoch_rx.changed().await.map_err(|_| {
                AcpError::new(-32603, "session load reservation closed unexpectedly")
            })?;
        }
    }

    pub(super) async fn open_prompt_after_session_loads(
        &self,
        request_id: Option<String>,
    ) -> Result<(String, PromptLease), AcpError> {
        let mut epoch_rx = self.session_load_reservations.epoch_tx.subscribe();
        loop {
            let opened = {
                let _operation = self.lifecycle.operation_gate().lock().await;
                let pending = self.session_load_reservations.pending.lock().unwrap();
                let _ = *epoch_rx.borrow_and_update();
                if *pending > 0 {
                    None
                } else {
                    let session_id = self
                        .lifecycle
                        .stable_identity()
                        .map(|(session_id, _)| session_id)
                        .ok_or_else(|| AcpError::new(-32603, "no active session"))?;
                    let lease = self
                        .lifecycle
                        .open_prompt(request_id.clone())
                        .map_err(|message| AcpError::new(-32603, message))?;
                    // Keep the reservation mutex through open_prompt: a dispatcher
                    // linearizes strictly before this prompt or after it.
                    Some((session_id, lease))
                }
            };
            if let Some(opened) = opened {
                return Ok(opened);
            }
            self.wait_for_session_load(&mut epoch_rx).await?;
        }
    }

    /// Establish resume/continue ownership before submit consumers can choose a
    /// fresh session. The reservation mutation shares the lifecycle operation
    /// gate with `ensure_session`, `new_session`, and `load_session`.
    pub async fn reserve_startup_restore(&self) {
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.startup_restore_tx.send_replace(true);
    }

    /// Resolve a startup restore reservation by loading its selected target.
    /// Waiters are released only after load commits Stable (or fails closed).
    pub async fn load_startup_session(
        &self,
        session_id: &str,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        let reservation = StartupRestoreGuard(&self.startup_restore_tx);
        let result = self.load_session_under_gate(session_id, cwd, model).await;
        drop(reservation);
        result
    }

    /// Release a startup reservation when resume lookup cannot select a target.
    pub async fn release_startup_restore(&self) {
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.startup_restore_tx.send_replace(false);
    }

    /// Load an existing session from ThreadStore history.
    /// Used when restoring a historical thread so the ACP server has the full context.
    ///
    /// Closes the previous session (if any) to release server-side memory.
    pub async fn load_session(
        &self,
        session_id: &str,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.load_session_under_gate(session_id, cwd, model).await
    }

    async fn load_session_under_gate(
        &self,
        session_id: &str,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, AcpError> {
        let start = self
            .lifecycle
            .begin_transition(TransitionKind::Load, Some(session_id.to_string()))
            .map_err(|message| AcpError::new(-32603, message))?;
        let transition = self.lifecycle.arm_transition(start.generation);
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::session_boundary::project_session_boundary(Some(session_id));
        }
        self.settle_claims_owned(start.claims).await;
        let old_id = start.from;
        if let Some(ref old_sid) = old_id
            && old_sid != session_id
        {
            let params = json!({ "sessionId": old_sid });
            if let Err(e) = self.transport.send_request("session/close", params).await {
                debug!(error = %e, "Failed to close previous session (non-fatal)");
            }
        }

        let params = json!({ "sessionId": session_id, "cwd": cwd, "model": model });
        if let Err(error) = self.transport.send_request("session/load", params).await {
            self.lifecycle.fail_transition(start.generation);
            transition.disarm();
            if self.projection_mode == ClientProjectionMode::Interactive {
                crate::kit::session_boundary::project_session_boundary(None);
            }
            return Err(error);
        }
        self.lifecycle
            .commit_stable(start.generation, session_id.to_string());
        transition.disarm();
        self.initialize_user_inputs_under_gate(session_id).await?;
        Ok(session_id.to_string())
    }

    async fn initialize_user_inputs_under_gate(&self, session_id: &str) -> Result<(), AcpError> {
        if self.supports_user_input_queue() {
            let snapshot = self.user_input_snapshot_under_gate(session_id).await?;
            if self.projection_mode == ClientProjectionMode::Interactive {
                crate::kit::steer_state::establish_session_snapshot(snapshot);
            }
        }
        Ok(())
    }

    /// Delete a session from history (standard ACP `session/delete`).
    ///
    /// 遵守 agentclientprotocol.com/protocol/v1/session-delete：`{ sessionId }`
    /// 请求、`{}` 响应；删除后会话不再出现在 `session/list` 中且无法
    /// `session/load`。若删除的是当前活跃会话，本地事实源一并清空
    /// （服务端会 cancel 该会话的 in-flight turn 并级联删除消息）。
    ///
    /// M3：删除的会话 id 记入黑名单，pump 过滤其延迟通知（`current_session_id`
    /// 置 None 后"首次连接放行"语义会让已删除会话的事件回写 UI）。
    pub async fn delete_session(&self, session_id: &str) -> Result<(), AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        let is_current = self
            .lifecycle
            .stable_identity()
            .as_ref()
            .is_some_and(|(id, _)| id == session_id);
        let transition = if is_current {
            let start = self
                .lifecycle
                .begin_transition(TransitionKind::DeleteCurrent, None)
                .map_err(|message| AcpError::new(-32603, message))?;
            let lease = self.lifecycle.arm_transition(start.generation);
            if self.projection_mode == ClientProjectionMode::Interactive {
                crate::kit::session_boundary::project_session_boundary(None);
            }
            self.settle_claims_owned(start.claims).await;
            Some((start.generation, lease))
        } else {
            None
        };
        let params = json!({ "sessionId": session_id });
        let result = self.transport.send_request("session/delete", params).await;
        match transition {
            Some((generation, lease)) => {
                self.lifecycle.fail_transition(generation);
                lease.disarm();
                if result.is_ok() {
                    self.lifecycle.mark_deleted(session_id);
                }
            }
            None if result.is_ok() => self.lifecycle.mark_deleted(session_id),
            None => {}
        }
        result.map(|_| ())
    }
}
