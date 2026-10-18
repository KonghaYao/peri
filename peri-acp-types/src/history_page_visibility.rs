use serde::Deserialize;

use crate::messages::{BaseMessage, MessageId};
use crate::store::{PersistedPayload, PERSISTED_PAYLOAD_VERSION};
use crate::system_reminder::{SystemReminder, TrustedSystemReminderFactory};

// 消息正文由 serde 的 IgnoredAny 流式跳过，不随 oversize 大小分配内存。
// reminder 仍通过既有 DTO 校验和出口规则，不另写一份受众逻辑。
#[derive(Deserialize)]
struct VisibilityEnvelope {
    version: Option<u16>,
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<MessageRole>,
    reminder: Option<SystemReminder>,
    role: Option<Role>,
}

#[derive(Deserialize)]
struct MessageRole {
    role: Role,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Role {
    User,
    Assistant,
    System,
    Tool,
}

/// 流式检查持久正文的客户端可见性，不保留消息正文。
pub fn history_body_displayable_to_client(
    reader: impl std::io::Read,
) -> Result<bool, serde_json::Error> {
    let envelope: VisibilityEnvelope = serde_json::from_reader(reader)?;
    let payload = match (envelope.version, envelope.kind.as_deref()) {
        (None, None) => envelope.role.map(role_payload),
        (Some(PERSISTED_PAYLOAD_VERSION), Some("message")) => {
            envelope.message.map(|message| role_payload(message.role))
        }
        (Some(PERSISTED_PAYLOAD_VERSION), Some("system_reminder")) => envelope
            .reminder
            .and_then(|reminder| {
                TrustedSystemReminderFactory::for_recovery()
                    .construct(reminder)
                    .ok()
            })
            .map(|reminder| PersistedPayload::SystemReminder {
                id: MessageId::new(),
                reminder,
            }),
        _ => None,
    };
    Ok(payload.is_some_and(|payload| payload.displayable_to_client()))
}

fn role_payload(role: Role) -> PersistedPayload {
    PersistedPayload::Message(match role {
        Role::System => BaseMessage::system(""),
        Role::User | Role::Assistant | Role::Tool => BaseMessage::human(""),
    })
}

#[cfg(test)]
#[path = "history_page_visibility_test.rs"]
mod tests;
