//! Event sink abstraction for ACP session event routing.
//!
//! Different frontends (TUI via MpscTransport, IDE via stdio SDK) route agent
//! execution events differently. [`EventSink`] abstracts this so the core
//! prompt execution logic can live in `peri-acp`.
//!
//! L5：trait 定义已契约化至 `peri-acp-types::event::EventSink`（命令执行体 /
//! 事件发射辅助经契约端口调用），本模块保留 ACP 协议面实现
//! （TransportEventSink / StdioEventSink 等）。

mod legacy;
mod stdio;

#[cfg(test)]
use stdio::session_notification;
pub use stdio::StdioEventSink;

// Re-export SDK types used by StdioEventSink.
pub use agent_client_protocol::{
    schema::v1::{SessionId as SdkSessionId, SessionNotification, SessionUpdate},
    Client, ConnectionTo,
};
use async_trait::async_trait;
use dashmap::DashMap;
use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::PeriCaps;
use serde_json::json;
use std::sync::Arc;
use tracing::{debug, error};

use crate::{event::activity::map_agent_activity, event::map_event, transport::AcpTransport};

/// EventSink 契约（L5：事实源 peri-acp-types::event）。
pub use peri_acp_types::event::EventSink;

/// Receives [`ExecutorEvent`]s produced during agent execution and routes them
/// to the appropriate transport.
///
/// v1 `ExecutorEvent` 中间态已退役（批 2「v1-retire」）：本 trait 是 ACP 协议
/// 序列化面入口——输入为协议化载体事件（由 v2 事件经
/// `event_v2::*_event_to_executor` 转换而来，或命令/bg 等无 v2 等价物的
/// 功能载体事件），输出为 ACP wire 通知（SessionUpdate / AcpEvent）。
/// （L5：trait 定义契约化至 peri-acp-types，实现见下方。）
// ── TUI transport-backed EventSink ──────────────────────────────────────────
/// [`EventSink`] backed by an [`AcpTransport`]. Sends two notification types:
/// - `session/update` — standard ACP SessionUpdate (with ACP `_meta` routing metadata)
/// - `peri/agent_event` — AcpEvent DTO 序列化（TUI-only events，categories ②③）
///
/// Additionally, each event is routed through the event router to emit
/// `peri/unstable_event` notifications for new-protocol consumers.
pub struct TransportEventSink {
    transport: std::sync::Arc<dyn AcpTransport>,
    caps_registry: Arc<DashMap<String, PeriCaps>>,
}

impl TransportEventSink {
    pub(crate) async fn push_user_input_started(
        &self,
        session_id: &str,
        generation: String,
        request_id: String,
    ) -> Result<(), crate::transport::types::AcpError> {
        if !self
            .caps_registry
            .get(session_id)
            .is_some_and(|caps| caps.user_input_queue)
        {
            return Err(crate::transport::types::AcpError::new(
                -32601,
                "user input queue capability not negotiated",
            ));
        }
        let event = crate::event::AcpEvent::UserInputRunStarted {
            generation,
            request_id,
        };
        let event_json = serde_json::to_string(&event).map_err(|_| {
            crate::transport::types::AcpError::new(-32603, "user input start serialization failed")
        })?;
        self.transport
            .send_notification(
                "peri/agent_event",
                json!({
                    "sessionId": session_id,
                    "event_json": event_json,
                }),
            )
            .await
    }

    pub fn new(
        transport: std::sync::Arc<dyn AcpTransport>,
        caps_registry: Arc<DashMap<String, PeriCaps>>,
    ) -> Self {
        Self {
            transport,
            caps_registry,
        }
    }

    /// Push a `{event, data}` custom event through `peri/unstable_event` channel.
    ///
    /// Used by the event router to emit new-protocol events alongside the
    /// existing `peri/agent_event` path. The envelope is a JSON-RPC notification:
    /// ```json
    /// {"jsonrpc":"2.0","method":"peri/unstable_event","params":{"event":"...","data":{...}}}
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
            .send_notification("peri/unstable_event", payload)
            .await
    }
}

#[async_trait]
impl EventSink for TransportEventSink {
    async fn push_system_reminder(
        &self,
        session_id: &str,
        reminder: &peri_acp_types::system_reminder::SystemReminder,
        replay: bool,
    ) {
        let caps = self
            .caps_registry
            .get(session_id)
            .map(|caps| caps.clone())
            .unwrap_or_default();
        let (event, data) = if caps.system_reminder {
            (
                "system-reminder",
                json!({ "reminder": reminder, "replay": replay }),
            )
        } else {
            (
                "system-reminder-fallback",
                json!({ "text": reminder.summary.as_deref().unwrap_or(&reminder.body), "replay": replay, "legacy": true }),
            )
        };
        let _ = self
            .transport
            .send_notification(
                "peri/unstable_event",
                json!({ "sessionId": session_id, "event": event, "data": data }),
            )
            .await;
    }

    async fn push_event(&self, session_id: &str, event: &ExecutorEvent, context_window: u32) {
        if let ExecutorEvent::SystemReminder(reminder) = event {
            self.push_system_reminder(session_id, reminder, false).await;
            return;
        }
        let caps = self
            .caps_registry
            .get(session_id)
            .map(|r| r.clone())
            .unwrap_or_else(|| {
                tracing::error!(
                    session_id = %session_id,
                    "event_sink: session not found in caps_registry, falling back to all_enabled"
                );
                PeriCaps::all_enabled()
            });
        if matches!(
            event,
            ExecutorEvent::UserInputQueueChanged(_)
                | ExecutorEvent::UserInputRunStarted { .. }
                | ExecutorEvent::UserInputDelivered { .. }
        ) {
            if self
                .caps_registry
                .get(session_id)
                .is_some_and(|caps| caps.user_input_queue)
            {
                self.push_legacy_event(session_id, event).await;
            }
            return;
        }
        tracing::debug!(
            target: "acp.event_sink",
            session_id = %session_id,
            caps_found = self.caps_registry.contains_key(session_id),
            "push_event: caps registry lookup"
        );
        let mapped = map_event(event, context_window, &caps);

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
                tracing::debug!(
                    target: "acp.event_sink",
                    session_id = %session_id,
                    mapped.source_agent_id = ?m.source_agent_id,
                    "push_event: source_agent_id injection"
                );
                // ACP reserves params._meta for extension metadata and typed SDKs preserve it.
                // The source identity is routing semantics, so it is not capability-gated.
                if let Some(ref aid) = m.source_agent_id {
                    if let serde_json::Value::Object(ref mut map) = payload {
                        map.insert(
                            "_meta".to_string(),
                            json!({
                                "peri": { "sourceAgentId": aid }
                            }),
                        );
                    }
                }
                let _ = self
                    .transport
                    .send_notification("session/update", payload)
                    .await;
            }
        }

        // Privacy-safe GUI activity channel. This is intentionally independent
        // from legacy `peri/agent_event`: the mapper has already removed raw
        // messages, summaries, paths, outputs, errors and URLs before transport.
        if caps.agent_activity {
            if let Some(activity) = map_agent_activity(event) {
                if let Err(error) = self
                    .transport
                    .send_notification(
                        "peri/agent_activity",
                        json!({ "sessionId": session_id, "activity": activity }),
                    )
                    .await
                {
                    tracing::trace!(
                        session_id = %session_id,
                        error = %error,
                        "EventSink: agent activity send failed (non-critical)"
                    );
                }
            }
        }

        // 2. peri/agent_event — TUI 专用通知（Category ③）
        // SubagentStarted/SubagentStopped 等事件不产生 SessionUpdate，
        // 但必须通过 peri/agent_event 通道送达 TUI 以创建/销毁 SubAgentGroup 容器。
        if caps.agent_event {
            self.push_legacy_event(session_id, event).await;
        }
    }

    // 设计决策：ACP v1 无 turn_done SessionUpdate tag，TurnDone 信号通过
    // peri/agent_event_done 传输层通知传递。TUI 侧 acp_client/client.rs:188 将
    // transport 层 "peri/agent_event_done" method 映射为 AcpNotification::AgentDone，
    // acp_notifier.rs:127 再将 AgentDone 转换为 AcpEventData::TurnDone 推入双 bridge。
    // 若未来 ACP 标准协议新增 turn_done tag，应迁移至 session/update 标准通道。
    async fn push_done(&self, session_id: &str, stop_reason: &str, request_id: Option<&str>) {
        let caps = self
            .caps_registry
            .get(session_id)
            .map(|r| r.clone())
            .unwrap_or_else(|| {
                tracing::error!(
                    session_id = %session_id,
                    "event_sink: session not found in caps_registry, falling back to all_enabled"
                );
                PeriCaps::all_enabled()
            });
        if caps.agent_event_done || caps.user_input_queue {
            debug!(session_id = %session_id, "EventSink: sending agent_event_done");
            let mut payload = json!({ "sessionId": session_id, "stopReason": stop_reason });
            // requestId 为可选字段：有则回带（TUI stale TurnInterrupted 配对），
            // 无则省略（缺失路径如 continuation/Immediate 命令/stdio 不携带）。
            if let Some(rid) = request_id {
                payload["requestId"] = json!(rid);
            }
            if let Err(e) = self
                .transport
                .send_notification("peri/agent_event_done", payload)
                .await
            {
                error!(session_id = %session_id, error = %e, "EventSink: agent_event_done send failed")
            }
        } else {
            debug!(session_id = %session_id, "EventSink: agent_event_done suppressed (cap not declared)");
        }
    }

    async fn push_unstable_event(&self, session_id: &str, event: String, data: serde_json::Value) {
        let caps = self
            .caps_registry
            .get(session_id)
            .map(|r| r.clone())
            .unwrap_or_else(|| {
                tracing::error!(
                    session_id = %session_id,
                    "event_sink: session not found in caps_registry, falling back to all_enabled"
                );
                PeriCaps::all_enabled()
            });
        if !caps.unstable_event {
            tracing::warn!(
                session_id = %session_id,
                event_name = %event,
                "[caps] push_unstable_event: unstable_event cap not declared, event dropped"
            );
            return;
        }
        if let Err(e) = TransportEventSink::push_unstable_event(self, session_id, event, data).await
        {
            tracing::trace!(
                session_id = %session_id,
                error = %e,
                "EventSink: push_unstable_event failed (non-critical)"
            );
        }
    }

    async fn push_session_update(&self, session_id: &str, update: serde_json::Value) {
        let payload = serde_json::json!({
            "sessionId": session_id,
            "update": update,
        });
        let _ = self
            .transport
            .send_notification("session/update", payload)
            .await;
    }
}

#[cfg(test)]
#[path = "event_sink_test.rs"]
mod tests;
