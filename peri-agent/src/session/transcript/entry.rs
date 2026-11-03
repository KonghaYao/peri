//! Transcript 条目与暂存模型（[`super::MessageTranscript`] 的值类型）。
//!
//! 条目直接表达 canonical 形态——普通消息与 canonical reminder 同级，reminder 不是伪造的
//! 空 Human message；出口可见性与持久化载荷映射只在这里定义一份。

use std::borrow::Cow;

use crate::messages::{BaseMessage, MessageContent, MessageId};

use peri_acp_types::store::PersistedPayload;
use peri_acp_types::system_reminder::{
    encode_system_reminder, reminder_egress_allowed, ReminderAudience, TrustedSystemReminder,
};

// ─── TranscriptEntry ──────────────────────────────────────────────────────────

/// Transcript 中的单条逻辑条目。Reminder 不是伪造的空 Human message。
#[derive(Debug, Clone)]
pub enum TranscriptEntry {
    Message(BaseMessage),
    Reminder {
        id: MessageId,
        reminder: TrustedSystemReminder,
    },
}

impl TranscriptEntry {
    pub fn id(&self) -> MessageId {
        match self {
            Self::Message(message) => message.id(),
            Self::Reminder { id, .. } => *id,
        }
    }

    pub fn as_message(&self) -> Option<&BaseMessage> {
        match self {
            Self::Message(message) => Some(message),
            Self::Reminder { .. } => None,
        }
    }

    pub fn message(&self) -> &BaseMessage {
        self.as_message()
            .expect("canonical reminder has no stored BaseMessage")
    }

    /// 该条目是否应向目标受众投递。
    ///
    /// 普通消息恒可见；canonical reminder 一律经 [`reminder_egress_allowed`] 的单一
    /// 投递规则判定（`Required` 只在声明受众内不可被屏蔽，`DiagnosticOnly`
    /// 默认只进 Diagnostics）。持久化保留 canonical reminder，过滤只发生在出口。
    pub fn visible_to(&self, audience: ReminderAudience) -> bool {
        match self {
            Self::Message(_) => true,
            Self::Reminder { reminder, .. } => {
                reminder_egress_allowed(reminder.as_reminder(), audience)
            }
        }
    }

    /// Canonical model projection shared by normal Reason and compact rendering.
    /// 转为 canonical 持久化载荷（与 `active_payloads` 同一映射，不另立格式）。
    pub fn into_payload(self) -> PersistedPayload {
        match self {
            Self::Message(message) => PersistedPayload::Message(message),
            Self::Reminder { id, reminder } => PersistedPayload::SystemReminder { id, reminder },
        }
    }

    /// Canonical model projection shared by normal Reason and compact rendering.
    ///
    /// 普通消息借用 canonical 正文；only legacy compact reminders and canonical
    /// reminders are re-encoded into an owned message.
    pub fn project_message_ref(&self) -> anyhow::Result<Cow<'_, BaseMessage>> {
        match self {
            Self::Message(message) => {
                let reminders = peri_acp_types::compact_reminder::legacy_compact_reminders(message);
                if reminders.is_empty() {
                    return Ok(Cow::Borrowed(message));
                }
                let encoded = reminders
                    .iter()
                    .map(|reminder| {
                        peri_acp_types::system_reminder::encode_legacy_system_reminder(
                            &reminder.body,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join("\n");
                Ok(Cow::Owned(BaseMessage::Human {
                    id: message.id(),
                    content: MessageContent::text(encoded),
                }))
            }
            Self::Reminder { id, reminder } => Ok(Cow::Owned(BaseMessage::Human {
                id: *id,
                content: MessageContent::text(encode_system_reminder(reminder)?),
            })),
        }
    }

    pub fn project_message(&self) -> anyhow::Result<BaseMessage> {
        Ok(self.project_message_ref()?.into_owned())
    }
}

// ─── StagedData ───────────────────────────────────────────────────────────────

/// 两阶段写入的暂存数据
///
/// AI 消息（含 tool_calls）先暂存，Act 阶段收集 ToolResult 后原子提交。
/// 提交前这些消息对 LLM 请求不可见。
#[derive(Debug, Clone)]
pub struct StagedData {
    pub ai_message: BaseMessage,
    pub tool_results: Vec<BaseMessage>,
}
