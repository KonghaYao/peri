//! Thin TUI-side wrapper around [`peri_acp::transport::mpsc::MpscClientTransport`].
//!
//! Translates raw [`IncomingMessage`]s into [`AcpNotification`]s for the TUI event
//! loop to consume. The notification pump runs as a background tokio task.

use std::sync::{Arc, Mutex};

use peri_acp::event::AcpEvent;
use peri_acp::transport::{
    AcpTransport,
    mpsc::MpscClientTransport,
    types::{AcpError, IncomingMessage, RequestId},
};
use peri_acp_types::event_data::PredictionAction;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

/// Notification events dispatched from the background pump to the TUI event loop.
#[derive(Debug)]
pub enum AcpNotification {
    /// A `notifications/agent_event` notification carrying an AcpEvent DTO.
    /// The TUI converts this to its own AgentEvent via `map_acp_event`.
    AgentEvent { session_id: String, event: AcpEvent },
    /// A `notifications/session_update` notification from the ACP server.
    SessionUpdate { session_id: String, params: Value },
    /// A `RequestPermission` request requiring HITL interaction.
    RequestPermission { id: RequestId, params: Value },
    /// An `elicitation/create` request requiring AskUser interaction.
    Elicitation { id: RequestId, params: Value },
    /// An unrecognized notification or request.
    Other { msg: String },
    /// Agent execution completed (synthetic notification from ACP server).
    /// `request_id` 为被结束 turn 的 prompt requestId（服务器回带，可选）——
    /// TUI 用它识别事件所属 turn（Issue 2026-08-05 stale 判定）。
    AgentDone {
        session_id: String,
        stop_reason: String,
        request_id: Option<String>,
    },
    /// Prediction fork 完成后的建议文本与结构化动作。
    PredictionReady {
        session_id: String,
        text: String,
        actions: Vec<PredictionAction>,
    },
    /// A `notifications/peri/*` custom notification (SubAgent, Compact, LSP, etc.)
    Peri {
        session_id: String,
        method: String,
        params: Value,
    },
    /// A `peri/unstable-event` notification carrying v2 state machine events
    /// (text-chunk, tool-started, view-commit, turn-done, etc.).
    UnstableEvent {
        session_id: String,
        event: String,
        data: Value,
    },
}

/// TUI-side client that owns the ACP transport and routes notifications.
///
/// Uses `Arc<Mutex<Option<String>>>` for `current_session_id` so that
/// clones (e.g., in `interrupt()` and `submit_message()`'s async task)
/// share the same session state.
///
/// `notification_tx` 刻意不存于此 struct：sender 必须由 pump task 独占持有，
/// pump 退出时 channel 关闭，notifier 的 recv-None 分支才能触发（Issue 2
/// 死代码重接）。若未来需要从 client 主动发通知，走显式参数传递，勿加回字段。
#[derive(Clone)]
pub struct AcpTuiClient {
    transport: Arc<MpscClientTransport>,
    current_session_id: Arc<Mutex<Option<String>>>,
}

impl AcpTuiClient {
    /// Check whether a session has been created.
    pub fn has_session(&self) -> bool {
        self.current_session_id.lock().unwrap().is_some()
    }

    /// Get the current session ID, if any.
    pub fn current_session_id(&self) -> Option<String> {
        self.current_session_id.lock().unwrap().clone()
    }

    /// Send a raw ACP request and return the response.
    /// Used for custom RPC methods like `workflow/list_runs`.
    pub async fn send_raw_request(&self, method: &str, params: Value) -> Result<Value, AcpError> {
        self.transport.send_request(method, params).await
    }

    /// Create a new client wrapping an existing `MpscClientTransport`.
    ///
    /// Returns `(Self, notification_sender, notification_receiver)`. The caller must:
    /// 1. Move `notification_sender` into [`AcpTuiClient::spawn_pump`] — the pump
    ///    task must remain its **sole** holder; when the pump exits (transport
    ///    closed) the sender drops, the channel closes, and the notifier's
    ///    recv-None fallback fires (Issue 2).
    /// 2. Move `notification_receiver` to the TUI event loop (`spawn_kit_notifier`).
    pub fn new(
        transport: MpscClientTransport,
    ) -> (
        Self,
        mpsc::UnboundedSender<AcpNotification>,
        mpsc::UnboundedReceiver<AcpNotification>,
    ) {
        let (notification_tx, notification_rx) = mpsc::unbounded_channel();
        let client = Self {
            transport: Arc::new(transport),
            current_session_id: Arc::new(Mutex::new(None)),
        };
        (client, notification_tx, notification_rx)
    }

    /// Spawn the notification pump as a tokio task. Consumes the notification
    /// sender and clones of transport/session state.
    ///
    /// `notification_tx` 由 pump task 独占持有：禁止克隆到 struct/全局/任何
    /// 长生命周期对象，否则 channel 不再随 pump 退出关闭，notifier 的
    /// recv-None 兜底失效（Issue 2）。从 client 主动发通知走显式参数传递。
    pub fn spawn_pump(&self, notification_tx: mpsc::UnboundedSender<AcpNotification>) {
        let transport = self.transport.clone();
        let current_session_id = self.current_session_id.clone();
        tokio::spawn(async move {
            Self::run_pump(transport, notification_tx, current_session_id).await;
        });
    }

    /// 检查 session_id 是否匹配当前会话。
    ///
    /// 当 `current_session_id` 为 `None`（首次连接、尚未创建会话）时返回 `true`，
    /// 确保 `AvailableCommandsUpdate` 等初始化通知不被丢弃。
    /// 当已设置会话后，严格按 session_id 过滤。
    fn is_current_session(
        current_session_id: &Arc<Mutex<Option<String>>>,
        session_id: &str,
    ) -> bool {
        current_session_id
            .lock()
            .unwrap()
            .as_deref()
            .is_none_or(|current| current == session_id)
    }

    // ── Pump ──

    /// Background task that polls the transport and dispatches notifications.
    async fn run_pump(
        transport: Arc<MpscClientTransport>,
        notification_tx: mpsc::UnboundedSender<AcpNotification>,
        current_session_id: Arc<Mutex<Option<String>>>,
    ) {
        let mut event_count: u64 = 0;
        loop {
            let msg = transport.recv().await;
            match msg {
                Some(IncomingMessage::Notification { method, params }) => {
                    if method == "peri/agent_event" {
                        event_count += 1;
                        let session_id = params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        // Prefer pre-serialized string (avoids clone + double-deserialize).
                        // Fall back to old "event" Value field for backward compat during rollout.
                        let event_result = if let Some(event_str) =
                            params.get("event_json").and_then(|v| v.as_str())
                        {
                            serde_json::from_str::<AcpEvent>(event_str)
                        } else if let Some(event_value) = params.get("event") {
                            serde_json::from_value::<AcpEvent>(event_value.clone())
                        } else {
                            warn!(
                                "ACP client pump: agent_event notification missing 'event_json' or 'event' field"
                            );
                            continue;
                        };
                        match event_result {
                            Ok(event) => {
                                debug!(
                                    event_count = event_count,
                                    session_id = %session_id,
                                    "ACP client pump: received agent_event"
                                );
                                if !Self::is_current_session(&current_session_id, &session_id) {
                                    debug!(session_id = %session_id, "ACP client pump: dropping stale agent_event");
                                    continue;
                                }
                                let _ = notification_tx
                                    .send(AcpNotification::AgentEvent { session_id, event });
                            }
                            Err(e) => {
                                error!(
                                    event_count = event_count,
                                    error = %e,
                                    "ACP client pump: failed to parse AcpEvent — event LOST"
                                );
                                let _ = notification_tx.send(AcpNotification::Other {
                                    msg: format!("failed to parse AcpEvent: {e}"),
                                });
                            }
                        }
                    } else if method == "session/update" {
                        let session_id = params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !Self::is_current_session(&current_session_id, &session_id) {
                            debug!(session_id = %session_id, "ACP client pump: dropping stale session/update");
                            continue;
                        }
                        let _ = notification_tx
                            .send(AcpNotification::SessionUpdate { session_id, params });
                    } else if method == "peri/unstable-event" {
                        let session_id = params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let event = params
                            .get("event")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let data = params.get("data").cloned().unwrap_or(Value::Null);
                        debug!(
                            session_id = %session_id,
                            event = %event,
                            "ACP client pump: received unstable-event"
                        );
                        if !Self::is_current_session(&current_session_id, &session_id) {
                            debug!(session_id = %session_id, event = %event, "ACP client pump: dropping stale unstable-event");
                            continue;
                        }
                        let _ = notification_tx.send(AcpNotification::UnstableEvent {
                            session_id,
                            event,
                            data,
                        });
                    } else if method == "peri/agent_event_done" {
                        let session_id = params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        debug!(
                            session_id = %session_id,
                            total_events = event_count,
                            "ACP client pump: received agent_event_done"
                        );
                        let stop_reason = params
                            .get("stopReason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("end_turn")
                            .to_string();
                        // requestId 为可选字段（缺失路径如 continuation/Immediate 命令/stdio）
                        let request_id = params
                            .get("requestId")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        if !Self::is_current_session(&current_session_id, &session_id) {
                            debug!(session_id = %session_id, "ACP client pump: dropping stale agent_done");
                            continue;
                        }
                        let _ = notification_tx.send(AcpNotification::AgentDone {
                            session_id,
                            stop_reason,
                            request_id,
                        });
                    } else if method == "peri/prediction_ready" {
                        let session_id = params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let text = params
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let actions = params
                            .get("actions")
                            .and_then(|v| {
                                serde_json::from_value::<Vec<PredictionAction>>(v.clone()).ok()
                            })
                            .unwrap_or_default();
                        if !Self::is_current_session(&current_session_id, &session_id) {
                            debug!(session_id = %session_id, "ACP client pump: dropping stale prediction_ready");
                            continue;
                        }
                        if !actions.is_empty() || !text.is_empty() {
                            let _ = notification_tx.send(AcpNotification::PredictionReady {
                                session_id,
                                text,
                                actions,
                            });
                        }
                    } else if method.starts_with("notifications/peri/") {
                        let session_id = params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !Self::is_current_session(&current_session_id, &session_id) {
                            debug!(session_id = %session_id, method = %method, "ACP client pump: dropping stale peri notification");
                            continue;
                        }
                        let _ = notification_tx.send(AcpNotification::Peri {
                            session_id,
                            method,
                            params,
                        });
                    } else {
                        let _ = notification_tx.send(AcpNotification::Other {
                            msg: format!("notification: {method}"),
                        });
                    }
                }
                Some(IncomingMessage::Request { id, method, params }) => {
                    if method == "session/request_permission" {
                        let _ =
                            notification_tx.send(AcpNotification::RequestPermission { id, params });
                    } else if method == "elicitation/create" {
                        let _ = notification_tx.send(AcpNotification::Elicitation { id, params });
                    } else {
                        let _ = notification_tx.send(AcpNotification::Other {
                            msg: format!("request: {method}"),
                        });
                    }
                }
                Some(IncomingMessage::Response { .. }) => {}
                None => {
                    debug!("ACP client pump: transport closed, exiting");
                    break;
                }
            }
        }
    }

    // ── High-level RPC wrappers ──

    /// Create a new agent session.
    ///
    /// Closes the previous session (if any) to release its history, AgentPool,
    /// and FrozenSessionData from the server-side sessions HashMap.
    pub async fn new_session(&self, cwd: &str, model: Option<&str>) -> Result<String, AcpError> {
        // 先清空本地事实源，避免旧 session 的延迟 notification 在 /clear 创建新会话前回写 UI。
        let old_id = self.current_session_id.lock().unwrap().take();
        if let Some(ref old_sid) = old_id {
            let params = json!({ "sessionId": old_sid });
            if let Err(e) = self.transport.send_request("session/close", params).await {
                debug!(error = %e, "Failed to close previous session (non-fatal)");
            }
        }

        let params = json!({ "cwd": cwd, "model": model });
        let result = self.transport.send_request("session/new", params).await?;
        // ACP protocol uses camelCase: {"sessionId": "..."}
        let session_id = result
            .get("sessionId")
            .or_else(|| result.get("session_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| AcpError::new(-32603, "no session_id in response"))?
            .to_string();
        *self.current_session_id.lock().unwrap() = Some(session_id.clone());
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
        // 先切换本地事实源，再发 close/load，避免旧 session 的延迟 notification 回写 UI。
        let old_id = self
            .current_session_id
            .lock()
            .unwrap()
            .replace(session_id.to_string());
        if let Some(ref old_sid) = old_id
            && old_sid != session_id
        {
            let params = json!({ "sessionId": old_sid });
            if let Err(e) = self.transport.send_request("session/close", params).await {
                debug!(error = %e, "Failed to close previous session (non-fatal)");
            }
        }

        let params = json!({ "sessionId": session_id, "cwd": cwd, "model": model });
        self.transport.send_request("session/load", params).await?;
        Ok(session_id.to_string())
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
        content: &peri_agent::messages::MessageContent,
        request_id: Option<String>,
    ) -> Result<(), AcpError> {
        let session_id = self
            .current_session_id
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| AcpError::new(-32603, "no active session"))?;
        let mut params = json!({
            "sessionId": session_id,
            "message": { "role": "user", "content": content },
        });
        if let Some(rid) = request_id {
            params["requestId"] = json!(rid);
        }
        self.transport
            .send_request("session/prompt", params)
            .await
            .map(|_| ())
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
        content: &peri_agent::messages::MessageContent,
        bg_results: Vec<peri_agent::agent::events::BackgroundTaskResult>,
        request_id: Option<String>,
    ) -> Result<(), AcpError> {
        let session_id = self
            .current_session_id
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| AcpError::new(-32603, "no active session"))?;
        let mut params = json!({
            "sessionId": session_id,
            "message": { "role": "user", "content": content },
            "bgResults": bg_results,
        });
        if let Some(rid) = request_id {
            params["requestId"] = json!(rid);
        }
        self.transport
            .send_request("session/prompt", params)
            .await
            .map(|_| ())
    }

    /// Change the model for the current session.
    pub async fn set_model(&self, alias: &str) -> Result<(), AcpError> {
        let session_id = self
            .current_session_id
            .lock()
            .unwrap()
            .clone()
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
            .current_session_id
            .lock()
            .unwrap()
            .clone()
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
        let session_id = {
            let guard = self.current_session_id.lock().unwrap();
            guard.clone()
        };
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
        let session_id = {
            let guard = self.current_session_id.lock().unwrap();
            guard.clone()
        };
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
        let session_id = self
            .current_session_id
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| AcpError::new(-32603, "no active session"))?;
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

    /// Send a response to a server-initiated request (e.g. HITL approval).
    pub async fn send_response(
        &self,
        id: RequestId,
        result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        self.transport.send_response(id, result).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp::transport::mpsc::mpsc_transport_pair;

    /// Issue 2026-08-05 返工链路测试：pump 解析 `peri/agent_event_done` 的
    /// requestId → `AgentDone.request_id`（服务器回带 → TUI stale 配对）。
    #[tokio::test]
    async fn test_pump_parses_agent_event_done_request_id() {
        let (client_transport, server_transport) = mpsc_transport_pair();
        let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
        client.spawn_pump(notification_tx);

        server_transport
            .send_notification(
                "peri/agent_event_done",
                json!({
                    "sessionId": "s1",
                    "stopReason": "cancelled",
                    "requestId": "rid-1",
                }),
            )
            .await
            .unwrap();

        match notification_rx.recv().await.unwrap() {
            AcpNotification::AgentDone {
                session_id,
                stop_reason,
                request_id,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(stop_reason, "cancelled");
                assert_eq!(request_id.as_deref(), Some("rid-1"));
            }
            other => panic!("expected AgentDone, got {other:?}"),
        }
    }

    /// 兼容性：requestId 缺失时 AgentDone.request_id 应为 None（continuation /
    /// Immediate 命令 / stdio 等路径）。
    #[tokio::test]
    async fn test_pump_agent_event_done_without_request_id() {
        let (client_transport, server_transport) = mpsc_transport_pair();
        let (client, notification_tx, mut notification_rx) = AcpTuiClient::new(client_transport);
        client.spawn_pump(notification_tx);

        server_transport
            .send_notification(
                "peri/agent_event_done",
                json!({ "sessionId": "s1", "stopReason": "end_turn" }),
            )
            .await
            .unwrap();

        match notification_rx.recv().await.unwrap() {
            AcpNotification::AgentDone {
                session_id,
                stop_reason,
                request_id,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(stop_reason, "end_turn");
                assert_eq!(request_id, None);
            }
            other => panic!("expected AgentDone, got {other:?}"),
        }
    }
}
