//! Read-only transcript formatting; it never touches the active session projection.

use peri_acp_types::{messages::BaseMessage, store::PersistedPayload};

pub(super) fn text(payloads: &[PersistedPayload]) -> String {
    let mut output = String::new();
    for payload in payloads {
        match payload {
            PersistedPayload::Message(message) => {
                let label = match message {
                    BaseMessage::Human { .. } => "thread-history-user",
                    BaseMessage::Ai { .. } => "thread-history-assistant",
                    BaseMessage::System { .. } => "thread-history-system",
                    BaseMessage::Tool { .. } => "thread-history-tool",
                };
                output.push_str(&crate::i18n::tr(label));
                output.push('\n');
                output.push_str(&message.content());
                for call in message.tool_calls() {
                    output.push('\n');
                    // Preserve tool requests, including arguments, in the read-only transcript.
                    output
                        .push_str(&serde_json::to_string_pretty(call).expect("tool call is JSON"));
                }
            }
            PersistedPayload::SystemReminder { reminder, .. } => {
                output.push_str(&crate::i18n::tr("thread-history-system"));
                output.push('\n');
                output.push_str(
                    &serde_json::to_string_pretty(reminder.as_reminder())
                        .expect("reminder is JSON"),
                );
            }
        }
        output.push_str("\n\n");
    }
    output.retain(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'));
    output
}
