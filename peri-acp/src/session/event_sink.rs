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
use peri_acp_types::event::{CompactStrategy, ExecutorEvent};
use peri_acp_types::PeriCaps;
use serde_json::json;
use std::sync::Arc;
use tracing::{debug, error};

use crate::host::live_delivery::{LiveDelivery, PublishOutcome};
use crate::{event::activity::map_agent_activity, event::map_event, transport::AcpTransport};

/// EventSink 契约（L5：事实源 peri-acp-types::event）。
pub use peri_acp_types::event::EventSink;
use peri_acp_types::event::EventSinkError;

/// Compact 提交事实的宿主观察者。
///
/// `CompactCompleted` 只表示“提交已发生、事件已产生”，不携带存储水位。窗口通知
/// 需要读取 Store 的精确边界并在 Host 侧发布，因此由宿主注入这个窄回调；事件
/// 序列化路径本身不查询存储。
///
/// 只有 Full 策略会触发回调：Micro/Smart 的投影变化不伪造 Full 检查点。
#[async_trait]
pub trait CompactCommitObserver: Send + Sync {
    async fn on_compact_committed(&self, session_id: &str);
}

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
    /// 宿主注入的 Full Compact 提交观察者；None 时保持纯事件转发。
    compact_commit_observer: Option<Arc<dyn CompactCommitObserver>>,
    /// History V2 实时投递注册表（连接级）；None 时 wire 与 V1 完全一致。
    live_delivery: Option<Arc<LiveDelivery>>,
}

impl TransportEventSink {
    pub(crate) async fn push_execution_started(
        &self,
        session_id: &str,
        generation: String,
        request_id: String,
    ) -> Result<(), crate::transport::types::AcpError> {
        let caps = self.caps_registry.get(session_id).map(|caps| caps.clone());
        if !caps.is_some_and(|caps| caps.agent_event || caps.user_input_queue) {
            return Ok(());
        }
        let event = crate::event::AcpEvent::ExecutionStarted {
            generation,
            request_id,
        };
        let event_json = serde_json::to_string(&event).map_err(|error| {
            crate::transport::types::AcpError::new(
                -32603,
                format!("execution start serialization failed: {error}"),
            )
        })?;
        self.transport
            .send_notification(
                "peri/agent_event",
                json!({
                    "sessionId": session_id, "event_json": event_json,
                }),
            )
            .await
    }

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
            compact_commit_observer: None,
            live_delivery: None,
        }
    }

    /// 注入本连接/本会话的 History V2 实时投递注册表。
    ///
    /// 只有 V2 `session/load` 成功装配的会话才有注册表；其余会话不调用本方法，
    /// `session/update` 的 wire 形状保持不变。
    pub(crate) fn with_live_delivery(mut self, live: Arc<LiveDelivery>) -> Self {
        self.live_delivery = Some(live);
        self
    }

    /// 注入 Full Compact 提交观察者。仅宿主装配路径使用；测试/轻量 sink 不注入时
    /// 行为与原来完全一致。
    pub fn with_compact_commit_observer(
        mut self,
        observer: Arc<dyn CompactCommitObserver>,
    ) -> Self {
        self.compact_commit_observer = Some(observer);
        self
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
        source_agent_id: Option<&str>,
    ) -> Result<(), EventSinkError> {
        // H8：客户端出口必须先按目标受众过滤；结构化事件与文本 fallback 使用
        // 同一条规则，fallback 不得绕过过滤下发 Model-only 内容。
        if !peri_acp_types::system_reminder::reminder_egress_allowed(
            reminder,
            peri_acp_types::system_reminder::ReminderAudience::Tui,
        ) {
            tracing::debug!(
                session_id = %session_id,
                source = %reminder.source,
                kind = %reminder.kind,
                delivery = ?reminder.delivery,
                replay,
                "system reminder is not addressed to the client audience; nothing is sent"
            );
            return Ok(());
        }
        let caps = self
            .caps_registry
            .get(session_id)
            .map(|caps| caps.clone())
            .unwrap_or_default();
        let (event, data) = if caps.system_reminder {
            // 转发展示归属（子 Agent 提醒）随结构化 DTO 一起下发；只影响客户端标注。
            let mut data = json!({ "reminder": reminder, "replay": replay });
            if let Some(agent_id) = source_agent_id {
                data["source_agent_id"] = json!(agent_id);
            }
            ("system-reminder", data)
        } else {
            (
                "system-reminder-fallback",
                json!({ "text": reminder.summary.as_deref().unwrap_or(&reminder.body), "replay": replay, "legacy": true }),
            )
        };
        self.transport
            .send_notification(
                "peri/unstable_event",
                json!({ "sessionId": session_id, "event": event, "data": data }),
            )
            .await
            .map_err(|error| EventSinkError::new(format!("system reminder send failed: {error}")))
    }

    async fn push_event(
        &self,
        session_id: &str,
        event: &ExecutorEvent,
        context_window: u32,
    ) -> Result<(), EventSinkError> {
        if let ExecutorEvent::SystemReminder {
            reminder,
            source_agent_id,
        } = event
        {
            self.push_system_reminder(session_id, reminder, false, source_agent_id.as_deref())
                .await?;
            return Ok(());
        }
        // Full Compact 的 durable 提交事实不能等 prompt 收尾，也不能依赖后续
        // 展示事件是否送达。仅在策略为 Full 时触发；Micro/Smart 不伪造检查点。
        if let ExecutorEvent::CompactCompleted { strategy, .. } = event {
            if *strategy == CompactStrategy::Full {
                if let Some(observer) = &self.compact_commit_observer {
                    observer.on_compact_committed(session_id).await;
                }
            }
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
                self.push_legacy_event(session_id, event).await?;
            }
            return Ok(());
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
                // 只有 transcript 内容更新进入 V2 实时交付序列；命令/配置/用量等
                // 通知保持原样（结构判定见 `is_transcript_update`）。
                let transcript = is_transcript_update(&update);
                let update_value = match serde_json::to_value(&update) {
                    Ok(p) => p,
                    Err(e) => {
                        error!(error = %e, "EventSink: serialize SessionUpdate failed");
                        return Err(EventSinkError::new(format!(
                            "serialize session update failed: {e}"
                        )));
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
                let payload = match (&self.live_delivery, transcript) {
                    (Some(live), true) => match live.publish(session_id, payload) {
                        PublishOutcome::Send(payload) => Some(payload),
                        PublishOutcome::Buffered => None,
                        PublishOutcome::Overflow => {
                            // 客户端会因序号缺口进入 detached；服务端只记录，不阻塞
                            // 发送路径，也不伪装连续。
                            tracing::warn!(
                                session_id = %session_id,
                                "history v2 live buffer overflow; dropped oldest update"
                            );
                            None
                        }
                    },
                    _ => Some(payload),
                };
                let Some(payload) = payload else {
                    continue;
                };
                self.transport
                    .send_notification("session/update", payload)
                    .await
                    .map_err(|error| {
                        EventSinkError::new(format!("session/update send failed: {error}"))
                    })?;
            }
        }

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
            self.push_legacy_event(session_id, event).await?;
        }
        Ok(())
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
        let transcript = serde_json::from_value::<SessionUpdate>(update.clone())
            .map(|update| is_transcript_update(&update))
            .unwrap_or(false);
        let payload = serde_json::json!({
            "sessionId": session_id,
            "update": update,
        });
        let payload = match (&self.live_delivery, transcript) {
            (Some(live), true) => match live.publish(session_id, payload) {
                PublishOutcome::Send(payload) => Some(payload),
                PublishOutcome::Buffered => None,
                PublishOutcome::Overflow => {
                    tracing::warn!(
                        session_id = %session_id,
                        "history v2 live buffer overflow; dropped oldest update"
                    );
                    None
                }
            },
            _ => Some(payload),
        };
        let Some(payload) = payload else {
            return;
        };
        let _ = self
            .transport
            .send_notification("session/update", payload)
            .await;
    }
}

/// 会改变 transcript 内容的标准 `session/update` 变体。
///
/// 使用协议枚举判定而不是解析后的字符串黑名单：新增的非正文变体默认不进入
/// V2 实时投递序列（命令、配置、用量、计划与会话信息都由各自通道维护）。
fn is_transcript_update(update: &SessionUpdate) -> bool {
    matches!(
        update,
        SessionUpdate::AgentMessageChunk(_)
            | SessionUpdate::AgentThoughtChunk(_)
            | SessionUpdate::UserMessageChunk(_)
            | SessionUpdate::ToolCall(_)
            | SessionUpdate::ToolCallUpdate(_)
    )
}

#[cfg(test)]
#[path = "event_sink_test.rs"]
mod tests;

#[cfg(test)]
#[path = "event_sink/compact_observer_test.rs"]
mod compact_observer_tests;
