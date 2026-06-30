//! ACP notification bridge — converts AcpNotification → TUI AgentEvent dispatch.
//! Extracted from original agent_ops.rs (2026-05-20 split).

use tracing::debug;

use super::super::*;
use crate::app::App;

impl App {
    /// 处理 ACP notification — 将 AcpNotification 转换为相应的 UI 操作。
    /// 返回 `(updated, should_break, should_return)`，与 `handle_agent_event` 相同语义。
    pub(crate) fn handle_acp_notification(&mut self, notif: AcpNotification) -> (bool, bool, bool) {
        match notif {
            AcpNotification::AgentEvent { event, session_id } => {
                // Convert AcpEvent DTO → TUI AgentEvent via map_acp_event
                if let Some(agent_event) =
                    super::super::agent::map_acp_event(event, &self.services.cwd)
                {
                    debug!(
                        session_id = %session_id,
                        "ACP→TUI: AcpEvent dispatched to handle_agent_event"
                    );
                    return self.handle_agent_event(agent_event);
                }
                debug!(
                    session_id = %session_id,
                    "ACP→TUI: AcpEvent filtered by map_acp_event (internal event)"
                );
                (false, false, false)
            }
            AcpNotification::AgentDone { session_id } => {
                debug!(session_id = %session_id, "ACP→TUI: AgentDone received");
                self.handle_agent_event(super::super::AgentEvent::Done)
            }
            AcpNotification::RequestPermission { id, params } => {
                self.handle_acp_request_permission(id, params)
            }
            AcpNotification::Elicitation { id, params } => self.handle_acp_elicitation(id, params),
            AcpNotification::SessionUpdate { params, .. } => {
                self.handle_session_update_peri(&params)
            }
            AcpNotification::Peri { method, params, .. } => {
                tracing::debug!(%method, "ACP→TUI: peri/* notification (no TUI action)");
                let _ = params;
                (false, false, false)
            }
            AcpNotification::PredictionReady { text, .. } => {
                tracing::debug!(%text, "TUI received PredictionReady");
                // textarea 为空时才显示 prediction（用户可能已开始输入）
                let textarea_empty = self
                    .session_mgr
                    .current_mut()
                    .ui
                    .textarea
                    .lines()
                    .iter()
                    .all(|l| l.is_empty());
                if textarea_empty && !self.session_mgr.current_mut().ui.loading {
                    self.session_mgr.current_mut().ui.prediction =
                        Some(crate::app::ui_state::PredictionState {
                            text,
                            received_at: std::time::Instant::now(),
                        });
                }
                (true, false, false)
            }
            AcpNotification::UnstableEvent { event, .. } => {
                // Handled by the v2 state machine (path 1a). Legacy path is no-op.
                tracing::trace!(%event, "ACP→TUI: UnstableEvent (state machine path)");
                (false, false, false)
            }
            AcpNotification::Other { msg } => {
                tracing::warn!(%msg, "Unhandled ACP notification");
                (false, false, false)
            }
        }
    }
}

// ── Peri mode session/update → AgentEvent bridge ──────────────────────────

impl App {
    /// Peri 模式：将 session/update JSON 转换为 AgentEvent 并派发。
    ///
    /// 与 External 模式不同：External 模式直接操作 view_messages；
    /// Peri 模式转换为 AgentEvent 走 handle_agent_event() → pipeline。
    pub(crate) fn handle_session_update_peri(
        &mut self,
        params: &serde_json::Value,
    ) -> (bool, bool, bool) {
        let update = match params.get("update") {
            Some(u) => u,
            None => {
                tracing::warn!("SessionUpdate missing 'update' field");
                return (false, false, false);
            }
        };

        let source_agent_id = params
            .get("_peri")
            .and_then(|p| p.get("sourceAgentId"))
            .and_then(|v| v.as_str())
            .map(String::from);

        let update_type = update
            .get("sessionUpdate")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match update_type {
            "agent_message_chunk" => {
                self.handle_agent_event(super::super::AgentEvent::AssistantChunk)
            }
            "tool_call" => {
                let tool_call_id = update
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = update
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = update
                    .get("rawInput")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);

                let display = super::super::tool_display::format_tool_name(&name);
                let args = super::super::tool_display::format_tool_args(
                    &name,
                    &input,
                    Some(&self.services.cwd),
                )
                .unwrap_or_default();

                self.handle_agent_event(super::super::AgentEvent::ToolStart {
                    tool_call_id,
                    name,
                    display,
                    args,
                    input,
                    source_agent_id,
                })
            }
            "tool_call_update" => {
                let tool_call_id = update
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let raw_output = update
                    .get("rawOutput")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let is_error = update
                    .get("status")
                    .and_then(|v| v.as_str())
                    .map(|s| s == "failed")
                    .unwrap_or(false);

                let output = if is_error {
                    format!("✗ {}", super::super::tool_display::truncate(raw_output, 60))
                } else {
                    super::super::tool_display::truncate(raw_output, 200)
                };

                let name = update
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                self.handle_agent_event(super::super::AgentEvent::ToolEnd {
                    tool_call_id,
                    name,
                    output,
                    is_error,
                    source_agent_id,
                })
            }
            "plan" => {
                let entries = update.get("entries").and_then(|v| v.as_array());
                let mut todos = Vec::new();
                if let Some(entries) = entries {
                    for entry in entries {
                        let content = entry
                            .get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let status_str = entry
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("pending");
                        let status = match status_str {
                            "in_progress" => peri_acp::event::TodoStatusDto::InProgress,
                            "completed" => peri_acp::event::TodoStatusDto::Completed,
                            _ => peri_acp::event::TodoStatusDto::Pending,
                        };
                        todos.push(peri_acp::event::TodoItemDto {
                            content,
                            status,
                            active_form: None,
                        });
                    }
                }
                self.handle_agent_event(super::super::AgentEvent::TodoUpdate(todos))
            }
            "usage_update" => {
                // 从 enriched UsageUpdate _meta 中解析完整 token 用量
                let meta = update.get("_meta");
                let input_tokens = meta
                    .and_then(|m| m.get("inputTokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let output_tokens = meta
                    .and_then(|m| m.get("outputTokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let cache_creation_input_tokens = meta
                    .and_then(|m| m.get("cacheCreationTokens"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let cache_read_input_tokens = meta
                    .and_then(|m| m.get("cacheReadTokens"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let request_id = meta
                    .and_then(|m| m.get("requestId"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let model = meta
                    .and_then(|m| m.get("model"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let stop_reason = meta
                    .and_then(|m| m.get("stopReason"))
                    .and_then(|v| v.as_str())
                    .map(|s| match s {
                        "end_turn" => peri_acp::event::StopReasonDto::EndTurn,
                        "tool_use" => peri_acp::event::StopReasonDto::ToolUse,
                        "max_tokens" => peri_acp::event::StopReasonDto::MaxTokens,
                        other => peri_acp::event::StopReasonDto::Other {
                            value: other.to_string(),
                        },
                    });

                let usage = peri_acp::event::TokenUsageDto {
                    input_tokens,
                    output_tokens,
                    cache_creation_input_tokens,
                    cache_read_input_tokens,
                    request_id,
                };

                self.handle_agent_event(super::super::AgentEvent::TokenUsageUpdate {
                    usage,
                    model,
                    stop_reason,
                })
            }
            "session_info_update" => (false, false, false),
            "available_commands_update" => {
                // 从 ACP AvailableCommandsUpdate 学习 Agent 命令列表
                tracing::debug!(?update, "ACP→TUI: received available_commands_update");
                if let Some(cmds) = update
                    .get("availableCommands")
                    .or_else(|| update.get("commands"))
                    .and_then(|c| c.as_array())
                {
                    let names: Vec<String> = cmds
                        .iter()
                        .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(String::from))
                        .collect();
                    tracing::debug!(?names, "ACP→TUI: parsed command names");
                    if !names.is_empty() {
                        self.session_mgr
                            .current_mut()
                            .commands
                            .update_agent_commands(names);
                        tracing::debug!(
                            "ACP→TUI: learned {} agent commands from AvailableCommandsUpdate",
                            self.session_mgr.current_mut().commands.agent_commands.len()
                        );
                    }
                }
                (false, false, false)
            }
            _ => {
                tracing::debug!(update_type, "Peri mode: unhandled SessionUpdate type");
                (false, false, false)
            }
        }
    }
}
