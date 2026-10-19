//! Session transitions and queue-side load reservations under the lifecycle gate.

use std::sync::{Arc, Mutex};

use peri_acp::transport::{AcpTransport, types::AcpError};
use serde_json::json;
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

/// ACP 明示的 close incomplete：旧会话资源尚未释放，同一 `session/close` 可重试。
const INCOMPLETE_CLOSE_CODE: i64 = -32010;

/// 一个尚未关闭成功的旧会话，以及失败时被打断的切换目标。
///
/// 关闭失败后 lifecycle 只留下 `NoSession`，旧身份随之消失；没有这份记录，下一次
/// 输入会绕过关闭直接 `session/new`——服务端的旧会话可能仍在 Closing、资源未释放，
/// 而用户原本要加载的历史会被静默换成空会话。只有 `-32010` 属于可重试语义，其余
/// 错误没有可行路径，不保留身份（`from_close_failure`）。
#[derive(Debug)]
pub(super) struct PendingClose {
    /// 必须先关闭成功才能建立新会话的旧身份。
    pub(super) session_id: String,
    /// 失败时用户原本要加载的会话；`Some` 时下一次建立会话先完成这次加载，
    /// 而不是新建空会话。新建路径为 `None`：重试同一路径自然回到「新建」。
    pub(super) resume_load: Option<String>,
}

impl PendingClose {
    fn from_close_failure(
        session_id: &str,
        resume_load: Option<String>,
        error: &AcpError,
    ) -> Option<Self> {
        (error.code == INCOMPLETE_CLOSE_CODE).then(|| Self {
            session_id: session_id.to_owned(),
            resume_load,
        })
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
        // 上一次关闭失败留下的旧身份优先：`fail_transition` 之后 lifecycle 不再保留
        // 它，但服务端可能仍在 Closing，绕过它就会把旧会话与用户意图一起丢掉。
        let close_target = self.pending_close_target().or(start.from);
        if let Some(ref old_sid) = close_target {
            if let Err(error) = self.close_previous_session(old_sid).await {
                self.lifecycle.fail_transition(start.generation);
                transition.disarm();
                if self.projection_mode == ClientProjectionMode::Interactive {
                    crate::kit::session_boundary::project_session_boundary(None);
                }
                // 旧会话清理失败不写入粘滞 `restore_error`：那会让后续每次
                // 建立/发送都被同一条陈旧错误拦截且无从清除。改记待关闭身份，
                // 下一次建立会话先重放同一关闭。
                *self.restore_error.lock().unwrap() = None;
                self.remember_close_failure(old_sid, None, &error);
                tracing::warn!(
                    session_id = %old_sid,
                    %error,
                    "previous session close incomplete; kept pending for retry"
                );
                return Err(error);
            }
            self.clear_pending_close();
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
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::acp_events::request_bg_task_snapshot();
        }
        Ok(session_id)
    }

    /// Close the previous session before switching identity.
    ///
    /// `-32010`（close incomplete）是 ACP 明示的可重试错误：在预算内重放同一
    /// 请求，与 `@peri-sdk` 的 `closeUntilSettled` 同一语义。重放耗尽仍不完整
    /// 时返回最后一次错误；调用方保留待关闭身份并保持可重试，不把它写成粘滞
    /// `restore_error`。
    async fn close_previous_session(&self, session_id: &str) -> Result<(), AcpError> {
        const INCOMPLETE_CLOSE_CODE: i64 = -32010;
        const DRAIN_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);
        const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
        let deadline = std::time::Instant::now() + DRAIN_BUDGET;
        loop {
            let params = json!({ "sessionId": session_id });
            match self.transport.send_request("session/close", params).await {
                Ok(_) => return Ok(()),
                Err(error) => {
                    let now = std::time::Instant::now();
                    if error.code != INCOMPLETE_CLOSE_CODE || now >= deadline {
                        return Err(error);
                    }
                    tokio::time::sleep(RETRY_INTERVAL.min(deadline - now)).await;
                }
            }
        }
    }

    /// 记录一次关闭失败：只有 `-32010` 保留待关闭身份（其余错误没有重试路径）。
    fn remember_close_failure(
        &self,
        session_id: &str,
        resume_load: Option<String>,
        error: &AcpError,
    ) {
        *self.pending_close.lock().unwrap() =
            PendingClose::from_close_failure(session_id, resume_load, error);
    }

    fn clear_pending_close(&self) {
        *self.pending_close.lock().unwrap() = None;
    }

    /// 尚未关闭成功的旧会话身份；存在时下一次建立会话必须先关闭它。
    fn pending_close_target(&self) -> Option<String> {
        self.pending_close
            .lock()
            .unwrap()
            .as_ref()
            .map(|pending| pending.session_id.clone())
    }

    /// 被关闭失败打断的加载目标；存在时下一次建立会话先完成这次加载。
    fn pending_resume_load(&self) -> Option<String> {
        self.pending_close
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|pending| pending.resume_load.clone())
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
            // 上一次加载 B 被关闭失败打断：先完成同一加载（`load_session_under_gate`
            // 内部会先重放关闭 A），不要把用户的历史意图换成新建空会话。
            if let Some(target) = self.pending_resume_load() {
                return self.load_session_under_gate(&target, cwd, model).await;
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
        let close_target = self
            .pending_close_target()
            .or(start.from)
            .filter(|candidate| *candidate != session_id);
        if let Some(ref old_sid) = close_target {
            if let Err(error) = self.close_previous_session(old_sid).await {
                self.lifecycle.fail_transition(start.generation);
                transition.disarm();
                if self.projection_mode == ClientProjectionMode::Interactive {
                    crate::kit::session_boundary::project_session_boundary(None);
                }
                // 与 `new_session_under_gate` 同一口径：清理旧会话失败不是
                // 恢复失败，不写入粘滞 `restore_error`（含本函数入口的占位）。
                // 额外记住本次要加载的目标，避免用户的历史意图被换成空会话。
                *self.restore_error.lock().unwrap() = None;
                self.remember_close_failure(old_sid, Some(session_id.to_owned()), &error);
                tracing::warn!(
                    session_id = %old_sid,
                    target_session_id = %session_id,
                    %error,
                    "previous session close incomplete; kept pending for retry"
                );
                return Err(error);
            }
            self.clear_pending_close();
        }

        let params = json!({ "sessionId": session_id, "cwd": effective_cwd, "model": model });
        let loaded = match self
            .transport
            .send_request("session/load", params.clone())
            .await
        {
            Ok(loaded) => loaded,
            Err(error) => {
                *self.restore_error.lock().unwrap() = Some(error.to_string());
                self.lifecycle.fail_transition(start.generation);
                transition.disarm();
                if self.projection_mode == ClientProjectionMode::Interactive {
                    crate::kit::session_boundary::project_session_boundary(None);
                }
                return Err(error);
            }
        };
        // 本次浏览身份（REQ §5.2 `nextCursor` / `snapshotToken`）：只有协商过分页能力的
        // 连接才有这两个字段，未协商时两者都是 None，触发侧因此永不请求历史页。
        // 响应解码失败不改变 load 的成败——浏览能力降级，会话本身已经装配成功。
        let (snapshot_token, next_cursor) =
            crate::kit::history_paging::decode_session_cursor(&loaded);
        crate::kit::history_paging::note_initial_cursor(session_id, snapshot_token, next_cursor);
        // S5.4：协商过窗口能力且开关开启时，额外建立 V1 窗口（默认关闭时无行为）。
        crate::kit::history_page_consumer::enqueue_window_open(
            session_id,
            crate::kit::history_paging::WindowDirection::Before,
        );
        self.lifecycle
            .commit_stable(start.generation, session_id.to_string());
        self.finish_session_initialization(start.generation, session_id)
            .await?;
        self.project_execution_cwd(Some(effective_cwd));
        *self.restore_error.lock().unwrap() = None;
        projection.committed = true;
        transition.disarm();
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::acp_events::request_bg_task_snapshot();
        }
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
