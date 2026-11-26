//! 已持久化会话条目到官方 `AgentSessionItem` 的投影。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ItemKind, ItemView, OfficialContent, TurnStatus};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OfficialSessionItem {
    Message {
        id: Option<String>,
        content: Vec<OfficialContent>,
        phase: Option<MessagePhase>,
        role: MessageRole,
        status: OutputItemStatus,
        turn_id: String,
    },
    Reasoning {
        id: String,
        status: Option<OutputItemStatus>,
        summary: Vec<SummaryText>,
        turn_id: String,
    },
    FunctionCall {
        id: String,
        arguments: Value,
        call_id: String,
        name: String,
        status: FunctionCallStatus,
        turn_id: String,
    },
    FunctionCallOutput {
        id: String,
        call_id: String,
        error: Option<String>,
        output: Option<String>,
        status: FunctionCallStatus,
        turn_id: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePhase {
    Commentary,
    FinalAnswer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputItemStatus {
    InProgress,
    Completed,
    Incomplete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FunctionCallStatus {
    InProgress,
    Completed,
    Failed,
    Incomplete,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SummaryText {
    SummaryText { text: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GroupClass {
    User,
    CompleteAssistant,
    UnfinishedAssistant,
    Other,
}

pub(crate) struct TurnContext<'a> {
    pub id: &'a str,
    pub status: Option<TurnStatus>,
}

/// 页内同一 itemId 的行，按 seq 升序；单行与历史多行使用同一规则。
pub(crate) struct ItemGroup<'a> {
    rows: Vec<&'a ItemView>,
    pub selected: &'a ItemView,
    pub class: GroupClass,
}

impl<'a> ItemGroup<'a> {
    pub(crate) fn new(mut rows: Vec<&'a ItemView>) -> Option<Self> {
        rows.sort_unstable_by_key(|row| row.seq);
        let last = *rows.last()?;
        let complete = rows.iter().rev().copied().find(|row| {
            row.source.is_root()
                && row.kind == last.kind
                && (row.kind == ItemKind::Thought
                    || (row.kind == ItemKind::Message
                        && text_field(&row.payload, "role") == Some("assistant")))
                && row.payload["complete"].as_bool() == Some(true)
        });
        let (selected, class) = match (last.kind, text_field(&last.payload, "role"), complete) {
            (ItemKind::Message, Some("user"), _) if last.source.is_root() => {
                (last, GroupClass::User)
            }
            (ItemKind::Message, _, Some(row)) => (row, GroupClass::CompleteAssistant),
            (ItemKind::Message, Some("assistant"), None) if last.source.is_root() => {
                (last, GroupClass::UnfinishedAssistant)
            }
            (ItemKind::Thought, _, Some(row)) => (row, GroupClass::Other),
            _ => (last, GroupClass::Other),
        };
        Some(Self {
            rows,
            selected,
            class,
        })
    }

    fn joined_text(&self) -> Option<String> {
        // 拼接只使用完整的根会话帧；缺帧时不将残缺文本冒充完整内容。
        if self
            .rows
            .iter()
            .any(|row| !row.source.is_root() || row.truncated)
        {
            return None;
        }
        Some(
            self.rows
                .iter()
                .filter_map(|row| text_field(&row.payload, "text"))
                .collect(),
        )
    }
}

pub(crate) fn map_group(
    group: &ItemGroup<'_>,
    context: Option<TurnContext<'_>>,
) -> Option<OfficialSessionItem> {
    let context = context?;
    let row = group.selected;
    if row.truncated || !row.source.is_root() {
        return None;
    }
    let id = row.item_id.clone();
    let turn_id = context.id.to_owned();
    let payload = &row.payload;
    let text = || text_field(payload, "text").unwrap_or_default().to_owned();
    Some(match row.kind {
        ItemKind::Message => {
            let phase = match text_field(payload, "phase") {
                Some("commentary") => Some(MessagePhase::Commentary),
                Some("final_answer") => Some(MessagePhase::FinalAnswer),
                _ => None,
            };
            let (role, content, phase, status) = match group.class {
                GroupClass::User => (
                    MessageRole::User,
                    OfficialContent::InputText { text: text() },
                    None,
                    OutputItemStatus::Completed,
                ),
                GroupClass::CompleteAssistant => (
                    MessageRole::Assistant,
                    OfficialContent::OutputText { text: text() },
                    phase.or(Some(MessagePhase::FinalAnswer)),
                    OutputItemStatus::Completed,
                ),
                GroupClass::UnfinishedAssistant => (
                    MessageRole::Assistant,
                    OfficialContent::OutputText {
                        text: group.joined_text()?,
                    },
                    phase,
                    if context.status.is_some_and(|status| !status.is_open()) {
                        OutputItemStatus::Incomplete
                    } else {
                        OutputItemStatus::InProgress
                    },
                ),
                GroupClass::Other => return None,
            };
            OfficialSessionItem::Message {
                id: Some(id),
                content: vec![content],
                phase,
                role,
                status,
                turn_id,
            }
        }
        ItemKind::Thought => {
            let text = if payload["complete"].as_bool() == Some(true) {
                text()
            } else {
                group.joined_text()?
            };
            if text.is_empty() {
                return None;
            }
            OfficialSessionItem::Reasoning {
                id,
                status: None,
                summary: vec![SummaryText::SummaryText { text }],
                turn_id,
            }
        }
        ItemKind::ToolCall => OfficialSessionItem::FunctionCall {
            id,
            arguments: payload
                .get("input")
                .filter(|input| !input.is_null())
                .or_else(|| payload.get("body"))
                .cloned()
                .unwrap_or(Value::Null),
            call_id: nonempty_field(payload, "toolCallId")?.to_owned(),
            name: nonempty_field(payload, "title")?.to_owned(),
            status: call_status(payload).unwrap_or(FunctionCallStatus::InProgress),
            turn_id,
        },
        ItemKind::ToolResult => {
            let output = payload
                .get("output")
                .filter(|value| !value.is_null())
                .map(|value| {
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                });
            let status = call_status(payload).unwrap_or(if output.is_some() {
                FunctionCallStatus::Completed
            } else {
                FunctionCallStatus::Incomplete
            });
            let (error, output) = if status == FunctionCallStatus::Failed {
                (output, None)
            } else {
                (None, output)
            };
            OfficialSessionItem::FunctionCallOutput {
                id,
                call_id: nonempty_field(payload, "toolCallId")?.to_owned(),
                error,
                output,
                status,
                turn_id,
            }
        }
        ItemKind::ToolUpdate | ItemKind::Receipt | ItemKind::Approval => return None,
    })
}

fn text_field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

fn nonempty_field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    text_field(payload, key).filter(|value| !value.trim().is_empty())
}

fn call_status(payload: &Value) -> Option<FunctionCallStatus> {
    match text_field(payload, "status") {
        Some("pending" | "in_progress") => Some(FunctionCallStatus::InProgress),
        Some("completed") => Some(FunctionCallStatus::Completed),
        Some("failed") => Some(FunctionCallStatus::Failed),
        _ => None,
    }
}

#[cfg(test)]
#[path = "official_item_test.rs"]
mod tests;
