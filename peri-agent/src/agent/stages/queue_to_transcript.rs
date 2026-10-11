//! Queue payload projection into the canonical transcript.

use crate::session::{MessageTranscript, QueuedMessage};

/// Writes drained queue payloads into the transcript without inferring semantics from scheduling.
pub fn append_messages_to_transcript(
    transcript: &mut MessageTranscript,
    messages: Vec<QueuedMessage>,
) {
    use crate::session::QueuedPayload;

    for msg in messages {
        debug_assert!(
            msg.delivery_id.is_none(),
            "stable-ID terminal reminders must pass through Receive's durable path"
        );
        match msg.payload {
            QueuedPayload::Message(message) => {
                if msg.kind == crate::session::MessageKind::Prompt
                    && message.message_content().is_empty()
                {
                    continue;
                }
                transcript.append(message);
            }
            QueuedPayload::SystemReminder(reminder) => {
                transcript.append_system_reminder(reminder);
            }
        }
    }
}
