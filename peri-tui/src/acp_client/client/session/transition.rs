//! New-session and load transitions under the operation gate.

use peri_acp::transport::{AcpTransport, types::AcpError};
use serde_json::{Value, json};

use crate::acp_client::client::{AcpTuiClient, ClientProjectionMode, SessionLoadReservation};
use crate::acp_client::interaction_lifecycle::{TransitionKind, TransitionOperation};

use super::guards::{PreparingSessionGuard, SessionProjectionGuard};

/// 冷 load 清理未完成时服务端已把连接绑定到该 owner，客户端必须显式 close。
fn cleanup_incomplete_owner(error: &AcpError) -> Option<&str> {
    if error.code != -32010 {
        return None;
    }
    let data: &Value = error.data.as_ref()?;
    if data.get("reason").and_then(Value::as_str) != Some("session_cleanup_incomplete") {
        return None;
    }
    data.get("sessionId")
        .and_then(Value::as_str)
        .filter(|session_id| !session_id.is_empty())
}

impl AcpTuiClient {
    /// Create a new agent session.
    ///
    /// Closes the previous session (if any) to release its history, AgentPool,
    /// and FrozenSessionData from the server-side sessions HashMap.
    ///
    /// 整段建立过程都对用户可见（`SESSION_PREPARING`）：这段窗口里输入既不在待发送
    /// 队列、也还没有会话，等待中的输入可能来自提交，也可能来自启动时的那一次。
    pub async fn new_session(&self, cwd: &str, model: Option<&str>) -> Result<String, AcpError> {
        // 用户显式换会话：排队中的输入从此不能再落到新建的会话上，必须先于
        // operation gate 可见（等待中的 cohort 在持锁判定时会读到这次决定）。
        self.bump_session_switch_seq();
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.new_session_under_gate(cwd, model, TransitionOperation::New)
            .await
    }

    pub(super) async fn new_session_under_gate(
        &self,
        cwd: &str,
        model: Option<&str>,
        operation: TransitionOperation,
    ) -> Result<String, AcpError> {
        let _preparing = PreparingSessionGuard::enter();
        *self.restore_error.lock().unwrap() = None;
        self.clear_startup_restore_target();
        self.project_execution_cwd(None);
        let start = self
            .lifecycle
            .begin_transition_with_operation(TransitionKind::New, None, operation)
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
        self.lifecycle
            .commit_stable(start.generation, session_id.clone());
        self.finish_session_initialization(start.generation, &session_id)
            .await?;
        // S5.4：新建与恢复统一建立窗口。服务端只向持有 window ticket 的连接发送
        // `peri/history_committed`；新会话不 open 会让实时段永远停在 position=None。
        crate::kit::history_page_consumer::enqueue_window_open(
            &session_id,
            crate::kit::history_paging::WindowDirection::Before,
        );
        self.project_execution_cwd(Some(effective_cwd));
        *self.restore_error.lock().unwrap() = None;
        projection.committed = true;
        transition.disarm();
        self.flush_buffered();
        if self.projection_mode == ClientProjectionMode::Interactive {
            crate::kit::acp_events::request_bg_task_snapshot();
        }
        Ok(session_id)
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
        // 交互式切换是新的用户意图：不再自动重试上一次启动恢复目标。
        self.clear_startup_restore_target();
        self.load_session_under_gate(session_id, cwd, model, None, TransitionOperation::New)
            .await
    }

    /// 线程切换消费者在预约保护下加载。
    ///
    /// 预约身份在认领与路由切换的同一临界区内生效：准入快照要么看到
    /// 「未认领 + 旧路由」，要么看到「已认领 + Transitioning」，不会把本次
    /// load 之后的输入绑定到旧会话。
    pub(crate) async fn load_reserved_session(
        &self,
        reservation: &SessionLoadReservation,
        session_id: &str,
        cwd: &str,
        model: Option<&str>,
    ) -> Result<String, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        // 交互式切换是新的用户意图：不再自动重试上一次启动恢复目标。
        self.clear_startup_restore_target();
        self.load_session_under_gate(
            session_id,
            cwd,
            model,
            Some(reservation),
            TransitionOperation::Owned(reservation.operation()),
        )
        .await
    }

    pub(super) async fn load_session_under_gate(
        &self,
        session_id: &str,
        cwd: &str,
        model: Option<&str>,
        reservation: Option<&SessionLoadReservation>,
        operation: TransitionOperation,
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
            .claim_and_begin_transition(
                reservation,
                TransitionKind::Load,
                Some(session_id.to_string()),
                operation,
            )
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
        // cleanup-incomplete owner 即使与本次目标同 ID 也必须先关闭：服务端仍把
        // 连接绑在 closing 会话上，直接重载只会再次收到 session_switch_requires_close。
        let close_target = self
            .pending_close_target()
            .or_else(|| start.from.filter(|candidate| *candidate != session_id));
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
                // cleanup-incomplete 已把连接绑定到该会话；下一次建立/切换先关闭它，
                // 否则服务端会以 session_switch_requires_close 永久拒绝。
                if let Some(owner) = cleanup_incomplete_owner(&error) {
                    self.remember_close_failure(owner, None, &error);
                }
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
        // 恢复结果是成功响应的一部分：解码失败只降级提示，不影响会话可用性。
        let recovery = crate::acp_client::client::recovery_view::decode(&loaded);
        crate::kit::history_paging::note_initial_cursor(session_id, snapshot_token, next_cursor);
        self.lifecycle
            .commit_stable(start.generation, session_id.to_string());
        if self.projection_mode == ClientProjectionMode::Interactive {
            // 进入会话先展示已保存占用；缺失字段由解码侧落成未知，不沿用上一会话。
            crate::kit::context_usage::establish_session_usage(
                crate::kit::context_usage::from_load_response(&loaded),
            );
        }
        self.finish_session_initialization(start.generation, session_id)
            .await?;
        if let Some(view) = recovery.as_ref() {
            crate::acp_client::client::recovery_view::present(session_id, view);
        }
        self.flush_buffered();
        // S5.4：协商过窗口能力且开关开启时，额外建立 V1 窗口（默认关闭时无行为）。
        //
        // 必须在初始化成功之后：服务端对声明 `peri.historyWindowV1` 的连接不再回放历史，
        // 首屏由这次 open 提供；初始化失败会关闭会话，此时 open 没有可交付的首屏。
        crate::kit::history_page_consumer::enqueue_window_open(
            session_id,
            crate::kit::history_paging::WindowDirection::Before,
        );
        self.project_execution_cwd(Some(effective_cwd));
        *self.restore_error.lock().unwrap() = None;
        {
            let mut target = self.startup_restore_target.lock().unwrap();
            if target.as_deref() == Some(session_id) {
                *target = None;
            }
        }
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
}
