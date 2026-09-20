//! Session transitions and queue-side load reservations under the lifecycle gate.

use std::sync::{Arc, Mutex};

use peri_acp::transport::{AcpTransport, types::AcpError};
use peri_acp_types::workspace::{ReadOnlyAdmission, RecoveryRequiredDetails};
use serde_json::{Value, json};
#[cfg(test)]
use tokio::sync::mpsc;
use tokio::sync::watch;

use super::super::interaction_lifecycle::{PromptLease, TransitionKind};
use super::{AcpTuiClient, ClientProjectionMode};

struct StartupRestoreGuard<'a>(&'a watch::Sender<bool>);

impl Drop for StartupRestoreGuard<'_> {
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}

/// 建会话期间的用户可见状态：进入时置位 `SESSION_PREPARING`，离开时清除。
///
/// 用 `Drop` 而不是在每个返回分支写清除：`session/new` 的 future 会被直接丢弃
/// （准备超时、应用关闭、取消），逐分支清理漏掉取消路径会把状态栏留在提示上。
struct PreparingSessionGuard;

impl PreparingSessionGuard {
    fn enter() -> Self {
        crate::kit::atoms::SESSION_PREPARING.set(true);
        Self
    }
}

impl Drop for PreparingSessionGuard {
    fn drop(&mut self) {
        crate::kit::atoms::SESSION_PREPARING.set(false);
    }
}

/// A dropped transition must also retire the UI target used for load replay.
/// This guard drops while the operation gate is still held.
struct SessionProjectionGuard<'a> {
    client: &'a AcpTuiClient,
    committed: bool,
}

impl Drop for SessionProjectionGuard<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.client.project_execution_cwd(None);
        if self.client.projection_mode == ClientProjectionMode::Interactive
            && !crate::kit::atoms::ACTIVE_SESSION_ID
                .state()
                .read()
                .is_empty()
        {
            crate::kit::session_boundary::project_session_boundary(None);
        }
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

#[cfg(test)]
#[path = "recovery_test.rs"]
mod recovery_tests;

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
    ///
    /// 整段建立过程都对用户可见（`SESSION_PREPARING`）：这段窗口里输入既不在待发送
    /// 队列、也还没有会话，等待中的输入可能来自提交，也可能来自启动时的那一次。
    pub async fn new_session(&self, cwd: &str, model: Option<&str>) -> Result<String, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.new_session_under_gate(cwd, model).await
    }

    async fn new_session_under_gate(
        &self,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, AcpError> {
        let _preparing = PreparingSessionGuard::enter();
        *self.restore_error.lock().unwrap() = None;
        self.project_execution_cwd(None);
        let start = self
            .lifecycle
            .begin_transition(TransitionKind::New, None)
            .map_err(|message| AcpError::new(-32603, message))?;
        let transition = self.lifecycle.arm_transition(start.generation);
        let mut projection = SessionProjectionGuard {
            client: self,
            committed: false,
        };
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::session_boundary::project_session_boundary(None);
        }
        self.settle_claims_owned(start.claims).await;
        let old_id = start.from;
        if let Some(ref old_sid) = old_id {
            let params = json!({ "sessionId": old_sid });
            if let Err(e) = self.transport.send_request("session/close", params).await {
                self.lifecycle.fail_transition(start.generation);
                transition.disarm();
                if self.projection_mode == ClientProjectionMode::Interactive {
                    crate::kit::session_boundary::project_session_boundary(None);
                }
                *self.restore_error.lock().unwrap() = Some(e.to_string());
                return Err(e);
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
        let effective_cwd = result
            .pointer("/_meta/peri.sessionWorkspaceV1/workspace/cwd")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(cwd)
            .to_string();
        #[cfg(test)]
        self.pause_before_transition_commit().await;
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::session_boundary::project_session_boundary(Some(&session_id));
        }
        let buffered = self
            .lifecycle
            .commit_stable(start.generation, session_id.clone());
        self.finish_session_initialization(start.generation, &session_id)
            .await?;
        self.project_execution_cwd(Some(effective_cwd));
        *self.restore_error.lock().unwrap() = None;
        projection.committed = true;
        transition.disarm();
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
                self.check_restore_error()?;
                return Ok(session_id);
            }
            if *startup_restore_rx.borrow_and_update() {
                drop(operation);
                startup_restore_rx.changed().await.map_err(|_| {
                    AcpError::new(-32603, "startup restore reservation closed unexpectedly")
                })?;
                continue;
            }
            self.check_restore_error()?;
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

    pub(super) async fn wait_for_session_load(
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
                    self.check_restore_error()?;
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
        *self.restore_error.lock().unwrap() =
            Some("session restore has not completed; retry or create a new session".into());
        let effective_cwd = if self.supports_session_workspace() {
            match self.session_context(Some(session_id), None).await {
                Ok(context) => context
                    .workspace
                    .cwd
                    .into_os_string()
                    .into_string()
                    .map_err(|_| AcpError::new(-32603, "session cwd is not valid UTF-8"))?,
                Err(error) => {
                    *self.restore_error.lock().unwrap() = Some(error.to_string());
                    return Err(error);
                }
            }
        } else {
            cwd.to_string()
        };
        self.project_execution_cwd(None);
        let start = self
            .lifecycle
            .begin_transition(TransitionKind::Load, Some(session_id.to_string()))
            .map_err(|message| AcpError::new(-32603, message))?;
        let transition = self.lifecycle.arm_transition(start.generation);
        let mut projection = SessionProjectionGuard {
            client: self,
            committed: false,
        };
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
                self.lifecycle.fail_transition(start.generation);
                transition.disarm();
                if self.projection_mode == ClientProjectionMode::Interactive {
                    crate::kit::session_boundary::project_session_boundary(None);
                }
                *self.restore_error.lock().unwrap() = Some(e.to_string());
                return Err(e);
            }
        }

        let params = json!({ "sessionId": session_id, "cwd": effective_cwd, "model": model });
        let first = self
            .transport
            .send_request("session/load", params.clone())
            .await;
        // 准入可能是只读的：历史已可读，但执行所有权不在本节点。它不是失败（不再用
        // 错误挡住进入），只把「本次准入只读」与原因带走。
        let mut read_only = first.as_ref().ok().and_then(read_only_admission);
        let mut result = first;
        if let Some(target) = recovery_target(&result, session_id)
            && self.projection_mode == ClientProjectionMode::Interactive
            && self
                .session_recovery
                .load(std::sync::atomic::Ordering::Acquire)
            && crate::kit::popups::confirm_popup::confirm_dirty_recovery(target.clone()).await
        {
            // operation gate 固定 source/target；确认等待期间不能提交其他 transition。
            let ack = peri_acp_types::workspace::ResetDirtyRequest {
                target,
                accept_risk: true,
            };
            let recovered = match self
                .transport
                .send_request(
                    "peri/session_reset_dirty",
                    serde_json::to_value(ack).expect("recovery request serialize"),
                )
                .await
            {
                Ok(_) => {
                    // 第二次 load 会让宿主把整段历史再回放一次：回放边界与回放请求必须
                    // 一一对应，否则两次回放叠加在同一个 committed 上，消息区整段重复。
                    // 边界放在 reset 成功之后：reset 失败时视图保持不动，无需重取历史。
                    crate::kit::session_boundary::project_session_boundary(Some(session_id));
                    self.transport
                        .send_request("session/load", params.clone())
                        .await
                }
                Err(error) => Err(error),
            };
            match recovered {
                Ok(response) => {
                    read_only = read_only_admission(&response);
                    result = Ok(response);
                }
                Err(error) => {
                    tracing::warn!(%error, session_id, "dirty recovery failed");
                    if read_only.is_some() {
                        // 首次准入本身是只读：它仍然有效，会话不因取回失败被丢弃。按只读
                        // 准入重取历史——成功即恢复渲染，失败就保留空视图与状态栏里的
                        // 只读原因。
                        //
                        // 重取同样会被宿主回放历史，因此先投影一次回放边界：边界与会被
                        // 回放的 load 一一对应，视图不会叠加两次历史。
                        crate::kit::session_boundary::project_session_boundary(Some(session_id));
                        match self.transport.send_request("session/load", params).await {
                            Ok(response) => {
                                read_only = read_only_admission(&response);
                                result = Ok(response);
                            }
                            Err(error) => tracing::warn!(
                                %error,
                                session_id,
                                "read-only re-admission after failed recovery failed"
                            ),
                        }
                    } else {
                        // 首次准入本身就是错误（未协商只读标记，或原因不是执行所有权）：
                        // 没有可回退的准入，按本次取回失败收敛，让用户看到最新的原因。
                        result = Err(error);
                    }
                }
            }
        }
        if let Err(error) = result {
            *self.restore_error.lock().unwrap() = Some(error.to_string());
            self.lifecycle.fail_transition(start.generation);
            transition.disarm();
            if self.projection_mode == ClientProjectionMode::Interactive {
                crate::kit::session_boundary::project_session_boundary(None);
            }
            return Err(error);
        }
        self.lifecycle
            .commit_stable(start.generation, session_id.to_string());
        self.finish_session_initialization(start.generation, session_id)
            .await?;
        self.project_execution_cwd(Some(effective_cwd));
        *self.restore_error.lock().unwrap() = None;
        // 只读标记是交互投影：唯一的清空点是 `project_session_boundary`（交互路径）。
        // 非交互客户端没有状态栏、不跑 steer consumer，写进去只会留下一个永不清空的
        // 全局标记，并污染并行的 UI 测试。
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::atoms::SESSION_READ_ONLY.set(read_only);
        }
        projection.committed = true;
        transition.disarm();
        Ok(session_id.to_string())
    }

    async fn finish_session_initialization(
        &self,
        generation: u64,
        session_id: &str,
    ) -> Result<(), AcpError> {
        if let Err(error) = self.initialize_user_inputs_under_gate(session_id).await {
            let claims = self.lifecycle.fail_transition(generation);
            self.settle_claims_owned(claims).await;
            self.project_execution_cwd(None);
            *self.restore_error.lock().unwrap() = Some(error.to_string());
            if self.projection_mode == ClientProjectionMode::Interactive {
                crate::kit::session_boundary::project_session_boundary(None);
            }
            if let Err(close_error) = self
                .transport
                .send_request("session/close", json!({"sessionId": session_id}))
                .await
            {
                tracing::warn!(%close_error, "session initialization cleanup failed");
            }
            return Err(error);
        }
        Ok(())
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
            self.project_execution_cwd(None);
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

/// 本次准入的只读标记；`None` 表示准入持有执行所有权（或响应没有该字段）。
///
/// host 只在协商了 `sessionWorkspaceV1` 的客户端上标注只读准入，未协商的连接仍旧
/// 收到准入错误。
fn read_only_admission(response: &Value) -> Option<ReadOnlyAdmission> {
    serde_json::from_value(
        response
            .pointer("/_meta/peri.sessionWorkspaceV1/read_only")?
            .clone(),
    )
    .ok()
}

/// 本次准入需要用户显式接受风险才能回到可执行，及其精确代际。
///
/// 两种携带方式指向同一件事：未协商只读标记的客户端从准入错误里读（`data`），协商过
/// 的从只读标记里读——后者已经进入只读会话，风险确认流程不变。
fn recovery_target(
    result: &Result<Value, AcpError>,
    session_id: &str,
) -> Option<RecoveryRequiredDetails> {
    let target = match result {
        Err(error) => match error.data.clone().and_then(|data| {
            serde_json::from_value::<peri_acp_types::workspace::WorkspaceErrorData>(data).ok()
        }) {
            Some(peri_acp_types::workspace::WorkspaceErrorData::RecoveryRequired(target)) => target,
            _ => return None,
        },
        Ok(response) => match read_only_admission(response) {
            Some(ReadOnlyAdmission::RecoveryRequired(target)) => target,
            _ => return None,
        },
    };
    (target.thread_id == session_id && target.generation > 0).then_some(target)
}
