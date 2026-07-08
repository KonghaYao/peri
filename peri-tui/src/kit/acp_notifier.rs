//! ACP notifier——AcpNotification → AcpEventData 转换器。
//!
//! 直接在 notifier 内完成 DTO 转换，产出的 `AcpEventData` 立即送入 `spawn_acp_bridge`。
//! - **以 session/update 为流式主通道**：ACP 服务端的高频流式事件
//!   （agent_message_chunk / agent_thought_chunk / tool_call / tool_call_update）
//!   通过标准 `session/update` 携带，在 `handle_session_update` 中转换为
//!   `AcpEventData` 变体推入双 bridge channel。
//! - **usage_update**：token 消耗通过标准 session/update 的 `usage_update` tag
//!   携带，直接写入 `SPINNER_TOKEN_COUNT` atom，不产生 AcpEventData。
//! - **AgentEvent DTO 已接入**：`peri/agent_event` 携带的 AcpEvent 变体
//!   （SubagentStarted/SubagentStopped）通过 `convert_agent_event` 转换为
//!   AcpEventData 推入双 bridge channel。未映射变体（TurnCommitted/
//!   StateSnapshotMeta/CompactCompleted/...）保持静默丢弃，S5+ 迭代扩展。
//!
//! 该任务是**纯转换 + channel push**——不做状态突变。

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::acp_client::AcpNotification;
use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch};
use crate::kit::atoms::{ASK_USER_REQUEST_ID, AVAILABLE_SLASH_COMMANDS, SPINNER_TOKEN_COUNT};
use crate::kit::input_area::refresh_slash_items;
use peri_acp::event::AcpEvent;
use peri_acp::event::truncate::summarize_input;
use peri_acp_types::event_data::{AskUser, Question, QuestionOption};
use serde_json::Value;

/// 启动 kit ACP notifier 后台任务。
///
/// 从 `notification_rx` 读取 `AcpNotification`，把可识别的流式事件转换为
/// `AcpEventData` 推入 `bridge_tx`，由 `spawn_acp_bridge` 消费并写入 Atom。
///
/// 通道关闭（transport 断开）或 shutdown 触发时干净退出。
pub fn spawn_kit_notifier(
    mut notification_rx: mpsc::UnboundedReceiver<AcpNotification>,
    bridge_tx: mpsc::UnboundedSender<AcpEventWithEpoch>,
    render_bridge_tx: mpsc::UnboundedSender<AcpEventWithEpoch>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    debug!("kit ACP notifier: shutdown signal received, exiting");
                    break;
                }
                n = notification_rx.recv() => {
                    match n {
                        Some(notif) => forward_notification(&bridge_tx, &render_bridge_tx, notif),
                        None => {
                            debug!("kit ACP notifier: notification channel closed (transport disconnected)");
                            break;
                        }
                    }
                }
            }
        }
    })
}

/// 将 `peri/agent_event` 通道的 `AcpEvent` DTO 转换为 kit 层的 `AcpEventData`。
///
/// 当前映射列表（需与后续 S5+ 迭代同步扩展）：
/// - `SubagentStarted` / `SubagentStopped` → 对应的 `AcpEventData` 变体
/// - 其他变体返回 `None`（不存在对应的 `AcpEventData` 或以其他通道覆盖）
fn convert_agent_event(event: AcpEvent) -> Option<AcpEventData> {
    match event {
        AcpEvent::SubagentStarted {
            agent_name,
            instance_id,
            ..
        } => Some(AcpEventData::SubagentStarted {
            agent_id: instance_id,
            agent_name,
        }),
        AcpEvent::SubagentStopped { instance_id, .. } => Some(AcpEventData::SubagentStopped {
            agent_id: instance_id,
        }),
        // ── §4.8 Agent Event Extensions (P1-5) ──
        AcpEvent::TurnCommitted {
            messages_json,
            steps,
        } => Some(AcpEventData::TurnCommitted {
            messages_json,
            steps,
        }),
        AcpEvent::CompactStarted => Some(AcpEventData::CompactStarted),
        AcpEvent::CompactCompleted {
            summary,
            files,
            skills,
            micro_cleared,
            messages_json,
        } => {
            let files_json: Vec<serde_json::Value> = files
                .into_iter()
                .filter_map(|f| serde_json::to_value(f).ok())
                .collect();
            Some(AcpEventData::CompactCompleted {
                summary,
                files: files_json,
                skills,
                micro_cleared,
                messages_json,
            })
        }
        AcpEvent::CompactError { message } => Some(AcpEventData::CompactError { message }),
        AcpEvent::BackgroundTaskCompleted {
            task_id,
            agent_name,
            success,
            output,
            tool_calls_count,
            duration_ms,
            child_thread_id,
        } => Some(AcpEventData::BackgroundTaskCompleted {
            task_id,
            agent_name,
            success,
            output,
            tool_calls_count,
            duration_ms,
            child_thread_id,
        }),
        AcpEvent::AgentExecutionFailed { message } => {
            Some(AcpEventData::AgentExecutionFailed { message })
        }
        AcpEvent::WorkflowProgress {
            run_id,
            workflow_name,
            event_type,
            agent_id,
            phase,
            label,
            agent_status,
            token_count,
            tool_count,
            run_status,
            message,
        } => Some(AcpEventData::WorkflowProgress {
            run_id,
            workflow_name,
            event_type,
            agent_id,
            phase,
            label,
            agent_status,
            token_count,
            tool_count,
            run_status,
            message,
        }),
        _ => {
            debug!("kit ACP notifier: AcpEvent variant not yet mapped to AcpEventData, dropping");
            None
        }
    }
}

/// 把单条 `AcpNotification` 转换并推入 bridge channel。
///
/// 设计决策：session/update 是流式主通道（agent_message_chunk / tool_call 等），
/// AgentDone 通过 TurnDone 转换，AgentEvent 通过 `convert_agent_event` 转换。
fn forward_notification(
    bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
    render_bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
    n: AcpNotification,
) {
    /// 将 AcpEventData 包装为 AcpEventWithEpoch（注入 session_id）。
    fn wrap_with_session(event: AcpEventData, session_id: String) -> AcpEventWithEpoch {
        AcpEventWithEpoch {
            event,
            active_session_id: session_id,
        }
    }

    match n {
        AcpNotification::UnstableEvent {
            session_id,
            event,
            data,
        } => {
            let decoded = AcpEventData::decode(&event, data);
            if matches!(decoded, AcpEventData::Unknown { .. }) {
                debug!(event = %event, "kit ACP notifier: unknown unstable-event, dropping");
                return;
            }
            let wrapped = wrap_with_session(decoded, session_id);
            if let Err(e) = render_bridge_tx.send(wrapped.clone()) {
                warn!(error = %e, "kit ACP notifier: render_bridge_tx closed, render cache may stall");
            }
            if let Err(e) = bridge_tx.send(wrapped) {
                warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping event");
            }
        }
        // kit notifier: extract AvailableCommandsUpdate / plan / streaming
        // from SessionUpdate. Streaming tags produce AcpEventData pushed to
        // dual-bridge; status tags write atoms directly.
        AcpNotification::SessionUpdate { session_id, params } => {
            if let Some(decoded) = handle_session_update(params) {
                let wrapped = wrap_with_session(decoded, session_id);
                if let Err(e) = render_bridge_tx.send(wrapped.clone()) {
                    warn!(error = %e, "kit ACP notifier: render_bridge_tx closed, render cache may stall");
                }
                if let Err(e) = bridge_tx.send(wrapped) {
                    warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping session/update streaming event");
                }
            }
        }
        AcpNotification::AgentDone {
            session_id,
            stop_reason,
        } => {
            let decoded = if stop_reason == "cancelled" {
                AcpEventData::TurnInterrupted {
                    reason: "user cancelled".into(),
                }
            } else {
                AcpEventData::TurnDone
            };
            let wrapped = wrap_with_session(decoded, session_id);
            if let Err(e) = render_bridge_tx.send(wrapped.clone()) {
                warn!(error = %e, "kit ACP notifier: render_bridge_tx closed, render cache may keep current turn");
            }
            if let Err(e) = bridge_tx.send(wrapped) {
                warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping agent done");
            }
        }
        AcpNotification::Elicitation { id, params } => {
            handle_elicitation(&id, &params, bridge_tx, render_bridge_tx);
        }
        // peri/agent_event → AcpEvent → AcpEventData 转换
        // SubagentStarted/SubagentStopped 首先映射至此通道；通过 convert_agent_event
        // 转换为 kit 层 DTO 后推送（与 UnstableEvent 路径形成双通道冗余）。
        AcpNotification::AgentEvent { session_id, event } => {
            if let Some(decoded) = convert_agent_event(event) {
                let wrapped = wrap_with_session(decoded, session_id);
                if let Err(e) = render_bridge_tx.send(wrapped.clone()) {
                    warn!(error = %e, "kit ACP notifier: render_bridge_tx closed, render cache may stall");
                }
                if let Err(e) = bridge_tx.send(wrapped) {
                    warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping AgentEvent");
                }
            }
        }
        AcpNotification::RequestPermission { .. }
        | AcpNotification::PredictionReady { .. }
        | AcpNotification::Peri { .. }
        | AcpNotification::Other { .. } => {
            debug!("kit ACP notifier: notification variant not yet handled, dropping");
        }
    }
}

/// Extract commands / plan / streaming events from a SessionUpdate notification.
///
/// Returns `Some(AcpEventData)` for streaming tags (agent_message_chunk,
/// agent_thought_chunk, tool_call, tool_call_update) so the caller can push
/// to the dual-bridge channel. Returns `None` for status-only updates
/// (available_commands_update, plan, usage_update).
fn handle_session_update(params: serde_json::Value) -> Option<AcpEventData> {
    // params: {"session_id": "...", "update": <SessionUpdate>}
    // SessionUpdate uses #[serde(tag = "sessionUpdate", rename_all = "snake_case")]
    // → AvailableCommandsUpdate serializes as:
    //   {"sessionUpdate": "available_commands_update", "availableCommands": [...]}
    let update = match params.get("update") {
        Some(u) => u,
        None => return None,
    };
    // Discriminate: check the tag field, not a container key
    let tag = update.get("sessionUpdate").and_then(|v| v.as_str());

    if tag == Some("available_commands_update") {
        let cmds = match update.get("availableCommands").and_then(|v| v.as_array()) {
            Some(c) => c,
            None => return None,
        };
        let entries: Vec<(String, String)> = cmds
            .iter()
            .filter_map(|cmd| {
                let name = cmd.get("name")?.as_str()?;
                let desc = cmd
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Some((name.to_string(), desc.to_string()))
            })
            .collect();
        let len = entries.len();
        *AVAILABLE_SLASH_COMMANDS.state().write() = entries;
        refresh_slash_items();
        debug!(
            "kit ACP notifier: updated AVAILABLE_SLASH_COMMANDS ({})",
            len
        );
        return None;
    }

    if tag == Some("plan") {
        debug!(update = %update, "handle_session_update: plan tag matched");
        crate::kit::acp_events::handle_plan_update(update);
        return None;
    }

    let is_session_replay = update
        .get("meta")
        .or_else(|| update.get("content").and_then(|c| c.get("meta")))
        .and_then(|m| m.get("periReplay"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // ── §4.1 streaming: standard session/update streaming tags ──
    // agent_id from params["_peri"]["sourceAgentId"] (ACP extension)

    let agent_id: Option<String> = params
        .get("_peri")
        .and_then(|p| p.get("sourceAgentId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    match tag {
        Some("agent_message_chunk") => {
            // ACP SDK ContentChunk wraps text in content.text, not at update top-level
            let text = update
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if is_session_replay {
                Some(AcpEventData::ReplayAssistantBubble { text })
            } else {
                let text_chunk = crate::kit::stream_data::TuiTextChunk { text, agent_id };
                Some(AcpEventData::TextChunk(text_chunk))
            }
        }
        Some("agent_thought_chunk") => {
            let text = update
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let reasoning_chunk = crate::kit::stream_data::TuiReasoningChunk { text, agent_id };
            Some(AcpEventData::ReasoningChunk(reasoning_chunk))
        }
        Some("tool_call") => {
            let tool_id = update
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // ACP SDK ToolCall uses "title" field, not "name"
            let tool_name = update
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input_summary = {
                let raw_input = update.get("rawInput").unwrap_or(&Value::Null);
                summarize_input(&tool_name, raw_input)
            };
            let tool_started = crate::kit::stream_data::TuiToolStarted {
                tool_id,
                tool_name,
                input_summary,
                agent_id,
            };
            Some(AcpEventData::ToolStarted(tool_started))
        }
        Some("tool_call_update") => {
            let tool_id = update
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // ACP SDK ToolCallUpdate 使用 #[serde(flatten)] 将 rawOutput/status 合并到顶层；
            // 先尝试顶层字段（flatten 后的正确格式），再 fallback 到 fields 嵌套（兼容旧格式）。
            let output_summary = update
                .get("rawOutput")
                .or_else(|| update.get("fields").and_then(|f| f.get("rawOutput")))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let is_error = update
                .get("status")
                .or_else(|| update.get("fields").and_then(|f| f.get("status")))
                .and_then(|v| v.as_str())
                .map(|s| s == "failed")
                .unwrap_or(false);
            let tool_ended = crate::kit::stream_data::TuiToolEnded {
                tool_id,
                output_summary,
                is_error,
                agent_id,
            };
            Some(AcpEventData::ToolEnded(tool_ended))
        }
        Some("usage_update") if !is_session_replay => {
            // §C: token-usage deprecated, read from standard usage_update meta
            let input = update
                .get("meta")
                .and_then(|m| m.get("inputTokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let output = update
                .get("meta")
                .and_then(|m| m.get("outputTokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            *SPINNER_TOKEN_COUNT.state().write() = (input + output) as usize;
            None
        }
        // ── session/replay: user_message_chunk → ReplayUserBubble ──
        // Session replay 通过 session/update 推送 user_message_chunk + agent_message_chunk，
        // 逐条重放历史。agent_message_chunk 已在上面映射为 TextChunk，
        // user_message_chunk 映射为 ReplayUserBubble 追加到 committed。
        Some("user_message_chunk") => {
            let text = update
                .get("content")
                .and_then(|c| c.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(AcpEventData::ReplayUserBubble { text })
        }
        _ => None, // unknown tags
    }
}

/// 处理 Elicitation 通知：解析 params 为 AskUser → 写入 ASK_USER_REQUEST_ID atom →
/// 构造 AcpEventData::AskUser 推入双 bridge。
fn handle_elicitation(
    id: &peri_acp::transport::types::RequestId,
    params: &Value,
    bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
    render_bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
) {
    // 从 params 中提取 session_id
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // 序列化 RequestId 存入 atom（供 popup 提交时回传）
    if let Ok(id_str) = serde_json::to_string(id) {
        *ASK_USER_REQUEST_ID.state().write() = Some(id_str);
    } else {
        warn!("kit ACP notifier: failed to serialize elicitation RequestId");
        return;
    }

    let questions = parse_elicitation_questions(params);
    let ask_user = AskUser { questions };
    let event = AcpEventData::AskUser(ask_user);
    let wrapped = AcpEventWithEpoch {
        event,
        active_session_id: session_id,
    };

    info!("kit ACP notifier: forwarding Elicitation as AskUser event");

    if let Err(e) = render_bridge_tx.send(wrapped.clone()) {
        warn!(error = %e, "kit ACP notifier: render_bridge_tx closed, render cache may miss AskUser");
    }
    if let Err(e) = bridge_tx.send(wrapped) {
        warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping AskUser");
    }
}

/// 从 CreateElicitationRequest JSON 中解析问题列表。
///
/// JSON 结构（CreateElicitationRequest 序列化后，#[serde(flatten)] 展开）:
/// ```json
/// {"mode": "form", "sessionId": "sess_1", "message": "...",
///  "requestedSchema": {"type": "object", "properties": {
///   "q_id": {"type": "string", "title": "Header", "description": "Question text",
///            "oneOf": [{"const": "label", "title": "label"}]},
///   "multi_q_id": {"type": "array", "title": "...", "description": "...",
///                  "items": {"anyOf": [{"const": "label", "title": "..."}]}}
/// }}}
/// ```
///
/// 解析失败时返回空 Vec（弹窗显示 "0 questions"）。
fn parse_elicitation_questions(params: &Value) -> Vec<Question> {
    let props = match params
        .get("requestedSchema")
        .and_then(|rs| rs.get("properties"))
        .and_then(|p| p.as_object())
    {
        Some(p) => p,
        None => {
            warn!("kit ACP notifier: elicitation params missing requestedSchema.properties");
            return vec![];
        }
    };

    props
        .iter()
        .map(|(id, prop)| {
            let header = prop
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let question = prop
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            let prop_type = prop.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match prop_type {
                "array" => {
                    // multi_select: options 在 items.anyOf
                    let options = extract_options_from_oneof(prop, "anyOf", true);
                    Question {
                        id: id.clone(),
                        question,
                        header,
                        options,
                        multi_select: true,
                    }
                }
                "string" => {
                    // single select: options 在 oneOf
                    let options = extract_options_from_oneof(prop, "oneOf", false);
                    Question {
                        id: id.clone(),
                        question,
                        header,
                        options,
                        multi_select: false,
                    }
                }
                _ => Question {
                    id: id.clone(),
                    question,
                    header,
                    options: vec![],
                    multi_select: false,
                },
            }
        })
        .collect()
}

/// 从 prop["items"][key] 或 prop[key] 中提取 QuestionOption 列表。
/// - `nested=true`：选项在 `prop["items"][key]`（multi_select / anyOf）
/// - `nested=false`：选项在 `prop[key]`（single_select / oneOf）
fn extract_options_from_oneof(prop: &Value, key: &str, nested: bool) -> Vec<QuestionOption> {
    let arr = if nested {
        prop.get("items").and_then(|items| items.get(key))
    } else {
        prop.get(key)
    }
    .and_then(|v| v.as_array());

    let Some(arr) = arr else {
        return vec![];
    };

    arr.iter()
        .map(|opt| QuestionOption {
            label: opt
                .get("const")
                .or_else(|| opt.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            description: opt
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp::event::AcpEvent;
    use serde_json::json;
    use serial_test::serial;

    fn spawn_test_notifier() -> (
        mpsc::UnboundedSender<AcpNotification>,
        mpsc::UnboundedReceiver<AcpEventWithEpoch>,
        mpsc::UnboundedReceiver<AcpEventWithEpoch>,
        CancellationToken,
    ) {
        let (notif_tx, notif_rx) = mpsc::unbounded_channel::<AcpNotification>();
        let (bridge_tx, bridge_rx) = mpsc::unbounded_channel::<AcpEventWithEpoch>();
        let (render_bridge_tx, render_bridge_rx) = mpsc::unbounded_channel::<AcpEventWithEpoch>();
        let shutdown = CancellationToken::new();
        let _handle = spawn_kit_notifier(notif_rx, bridge_tx, render_bridge_tx, shutdown.clone());
        (notif_tx, bridge_rx, render_bridge_rx, shutdown)
    }

    #[tokio::test]
    async fn test_session_update_agent_message_chunk_to_text_chunk() {
        let (notif_tx, mut bridge_rx, _render_bridge_rx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::SessionUpdate {
                session_id: "s1".into(),
                params: json!({
                    "sessionId": "s1",
                    "_peri": {"sourceAgentId": "sa-1"},
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": "hi"}
                    }
                }),
            })
            .unwrap();

        let ev = bridge_rx.recv().await.expect("expected one event");
        match ev.event {
            AcpEventData::TextChunk(tc) => {
                assert_eq!(tc.text, "hi");
                assert_eq!(tc.agent_id.as_deref(), Some("sa-1"));
            }
            other => panic!("expected TextChunk, got {other:?}"),
        }

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_session_update_agent_thought_chunk() {
        let (notif_tx, mut bridge_rx, _render_bridge_rx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::SessionUpdate {
                session_id: "s1".into(),
                params: json!({
                    "sessionId": "s1",
                    "update": {
                        "sessionUpdate": "agent_thought_chunk",
                        "content": {"type": "text", "text": "thinking..."}
                    }
                }),
            })
            .unwrap();

        let ev = bridge_rx.recv().await.expect("expected one event");
        match ev.event {
            AcpEventData::ReasoningChunk(rc) => {
                assert_eq!(rc.text, "thinking...");
                assert!(rc.agent_id.is_none());
            }
            other => panic!("expected ReasoningChunk, got {other:?}"),
        }

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_session_update_tool_call_to_tool_started() {
        let (notif_tx, mut bridge_rx, _render_bridge_rx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::SessionUpdate {
                session_id: "s1".into(),
                params: json!({
                    "sessionId": "s1",
                    "update": {
                        "sessionUpdate": "tool_call",
                        "toolCallId": "tc-1",
                        "title": "Read",
                        "rawInput": {"file_path": "/tmp/foo.rs"}
                    }
                }),
            })
            .unwrap();

        let ev = bridge_rx.recv().await.expect("expected one event");
        match ev.event {
            AcpEventData::ToolStarted(ts) => {
                assert_eq!(ts.tool_id, "tc-1");
                assert_eq!(ts.tool_name, "Read");
                assert_eq!(ts.input_summary, "/tmp/foo.rs");
            }
            other => panic!("expected ToolStarted, got {other:?}"),
        }

        shutdown.cancel();
    }

    /// 验证 tool_call_update 的顶层 flatten 格式（ACP SDK 实际序列化格式）：
    /// rawOutput/status 被 #[serde(flatten)] 合并到 update 顶层。
    #[tokio::test]
    async fn test_session_update_tool_call_update_flattened_format() {
        let (notif_tx, mut bridge_rx, _render_bridge_rx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::SessionUpdate {
                session_id: "s1".into(),
                params: json!({
                    "sessionId": "s1",
                    "update": {
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": "tc-1",
                        "rawOutput": "output content",
                        "status": "failed"
                    }
                }),
            })
            .unwrap();

        let ev = bridge_rx.recv().await.expect("expected one event");
        match ev.event {
            AcpEventData::ToolEnded(te) => {
                assert_eq!(te.tool_id, "tc-1");
                assert!(te.output_summary.contains("output content"));
                assert!(te.is_error);
            }
            other => panic!("expected ToolEnded, got {other:?}"),
        }

        shutdown.cancel();
    }

    /// 验证 tool_call_update 的 fields 嵌套格式（fallback 兼容路径）：
    /// rawOutput/status 在 fields 子对象内。
    #[tokio::test]
    async fn test_session_update_tool_call_update_nested_fields_fallback() {
        let (notif_tx, mut bridge_rx, _render_bridge_rx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::SessionUpdate {
                session_id: "s1".into(),
                params: json!({
                    "sessionId": "s1",
                    "update": {
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": "tc-2",
                        "fields": {
                            "rawOutput": "nested output",
                            "status": "failed"
                        }
                    }
                }),
            })
            .unwrap();

        let ev = bridge_rx.recv().await.expect("expected one event");
        match ev.event {
            AcpEventData::ToolEnded(te) => {
                assert_eq!(te.tool_id, "tc-2");
                assert!(te.output_summary.contains("nested output"));
                assert!(te.is_error);
            }
            other => panic!("expected ToolEnded from nested fields, got {other:?}"),
        }

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_session_replay_agent_message_chunk_to_replay_assistant_bubble() {
        let (notif_tx, mut bridge_rx, _render_bridge_rx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::SessionUpdate {
                session_id: "s1".into(),
                params: json!({
                    "sessionId": "s1",
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": "历史回答", "meta": {"periReplay": true}}
                    }
                }),
            })
            .unwrap();

        let ev = bridge_rx.recv().await.expect("expected one event");
        match ev.event {
            AcpEventData::ReplayAssistantBubble { text } => assert_eq!(text, "历史回答"),
            other => panic!("expected ReplayAssistantBubble, got {other:?}"),
        }

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_unstable_event_unknown_dropped() {
        let (notif_tx, mut bridge_rx, _render_bridge_rx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::UnstableEvent {
                session_id: "s1".into(),
                event: "future-event".into(),
                data: json!({"x": 1}),
            })
            .unwrap();

        // Unknown 事件被丢弃——bridge_rx 在短时间内应无数据
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(50), bridge_rx.recv()).await;
        assert!(
            matches!(result, Ok(None)) || result.is_err(),
            "expected no event (channel idle or timeout), got {result:?}"
        );

        shutdown.cancel();
    }

    /// 验证 AgentEvent 的 SubagentStarted 变体被正确转换并转发到 bridge。
    /// SubagentStopped 同理（此处仅覆盖 SubagentStarted 作为 smoke test）。
    #[tokio::test]
    async fn test_agent_event_forwards_subagent_started() {
        let (notif_tx, mut bridge_rx, mut render_bridge_rx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::AgentEvent {
                session_id: "s1".into(),
                event: AcpEvent::SubagentStarted {
                    agent_name: "explore".into(),
                    instance_id: "abc-123".into(),
                    is_background: false,
                },
            })
            .unwrap();

        let bridge_event = bridge_rx
            .recv()
            .await
            .expect("bridge 应收 到 SubagentStarted");
        match bridge_event.event {
            AcpEventData::SubagentStarted {
                agent_id,
                agent_name,
            } => {
                assert_eq!(agent_id, "abc-123", "agent_id 应从 instance_id 映射");
                assert_eq!(agent_name, "explore");
            }
            other => panic!("expected SubagentStarted, got {other:?}"),
        }

        let render_event = render_bridge_rx
            .recv()
            .await
            .expect("render bridge 应收到 SubagentStarted");
        match render_event.event {
            AcpEventData::SubagentStarted {
                agent_id,
                agent_name,
            } => {
                assert_eq!(agent_id, "abc-123");
                assert_eq!(agent_name, "explore");
            }
            other => panic!("expected SubagentStarted on render bridge, got {other:?}"),
        }

        shutdown.cancel();
    }

    /// 验证未映射的 AcpEvent 变体被静默丢弃（防御性测试）。
    #[tokio::test]
    async fn test_agent_event_unknown_variant_dropped() {
        let (notif_tx, mut bridge_rx, _render_bridge_rx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::AgentEvent {
                session_id: "s1".into(),
                event: AcpEvent::StateSnapshotMeta {
                    message_count: 0,
                    total_tokens: 0,
                    current_step: 0,
                    consecutive_failures: 0,
                    budget_pct: None,
                    context_total_tokens: None,
                },
            })
            .unwrap();

        // StateSnapshotMeta 保持丢弃（纯信息性，不影响渲染）
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(50), bridge_rx.recv()).await;
        assert!(
            matches!(result, Ok(None)) || result.is_err(),
            "expected unmapped AgentEvent to be dropped, got {result:?}"
        );

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_agent_done_forwards_turn_done_to_bridges() {
        let (notif_tx, mut bridge_rx, mut render_bridge_rx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::AgentDone {
                session_id: "s1".into(),
                stop_reason: "end_turn".into(),
            })
            .unwrap();

        let bridge_event = bridge_rx.recv().await.expect("bridge 应收到 TurnDone");
        assert!(matches!(bridge_event.event, AcpEventData::TurnDone));
        let render_event = render_bridge_rx
            .recv()
            .await
            .expect("render bridge 应收到 TurnDone");
        assert!(matches!(render_event.event, AcpEventData::TurnDone));

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_channel_close_exits_cleanly() {
        let (notif_tx, _bridge_rx, _render_bridge_rx, shutdown) = spawn_test_notifier();

        // 模拟 transport 断开：drop sender 让 recv() 返回 None
        drop(notif_tx);

        // 给任务一点时间退出
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // shutdown 仍可正常调用（任务已退出，cancel 信号无害）
        shutdown.cancel();
    }

    /// 验证 handle_session_update 能正确解析 ACP SessionUpdate 的 JSON 格式。
    /// SessionUpdate 使用 #[serde(tag = "sessionUpdate")] 内部标签，字段名 camelCase。
    #[test]
    #[serial]
    fn test_handle_session_update_parses_available_commands() {
        crate::kit::atoms::init_atoms();
        let payload = json!({
            "sessionId": "s1",
            "update": {
                "sessionUpdate": "available_commands_update",
                "availableCommands": [
                    {"name": "help", "description": "Show help"},
                    {"name": "clear", "description": "Clear conversation"},
                    {"name": "archify", "description": "Create architecture diagrams"}
                ]
            }
        });
        let _ = handle_session_update(payload);
        let entries = AVAILABLE_SLASH_COMMANDS.state().read().clone();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], ("help".to_string(), "Show help".to_string()));
        assert_eq!(
            entries[2],
            (
                "archify".to_string(),
                "Create architecture diagrams".to_string()
            )
        );
    }

    /// 验证非 available_commands_update 的 session/update 不会错误写入 atom。
    #[test]
    #[serial]
    fn test_handle_session_update_skips_non_command_update() {
        crate::kit::atoms::init_atoms();
        // 重置 atom 状态，避免跨测试污染
        *AVAILABLE_SLASH_COMMANDS.state().write() = Vec::new();
        let payload = json!({
            "sessionId": "s1",
            "update": {
                "sessionUpdate": "usage_update",
                "used": 1000,
                "total": 200000
            }
        });
        let _ = handle_session_update(payload);
        let entries = AVAILABLE_SLASH_COMMANDS.state().read().clone();
        assert_eq!(entries.len(), 0, "非 commands update 不应写入 atom");
    }

    /// 验证 handle_session_update 能正确解析 plan update 并写入 TODO_ITEMS atom。
    #[test]
    #[serial]
    fn test_handle_session_update_parses_plan() {
        use crate::kit::message_area::TodoStatus;
        crate::kit::atoms::init_atoms();
        *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();

        let payload = json!({
            "sessionId": "s1",
            "update": {
                "sessionUpdate": "plan",
                "entries": [
                    {"content": "Fix bug", "status": "in_progress", "priority": "medium"},
                    {"content": "Write tests", "status": "pending", "priority": "medium"},
                    {"content": "Document", "status": "completed", "priority": "medium"}
                ]
            }
        });

        let _ = handle_session_update(payload);

        let items = crate::kit::atoms::TODO_ITEMS.state().read().clone();
        assert_eq!(items.len(), 3, "应包含 3 个条目，实际: {items:?}");
        assert_eq!(items[0].content, "Fix bug");
        assert_eq!(items[1].content, "Write tests");
        assert_eq!(items[2].content, "Document");
        assert!(matches!(items[0].status, TodoStatus::InProgress));
        assert!(matches!(items[1].status, TodoStatus::Pending));
        assert!(matches!(items[2].status, TodoStatus::Completed));
    }
}
