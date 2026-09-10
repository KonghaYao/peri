//! SDK stdio sink：标准 SessionUpdate 与来源 metadata。

use super::{Client, ConnectionTo, EventSink, SdkSessionId, SessionNotification, SessionUpdate};
use crate::event::map_event;
use async_trait::async_trait;
use peri_acp_types::{event::ExecutorEvent, PeriCaps};
use serde_json::json;
use tracing::error;

/// Build ACP-standard metadata for routing output to its originating SubAgent.
fn source_agent_meta(source_agent_id: &str) -> agent_client_protocol::schema::v1::Meta {
    serde_json::Map::from_iter([(
        "peri".to_string(),
        json!({ "sourceAgentId": source_agent_id }),
    )])
}

/// Attach source identity to the typed notification field preserved by ACP SDKs.
pub(super) fn session_notification(
    session_id: SdkSessionId,
    update: SessionUpdate,
    source_agent_id: Option<&str>,
) -> SessionNotification {
    let notification = SessionNotification::new(session_id, update);
    match source_agent_id {
        Some(source_agent_id) => notification.meta(source_agent_meta(source_agent_id)),
        None => notification,
    }
}

// ── SDK-backed EventSink for stdio path ─────────────────────────────────────

/// [`EventSink`] backed by the SDK's [`ConnectionTo<Client>`].
///
/// Sends standard ACP `session/update` notifications only (no `peri/*` custom
/// notifications — those are TUI-specific). Used by the stdio `peri acp` mode
/// which communicates with external IDE clients via the agent-client-protocol SDK.
pub struct StdioEventSink {
    cx: ConnectionTo<Client>,
    session_id: SdkSessionId,
    caps: PeriCaps,
}

impl StdioEventSink {
    pub fn new(cx: ConnectionTo<Client>, session_id: SdkSessionId, caps: PeriCaps) -> Self {
        Self {
            cx,
            session_id,
            caps,
        }
    }

    /// Send an arbitrary `SessionUpdate` notification through the SDK connection.
    pub fn send_update(&self, update: SessionUpdate) {
        let notif = SessionNotification::new(self.session_id.clone(), update);
        if let Err(e) = self.cx.send_notification(notif) {
            error!(error = %e, "StdioEventSink: failed to send SessionUpdate");
        }
    }
}

#[async_trait]
impl EventSink for StdioEventSink {
    async fn push_event(&self, _session_id: &str, event: &ExecutorEvent, context_window: u32) {
        let mapped = map_event(event, context_window, &self.caps);
        for m in mapped {
            for update in m.updates {
                let notif = session_notification(
                    self.session_id.clone(),
                    update,
                    m.source_agent_id.as_deref(),
                );
                if let Err(e) = self.cx.send_notification(notif) {
                    error!(error = %e, "StdioEventSink: failed to send SessionNotification");
                    break;
                }
            }
        }
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {
        // No explicit done signal in standard ACP protocol.
    }
}
