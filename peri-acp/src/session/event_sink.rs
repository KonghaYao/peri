//! Event sink abstraction for ACP session event routing.
//!
//! Different frontends (TUI via MpscTransport, IDE via stdio SDK) route agent
//! execution events differently. [`EventSink`] abstracts this so the core
//! prompt execution logic can live in `peri-acp`.

// Re-export SDK types used by StdioEventSink.
pub use agent_client_protocol::{
    schema::{SessionId as SdkSessionId, SessionNotification, SessionUpdate},
    Client, ConnectionTo,
};
use async_trait::async_trait;
use peri_agent::agent::events::ExecutorEvent;
use serde_json::json;
use tracing::{debug, error};

use crate::{event::map_event, event::router, transport::AcpTransport};

/// Receives [`ExecutorEvent`]s produced during agent execution and routes them
/// to the appropriate transport.
#[async_trait]
pub trait EventSink: Send + Sync {
    /// Push a single executor event. Called from the background pump task.
    async fn push_event(&self, session_id: &str, event: &ExecutorEvent, context_window: u32);

    /// Signal that the agent execution stream has ended (no more events).
    async fn push_done(&self, session_id: &str);
}

// ── TUI transport-backed EventSink ──────────────────────────────────────────

/// [`EventSink`] backed by an [`AcpTransport`]. Sends two notification types:
/// - `session/update` — standard ACP SessionUpdate (with `_peri` metadata for TUI)
/// - `peri/agent_event` — raw serialized ExecutorEvent (for TUI-only events, categories ②③)
///
/// Additionally, each event is routed through the event router to emit
/// `peri/unstable-event` notifications for new-protocol consumers.
pub struct TransportEventSink {
    transport: std::sync::Arc<dyn AcpTransport>,
    /// Caching ViewMapper for the event router (interior mutability for async context).
    view_mapper: parking_lot::Mutex<crate::event::ViewMapperImpl>,
}

impl TransportEventSink {
    pub fn new(transport: std::sync::Arc<dyn AcpTransport>) -> Self {
        Self {
            transport,
            view_mapper: parking_lot::Mutex::new(crate::event::ViewMapperImpl::new()),
        }
    }

    /// Push a `{event, data}` custom event through `peri/unstable-event` channel.
    ///
    /// Used by the event router to emit new-protocol events alongside the
    /// existing `peri/agent_event` path. The envelope is a JSON-RPC notification:
    /// ```json
    /// {"jsonrpc":"2.0","method":"peri/unstable-event","params":{"event":"...","data":{...}}}
    /// ```
    pub async fn push_unstable_event(
        &self,
        session_id: &str,
        event: String,
        data: serde_json::Value,
    ) -> Result<(), crate::transport::types::AcpError> {
        let payload = json!({
            "sessionId": session_id,
            "event": event,
            "data": data,
        });
        self.transport
            .send_notification("peri/unstable-event", payload)
            .await
    }
}

#[async_trait]
impl EventSink for TransportEventSink {
    async fn push_event(&self, session_id: &str, event: &ExecutorEvent, context_window: u32) {
        let mapped = map_event(event, context_window);

        for m in mapped {
            // 1. session/update — 标准 ACP 通知（Category ①）
            for update in m.updates {
                let update_value = match serde_json::to_value(&update) {
                    Ok(p) => p,
                    Err(e) => {
                        error!(error = %e, "EventSink: serialize SessionUpdate failed");
                        continue;
                    }
                };
                // Wrap in {"update": ..., "sessionId": ...} format expected by
                // handle_session_update_peri on the TUI side.
                let mut payload = serde_json::json!({
                    "sessionId": session_id,
                    "update": update_value,
                });
                // Inject _peri metadata for TUI consumption (source_agent_id)
                if let Some(ref aid) = m.source_agent_id {
                    if let serde_json::Value::Object(ref mut map) = payload {
                        map.insert("_peri".to_string(), json!({ "sourceAgentId": aid }));
                    }
                }
                let _ = self
                    .transport
                    .send_notification("session/update", payload)
                    .await;
            }

            // 2. peri/agent_event — TUI 专用事件（Category ③）
            // Convert ExecutorEvent → AcpEvent DTO before serialization.
            if m.forward_to_tui {
                if let Some(acp_event) = crate::event::executor_event_to_acp(event) {
                    let event_json = match serde_json::to_string(&acp_event) {
                        Ok(s) => s,
                        Err(e) => {
                            error!(error = %e, "EventSink: serialize AcpEvent failed");
                            continue;
                        }
                    };
                    let agent_event_params = json!({
                        "sessionId": session_id,
                        "event_json": event_json,
                    });
                    if let Err(e) = self
                        .transport
                        .send_notification("peri/agent_event", agent_event_params)
                        .await
                    {
                        error!(error = %e, "EventSink: send peri/agent_event failed");
                    }
                }
            }

            // 3. peri/hitl_pending — HITL 审批事件（Category ②）
            // 预留：当前 HITL 通过 UserInteractionBroker 直接交互，
            // 未来 ExecutorEvent 扩展 HitlPending 时启用此通道。
            if m.hitl_pending {
                let _ = self
                    .transport
                    .send_notification("peri/hitl_pending", json!({ "sessionId": session_id }))
                    .await;
            }

            // 4. peri/observable — 观测层事件（Category ④）
            // 预留：当前无外部订阅者，未来通过 broadcast channel 分发。
            if m.observable {
                tracing::trace!(
                    session_id = %session_id,
                    event = ?event,
                    "EventSink: observable event (no subscribers yet)"
                );
            }

            // 5. peri/unstable-event — new-protocol event routing (Category ⑤)
            // Route each ExecutorEvent through the event router. Events that
            // map to a RoutingOutput are forwarded as unstable events; discarded
            // events return None and are silently dropped.
            let routing_out = {
                let mut vm = self.view_mapper.lock();
                router::route(event, &mut *vm)
            };
            if let Some(out) = routing_out {
                if let Err(e) = self
                    .push_unstable_event(session_id, out.event_name, out.data)
                    .await
                {
                    tracing::trace!(
                        session_id = %session_id,
                        error = %e,
                        "EventSink: peri/unstable-event send failed (non-critical)"
                    );
                }
            }
        }
    }

    async fn push_done(&self, session_id: &str) {
        debug!(session_id = %session_id, "EventSink: sending agent_event_done");
        if let Err(e) = self
            .transport
            .send_notification("peri/agent_event_done", json!({ "sessionId": session_id }))
            .await
        {
            error!(session_id = %session_id, error = %e, "EventSink: agent_event_done send failed")
        }
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
}

impl StdioEventSink {
    pub fn new(cx: ConnectionTo<Client>, session_id: SdkSessionId) -> Self {
        Self { cx, session_id }
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
        let mapped = map_event(event, context_window);
        for m in mapped {
            for update in m.updates {
                let notif = SessionNotification::new(self.session_id.clone(), update);
                if let Err(e) = self.cx.send_notification(notif) {
                    error!(error = %e, "StdioEventSink: failed to send SessionNotification");
                    break;
                }
            }
        }
    }

    async fn push_done(&self, _session_id: &str) {
        // No explicit done signal in standard ACP protocol.
    }
}
