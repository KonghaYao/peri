//! ACP session/load history replay via `session/update` notifications.
//!
//! Per ACP v1 spec, `session/load` MUST replay the entire conversation to the
//! client via `session/update` notifications (`user_message_chunk` +
//! `agent_message_chunk`) BEFORE responding to the request.
//!
//! Tool interactions (`ToolUse` / `ToolResult`) are replayed via standard
//! `tool_call` / `tool_call_update` events so the TUI can render tool cards.
//!
//! Reference: <https://agentclientprotocol.com/protocol/v1/session-setup#loading-a-session>

use agent_client_protocol_schema::v1::{
    ContentBlock, ContentChunk, SessionId, SessionNotification, SessionUpdate, TextContent,
    ToolCall, ToolCallId, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
use peri_agent::messages::{
    BaseMessage, ContentBlock as PeriContentBlock, MessageContent as PeriMessageContent,
};

/// Replay session history via `session/update` notifications.
///
/// Iterates `history`, converting each `BaseMessage` into one or more
/// `SessionUpdate` variants, then calls `sender` for each notification.
///
/// - `BaseMessage::Human`  → `SessionUpdate::UserMessageChunk`
/// - `BaseMessage::Ai`     → text blocks as `AgentMessageChunk`,
///   `ToolUse` blocks as `ToolCall` (periReplay=true)
/// - `BaseMessage::Tool`   → `ToolResult` blocks as `ToolCallUpdate` (periReplay=true)
/// - Other variants         → silently skipped
pub async fn replay_session_history(
    session_id: &str,
    history: &[BaseMessage],
    sender: &dyn ReplaySender,
) -> Result<(), ReplayError> {
    for msg in history.iter().filter(|m| !m.is_system()) {
        match msg {
            BaseMessage::Human { content, .. } => {
                let update = SessionUpdate::UserMessageChunk(replay_chunk(ContentBlock::Text(
                    TextContent::new(extract_text(content)),
                )));
                let notif =
                    SessionNotification::new(SessionId::new(session_id.to_string()), update);
                sender.send(notif).await?;
            }
            BaseMessage::Ai {
                content,
                tool_calls,
                ..
            } => {
                // 收集 ContentBlock::ToolUse 的 id，避免与 tool_calls 重复发射
                let mut emitted_ids: std::collections::HashSet<String> =
                    std::collections::HashSet::new();

                let blocks = match content {
                    PeriMessageContent::Text(s) => {
                        let update = SessionUpdate::AgentMessageChunk(replay_chunk(
                            ContentBlock::Text(TextContent::new(s.clone())),
                        ));
                        let notif = SessionNotification::new(
                            SessionId::new(session_id.to_string()),
                            update,
                        );
                        sender.send(notif).await?;
                        // 纯文本 AI 消息无 blocks，tool_calls 由下方单独处理
                        for tc in tool_calls {
                            let tool_call =
                                ToolCall::new(ToolCallId::new(tc.id.clone()), tc.name.clone())
                                    .raw_input(Some(tc.arguments.clone()))
                                    .status(ToolCallStatus::InProgress);
                            let update = SessionUpdate::ToolCall(replay_tool(tool_call));
                            let notif = SessionNotification::new(
                                SessionId::new(session_id.to_string()),
                                update,
                            );
                            sender.send(notif).await?;
                        }
                        continue;
                    }
                    PeriMessageContent::Blocks(blocks) => blocks,
                    PeriMessageContent::Raw(_) => continue,
                };

                for block in blocks {
                    match block {
                        PeriContentBlock::Text { text } => {
                            let update = SessionUpdate::AgentMessageChunk(replay_chunk(
                                ContentBlock::Text(TextContent::new(text.clone())),
                            ));
                            let notif = SessionNotification::new(
                                SessionId::new(session_id.to_string()),
                                update,
                            );
                            sender.send(notif).await?;
                        }
                        PeriContentBlock::ToolUse { id, name, input } => {
                            emitted_ids.insert(id.clone());
                            let tc = ToolCall::new(ToolCallId::new(id.clone()), name.clone())
                                .raw_input(Some(input.clone()))
                                .status(ToolCallStatus::InProgress);
                            let update = SessionUpdate::ToolCall(replay_tool(tc));
                            let notif = SessionNotification::new(
                                SessionId::new(session_id.to_string()),
                                update,
                            );
                            sender.send(notif).await?;
                        }
                        // Image / Document / Reasoning / Unknown → 跳过
                        _ => {}
                    }
                }

                // 发射 tool_calls 中未被 ContentBlock::ToolUse 覆盖的条目
                for tc in tool_calls {
                    if !emitted_ids.contains(&tc.id) {
                        let tool_call =
                            ToolCall::new(ToolCallId::new(tc.id.clone()), tc.name.clone())
                                .raw_input(Some(tc.arguments.clone()))
                                .status(ToolCallStatus::InProgress);
                        let update = SessionUpdate::ToolCall(replay_tool(tool_call));
                        let notif = SessionNotification::new(
                            SessionId::new(session_id.to_string()),
                            update,
                        );
                        sender.send(notif).await?;
                    }
                }
            }
            BaseMessage::Tool {
                content,
                is_error,
                tool_call_id,
                ..
            } => {
                let result_text = extract_text(content);
                let fields = ToolCallUpdateFields::new()
                    .status(Some(if *is_error {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    }))
                    .raw_output(Some(serde_json::Value::String(result_text)));
                let update = SessionUpdate::ToolCallUpdate(replay_tool_update(
                    ToolCallUpdate::new(ToolCallId::new(tool_call_id.clone()), fields),
                ));
                let notif =
                    SessionNotification::new(SessionId::new(session_id.to_string()), update);
                sender.send(notif).await?;
            }
            _ => continue,
        }
    }
    Ok(())
}

fn replay_chunk(content: ContentBlock) -> ContentChunk {
    let mut chunk = ContentChunk::new(content);
    let mut meta = serde_json::Map::new();
    meta.insert("periReplay".to_string(), serde_json::Value::Bool(true));
    chunk.meta = Some(meta);
    chunk
}

/// 给 `ToolCall` 打上 periReplay meta 标记。
fn replay_tool(mut tc: ToolCall) -> ToolCall {
    let mut meta = serde_json::Map::new();
    meta.insert("periReplay".to_string(), serde_json::Value::Bool(true));
    tc.meta = Some(meta);
    tc
}

/// 给 `ToolCallUpdate` 打上 periReplay meta 标记。
fn replay_tool_update(mut tu: ToolCallUpdate) -> ToolCallUpdate {
    let mut meta = serde_json::Map::new();
    meta.insert("periReplay".to_string(), serde_json::Value::Bool(true));
    tu.meta = Some(meta);
    tu
}

/// Extract plain text from a `MessageContent`.
fn extract_text(content: &PeriMessageContent) -> String {
    match content {
        PeriMessageContent::Text(s) => s.clone(),
        PeriMessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                PeriContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        PeriMessageContent::Raw(_) => String::new(),
    }
}

/// Abstraction over how to send a `SessionNotification`.
#[async_trait::async_trait]
pub trait ReplaySender: Send + Sync {
    async fn send(&self, notif: SessionNotification) -> Result<(), ReplayError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("transport send failed: {0}")]
    SendFailed(String),
}
