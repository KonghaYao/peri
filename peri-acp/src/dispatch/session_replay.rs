//! ACP session/load history replay via `session/update` notifications.
//!
//! Per ACP v1 spec, `session/load` MUST replay the entire conversation to the
//! client via `session/update` notifications (`user_message_chunk` +
//! `agent_message_chunk`) BEFORE responding to the request.
//!
//! Reference: <https://agentclientprotocol.com/protocol/v1/session-setup#loading-a-session>

use agent_client_protocol_schema::v1::{
    ContentBlock, ContentChunk, SessionId, SessionNotification, SessionUpdate, TextContent,
};
use peri_agent::messages::{
    BaseMessage, ContentBlock as PeriContentBlock, MessageContent as PeriMessageContent,
};

/// Replay session history via `session/update` notifications.
///
/// Iterates `history`, converting each `BaseMessage` into one or more
/// `SessionUpdate` variants, then calls `sender` for each notification.
///
/// - `BaseMessage::Human` → `SessionUpdate::UserMessageChunk`
/// - `BaseMessage::Ai`    → `SessionUpdate::AgentMessageChunk`
/// - Other variants        → silently skipped
pub async fn replay_session_history(
    session_id: &str,
    history: &[BaseMessage],
    sender: &dyn ReplaySender,
) -> Result<(), ReplayError> {
    for msg in history.iter().filter(|m| !m.is_system()) {
        let update = match msg {
            BaseMessage::Human { content, .. } => {
                let text = extract_text(content);
                SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::Text(
                    TextContent::new(text),
                )))
            }
            BaseMessage::Ai { content, .. } => {
                let text = extract_text(content);
                SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                    TextContent::new(text),
                )))
            }
            _ => continue,
        };
        let notif = SessionNotification::new(SessionId::new(session_id.to_string()), update);
        sender.send(notif).await?;
    }
    Ok(())
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
