//! History V2 的展示投影与有界可见性判定。
//!
//! 投影规则只有这一份：own 与 inherited 都经 [`dispatch::project_payload_updates`] 与
//! [`dispatch::reminder_visible_to_client`]。oversize 条目拿不到完整 payload 时，只允许
//! 读取固定前缀判断受众，不能为了可见性读取整条正文。

use peri_acp_types::history_v2::{
    HistoryNoticeSubItem, HistorySubItem, HistoryTextSubItem, HistoryToolCallSubItem,
    HistoryToolResultSubItem,
};

use crate::dispatch::{self, ProjectedUpdate};
use agent_client_protocol_schema::v1::{ContentBlock, ContentChunk, SessionUpdate};

/// oversize 可见性判定的最大前缀字节数；不得为判定读取整条正文。
pub(super) const VISIBILITY_PREFIX_BYTES: u32 = 1024;

/// 一个投影子项 → V2 语义子项；无法映射时记录告警并跳过。
pub(super) fn projected_sub_item(
    update: ProjectedUpdate,
    session_id: &str,
    entry_id: &str,
    index: usize,
) -> Option<HistorySubItem> {
    let sub_item_id = format!("{entry_id}:{index}");
    match update {
        ProjectedUpdate::Session(update) => {
            match *update {
                SessionUpdate::UserMessageChunk(chunk) => Some(HistorySubItem::Text(
                    text_sub_item(sub_item_id, "user", &chunk)?,
                )),
                SessionUpdate::AgentMessageChunk(chunk) => Some(HistorySubItem::Text(
                    text_sub_item(sub_item_id, "assistant", &chunk)?,
                )),
                SessionUpdate::AgentThoughtChunk(chunk) => Some(HistorySubItem::Reasoning(
                    text_sub_item(sub_item_id, "assistant", &chunk)?,
                )),
                SessionUpdate::ToolCall(tool_call) => {
                    Some(HistorySubItem::ToolCall(HistoryToolCallSubItem {
                        sub_item_id,
                        // 调用与结果跨历史行配对：键只由会话与 toolCallId 决定，
                        // 不能把各自所在 entryId 混入，否则同一调用会得到两个键。
                        invocation_key: format!("{session_id}:{}", tool_call.tool_call_id.0),
                        title: tool_call.title.clone(),
                        status: tool_status(tool_call.status),
                        input: Some(
                            tool_call
                                .raw_input
                                .clone()
                                .unwrap_or(serde_json::Value::Null),
                        ),
                        body: None,
                    }))
                }
                SessionUpdate::ToolCallUpdate(update) => {
                    let Some(status) = update.fields.status else {
                        tracing::warn!(
                            entry_id,
                            "history v2 projection skipped a tool result without status"
                        );
                        return None;
                    };
                    let output = match update.fields.raw_output {
                        Some(serde_json::Value::String(text)) => text,
                        Some(value) => serde_json::to_string(&value).ok()?,
                        None => {
                            tracing::warn!(
                                entry_id,
                                "history v2 projection skipped a tool result without output"
                            );
                            return None;
                        }
                    };
                    let tool_call_id = update.tool_call_id.0;
                    // 结果缺少调用身份时无法与任何调用配对，使用条目级稳定键显式标记，
                    // 不以结果自身的 entryId 冒充调用身份。
                    let invocation_key = if tool_call_id.is_empty() {
                        format!("tool_result:{entry_id}")
                    } else {
                        format!("{session_id}:{tool_call_id}")
                    };
                    Some(HistorySubItem::ToolResult(HistoryToolResultSubItem {
                        sub_item_id,
                        invocation_key,
                        status: tool_status(status),
                        output: Some(output),
                        body: None,
                    }))
                }
                other => {
                    tracing::warn!(
                        entry_id,
                        variant = session_update_name(&other),
                        "history v2 projection skipped an unsupported session update"
                    );
                    None
                }
            }
        }
        ProjectedUpdate::SystemReminder(reminder) => {
            if !dispatch::reminder_visible_to_client(&reminder) {
                return None;
            }
            Some(HistorySubItem::Reminder(HistoryNoticeSubItem {
                sub_item_id,
                category: enum_name(&reminder.category),
                title: reminder
                    .summary
                    .clone()
                    .unwrap_or_else(|| reminder.kind.clone()),
                text: Some(reminder.body.clone()),
                body: None,
            }))
        }
    }
}

fn text_sub_item(
    sub_item_id: String,
    role: &str,
    chunk: &ContentChunk,
) -> Option<HistoryTextSubItem> {
    let ContentBlock::Text(text) = &chunk.content else {
        tracing::warn!(
            sub_item_id,
            "history v2 projection skipped a non-text content chunk"
        );
        return None;
    };
    Some(HistoryTextSubItem {
        sub_item_id,
        role: role.to_owned(),
        text: Some(text.text.clone()),
        body: None,
    })
}

fn tool_status(status: agent_client_protocol_schema::v1::ToolCallStatus) -> String {
    enum_name(&status)
}

fn enum_name<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => "unknown".to_owned(),
    }
}

/// 稳定的 variant 名，仅用于告警，不进入 wire。
fn session_update_name(update: &SessionUpdate) -> &'static str {
    match update {
        SessionUpdate::UserMessageChunk(_) => "user_message_chunk",
        SessionUpdate::AgentMessageChunk(_) => "agent_message_chunk",
        SessionUpdate::AgentThoughtChunk(_) => "agent_thought_chunk",
        SessionUpdate::ToolCall(_) => "tool_call",
        SessionUpdate::ToolCallUpdate(_) => "tool_call_update",
        _ => "other",
    }
}

/// 1024 字节前缀是否证明 reminder 面向 TUI；`None` = 无法判定（fail-closed）。
pub(super) fn reminder_allows_tui(prefix: &[u8]) -> Option<bool> {
    let text = std::str::from_utf8(prefix).ok()?;
    if !text.contains("\"type\":\"system_reminder\"") {
        return None;
    }
    let audiences = json_array_after(text, "\"audiences\"")?;
    let audiences: Vec<String> = serde_json::from_str(audiences).ok()?;
    Some(audiences.iter().any(|audience| audience == "tui"))
}

/// 未知 role 的保守判定：前缀明确是 system / system_reminder 才处理；
/// 前缀可能在标记之前被截断时 fail-closed。
pub(super) fn unknown_role_visible(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    if text.contains("\"type\":\"system_reminder\"") {
        return reminder_allows_tui(prefix).unwrap_or(false);
    }
    if text.contains("\"role\":\"system\"") || text.contains("\"type\":\"system\"") {
        return false;
    }
    if prefix.len() as u32 == VISIBILITY_PREFIX_BYTES {
        tracing::warn!("history v2 omitted an oversized unknown-role entry: prefix truncated");
        return false;
    }
    true
}

/// inherited oversize 没有 role 列：只用有界前缀判定受众。
///
/// - `system_reminder`：`audiences` 含 `tui` 才可见；
/// - `message.role=system`：跳过；
/// - 明确 user/assistant/tool：可见；
/// - 前缀未覆盖身份字段或信封未知：fail-closed。
pub(super) fn inherited_prefix_visible(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        tracing::warn!("history v2 omitted an inherited oversized entry: prefix is not UTF-8");
        return false;
    };
    if text.contains("\"type\":\"system_reminder\"") {
        return match reminder_allows_tui(prefix) {
            Some(visible) => visible,
            None => {
                tracing::warn!(
                    "history v2 omitted an inherited oversized reminder: audiences undetermined"
                );
                false
            }
        };
    }
    if text.contains("\"role\":\"system\"") || text.contains("\"type\":\"system\"") {
        return false;
    }
    if text.contains("\"role\":\"user\"")
        || text.contains("\"role\":\"assistant\"")
        || text.contains("\"role\":\"tool\"")
    {
        return true;
    }
    tracing::warn!("history v2 omitted an inherited oversized entry: role prefix undetermined");
    false
}

/// 在文本中定位 `marker` 之后的第一个完整 JSON 数组切片（尊重字符串与转义）。
fn json_array_after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let marker_at = text.find(marker)? + marker.len();
    let array_start = marker_at + text[marker_at..].find('[')?;
    let bytes = text.as_bytes();
    let mut index = array_start;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&text[array_start..=index]);
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    None
}
