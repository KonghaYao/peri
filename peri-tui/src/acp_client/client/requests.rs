//! ACP request wrappers for capabilities, prompts, configuration, and cancellation.

use peri_acp::transport::{AcpTransport, types::AcpError};
use peri_acp_types::{PeriCaps, command::command_route::UiCommandSpec};
use serde_json::{Value, json};

use super::AcpTuiClient;

impl AcpTuiClient {
    /// Send a raw ACP request and return the response.
    /// Used for custom RPC methods like `workflow/list_runs`.
    pub async fn send_raw_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.transport.send_request(method, params).await
    }

    /// 上送 ui 域命令明细（设计 §88 / Phase 3 caps 通道：`clientCapabilities._meta.
    /// peri.uiCommands` 明细数组）。
    ///
    /// TUI 内部 Mpsc 路径无协议 initialize 握手，host 默认以
    /// [`PeriCaps::all_enabled`] 兜底（含 11 条旧兜底 ui 明细）。本方法显式协商
    /// caps：以 all_enabled 为基座、替换 `ui_commands` 为 TUI 实时明细，经
    /// initialize 请求送达 host（`handle_request` "initialize" 分支 → `set_pending_caps`
    /// → 首个 session/new 的 `ensure_session_caps` 取协商值 → `send_available_commands_update`
    /// 把明细注册进注册表，投影回推刷新补全缓存）。
    ///
    /// **时序契约**：必须在首个 `session/new` 之前调用（initialize 是进程级
    /// 一次性协商）；失败仅 warn 不阻断——host 回退 all_enabled 兜底明细，
    /// 首 session 仍可用（R2 双写窗口防御）。
    pub async fn register_ui_commands(&self, specs: &[UiCommandSpec]) -> Result<(), AcpError> {
        let mut caps = PeriCaps::all_enabled();
        caps.ui_commands = specs.to_vec();
        let params = json!({
            "protocolVersion": 1,
            "clientCapabilities": { "_meta": caps.to_agent_meta() },
        });
        self.transport.send_request("initialize", params).await?;
        Ok(())
    }

    /// Submit a user message to the current session.
    /// Note: prompt() is called from the spawned async task that already
    /// has a session via new_session(), so current_session_id is guaranteed Some.
    ///
    /// `request_id` 为本轮 prompt 的唯一标识（submit_consumer 生成）——服务器
    /// 随 turn 结束事件（peri/agent_event_done）回带，供 stale 事件配对判定
    /// （Issue 2026-08-05）。None = 缺失路径（不注入 params）。
    pub async fn prompt(
        &self,
        content: &peri_acp_types::messages::MessageContent,
        request_id: Option<String>,
    ) -> Result<(), AcpError> {
        let (session_id, lease) = self
            .open_prompt_after_session_loads(request_id.clone())
            .await?;
        let mut params = json!({
            "sessionId": session_id,
            "message": { "role": "user", "content": content },
        });
        if let Some(rid) = request_id {
            params["requestId"] = json!(rid);
        }
        let result = self.transport.send_request("session/prompt", params).await;
        let _operation = self.lifecycle.operation_gate().lock().await;
        let claims = lease.finish();
        self.settle_claims_owned(claims).await;
        result.map(|_| ())
    }

    /// Submit a user message with background task results attached.
    ///
    /// The server-side executor injects the bg_results as `Defer` messages into the
    /// v2 MessageQueue (see `peri-acp/src/session/executor.rs`). Defer is the
    /// correct semantic for async-delayed results: Receive skips them, End drains
    /// and awakens a new turn, and `run_react_loop` writes them to the transcript
    /// wrapped in `<system-reminder>` (see `append_messages_to_transcript`).
    pub async fn prompt_with_bg_results(
        &self,
        content: &peri_acp_types::messages::MessageContent,
        bg_results: Vec<peri_acp_types::event::BackgroundTaskResult>,
        request_id: Option<String>,
    ) -> Result<(), AcpError> {
        let (session_id, lease) = self
            .open_prompt_after_session_loads(request_id.clone())
            .await?;
        let mut params = json!({
            "sessionId": session_id,
            "message": { "role": "user", "content": content },
            "bgResults": bg_results,
        });
        if let Some(rid) = request_id {
            params["requestId"] = json!(rid);
        }
        let result = self.transport.send_request("session/prompt", params).await;
        let _operation = self.lifecycle.operation_gate().lock().await;
        let claims = lease.finish();
        self.settle_claims_owned(claims).await;
        result.map(|_| ())
    }

    /// Change the model for the current session.
    pub async fn set_model(&self, alias: &str) -> Result<(), AcpError> {
        let session_id = self
            .lifecycle
            .current_session_id()
            .ok_or_else(|| AcpError::new(-32603, "no active session"))?;
        let params = json!({ "sessionId": session_id, "modelId": alias });
        let _ = self
            .transport
            .send_request("session/set_model", params)
            .await?;
        Ok(())
    }

    /// Change the permission mode for the current session.
    pub async fn set_mode(&self, mode: &str) -> Result<(), AcpError> {
        let session_id = self
            .lifecycle
            .current_session_id()
            .ok_or_else(|| AcpError::new(-32603, "no active session"))?;
        let params = json!({ "sessionId": session_id, "modeId": mode });
        let _ = self
            .transport
            .send_request("session/set_mode", params)
            .await?;
        Ok(())
    }

    /// Set a config option (mode/model/thought_level) via the unified config API.
    /// Silently returns Ok if no session exists yet — uses notification to
    /// update ACP server state directly without requiring a session.
    pub async fn set_config_option(&self, config_id: &str, value: &str) -> Result<(), AcpError> {
        let session_id = self.lifecycle.current_session_id();
        match session_id {
            Some(session_id) => {
                let params =
                    json!({ "sessionId": session_id, "configId": config_id, "value": value });
                let _ = self
                    .transport
                    .send_request("session/set_config_option", params)
                    .await?;
            }
            None => {
                // No session yet — send via notification so ACP server updates its
                // peri_config/provider before any session is created.
                let params = json!({ "configId": config_id, "value": value });
                self.transport
                    .send_notification("session/config_update", params)
                    .await?;
            }
        }
        Ok(())
    }

    /// Update the full PeriConfig on the ACP server (for Login panel CRUD).
    /// When no session exists, uses notification to update server state directly.
    pub async fn update_config(&self, config: &crate::config::PeriConfig) -> Result<(), AcpError> {
        let session_id = self.lifecycle.current_session_id();
        match session_id {
            Some(session_id) => {
                let params = json!({
                    "sessionId": session_id,
                    "config": config,
                });
                let _ = self
                    .transport
                    .send_request("session/update_config", params)
                    .await?;
            }
            None => {
                // No session yet — send via notification so ACP server updates
                // peri_config/provider before any session is created.
                tracing::info!("update_config: no session, sending via notification");
                let params = json!({
                    "config": config,
                });
                self.transport
                    .send_notification("session/config_update", params)
                    .await?;
            }
        }
        Ok(())
    }

    /// Cancel the currently running prompt.
    pub async fn cancel(&self) -> Result<(), AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        let session_id = self
            .lifecycle
            .current_session_id()
            .ok_or_else(|| AcpError::new(-32603, "no active session"))?;
        let claims = self.lifecycle.cancel_active_prompt();
        self.settle_claims_owned(claims).await;
        let params = json!({ "sessionId": session_id });
        self.transport
            .send_notification("session/cancel", params)
            .await
    }

    /// Cancel a specific background task by task_id.
    pub async fn cancel_bg_task(&self, session_id: &str, task_id: &str) -> Result<Value, AcpError> {
        self.send_raw_request(
            "session/cancel-bg-task",
            json!({ "sessionId": session_id, "taskId": task_id }),
        )
        .await
    }

    /// Kill a workflow run by run_id（Workflow 面板 Enter / workflow/kill_run RPC）。
    /// 与 cancel_bg_task 对 Workflow 类型任务等效：走同一 WorkflowTaskRegistry::kill 通道。
    pub async fn kill_workflow_run(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<Value, AcpError> {
        self.send_raw_request(
            "workflow/kill_run",
            json!({ "sessionId": session_id, "runId": run_id }),
        )
        .await
    }
}
