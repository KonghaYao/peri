//! Event sink abstraction for ACP session event routing.
//!
//! Different frontends (TUI via MpscTransport, IDE via stdio SDK) route agent
//! execution events differently. [`EventSink`] abstracts this so the core
//! prompt execution logic can live in `peri-acp`.
//!
//! L5：trait 定义已契约化至 `peri-acp-types::event::EventSink`（命令执行体 /
//! 事件发射辅助经契约端口调用），本模块保留 ACP 协议面实现
//! （TransportEventSink / StdioEventSink 等）。

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

use crate::{
    event::map_event, event::AcpEvent, event::CompactFileInfoDto, transport::AcpTransport,
};

/// EventSink 契约（L5：事实源 peri-acp-types::event）。
pub use peri_acp_types::event::EventSink;

/// Serializes a serde `Serialize` value into its serde snake_case string
/// representation. Used for CompactStrategy/CompactOutcome enum variants
/// so TUI string matching works correctly.
fn to_serde_str<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
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
/// - `session/update` — standard ACP SessionUpdate (with `_peri` metadata for TUI)
/// - `peri/agent_event` — AcpEvent DTO 序列化（TUI-only events，categories ②③）
///
/// Additionally, each event is routed through the event router to emit
/// `peri/unstable-event` notifications for new-protocol consumers.
pub struct TransportEventSink {
    transport: std::sync::Arc<dyn AcpTransport>,
    caps_registry: Arc<DashMap<String, PeriCaps>>,
}

impl TransportEventSink {
    pub fn new(
        transport: std::sync::Arc<dyn AcpTransport>,
        caps_registry: Arc<DashMap<String, PeriCaps>>,
    ) -> Self {
        Self {
            transport,
            caps_registry,
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
                    caps.source_agent_id = %caps.source_agent_id,
                    caps.agent_event = %caps.agent_event,
                    mapped.source_agent_id = ?m.source_agent_id,
                    "push_event: caps check for source_agent_id injection"
                );
                // _peri.sourceAgentId 是事件路由语义字段，不应受 caps gating——
                // 否则 SubAgent 内部工具事件无法路由到正确的卡片容器。
                // MappedEvent 已有该字段时，无条件注入。
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
        }

        // 2. peri/agent_event — TUI 专用通知（Category ③）
        // SubagentStarted/SubagentStopped 等事件不产生 SessionUpdate，
        // 但必须通过 peri/agent_event 通道送达 TUI 以创建/销毁 SubAgentGroup 容器。
        if caps.agent_event {
            let acp_event = match event {
                ExecutorEvent::SubagentStarted {
                    agent_name,
                    instance_id,
                    is_background,
                } => Some(AcpEvent::SubagentStarted {
                    agent_name: agent_name.clone(),
                    instance_id: instance_id.clone(),
                    is_background: *is_background,
                }),
                ExecutorEvent::SubagentStopped {
                    agent_name,
                    result,
                    is_error,
                    instance_id,
                } => Some(AcpEvent::SubagentStopped {
                    agent_name: agent_name.clone(),
                    result: result.clone(),
                    is_error: *is_error,
                    instance_id: instance_id.clone(),
                }),
                ExecutorEvent::CompactCompleted {
                    summary,
                    files,
                    skills,
                    micro_cleared,
                    messages,
                    strategy,
                    trigger,
                    outcome,
                    ..
                } => {
                    let messages_json = match serde_json::to_string(messages) {
                        Ok(json) => json,
                        Err(e) => {
                            error!(error = %e, "EventSink: serialize CompactCompleted messages failed");
                            return;
                        }
                    };
                    let strategy_str = to_serde_str(strategy);
                    let trigger_str = to_serde_str(trigger);
                    let outcome_str = to_serde_str(outcome);
                    Some(AcpEvent::CompactCompleted {
                        summary: summary.clone(),
                        files: files
                            .iter()
                            .map(|f| CompactFileInfoDto {
                                path: f.path.clone(),
                                lines: f.lines,
                            })
                            .collect(),
                        skills: skills.clone(),
                        micro_cleared: *micro_cleared,
                        messages_json,
                        strategy: strategy_str,
                        trigger: trigger_str,
                        outcome: outcome_str,
                    })
                }
                ExecutorEvent::AgentExecutionFailed { message } => {
                    Some(AcpEvent::AgentExecutionFailed {
                        message: message.clone(),
                    })
                }
                // Rewind v2：RewindCompleted 经 peri/agent_event 通道送达 TUI，
                // TUI 侧 acp_notifier 转换为 AcpEventData::RewindCompleted 驱动
                // 弹窗关闭 + 消息区重建 + 输入框回填。
                ExecutorEvent::RewindCompleted { summary, messages } => {
                    let messages_json = match serde_json::to_string(messages) {
                        Ok(json) => json,
                        Err(e) => {
                            error!(error = %e, "EventSink: serialize RewindCompleted messages failed");
                            return;
                        }
                    };
                    Some(AcpEvent::RewindCompleted {
                        summary: summary.clone(),
                        messages_json,
                    })
                }
                ExecutorEvent::RewindError { message } => Some(AcpEvent::RewindError {
                    message: message.clone(),
                }),
                // SystemNotification：MCP 上下线等连接状态变化经 peri/agent_event
                // 通道送达 TUI（AcpEventData::SystemNotification → system-notification
                // 通知显示）。
                ExecutorEvent::SystemNotification { text, level } => {
                    Some(AcpEvent::SystemNotification {
                        text: text.clone(),
                        level: level.clone(),
                    })
                }
                // OAuth：MCP 授权流程事件经 host 装配面回调产生（初始化/重连
                // 阶段无 session event_sink），此处分支覆盖运行中经 session 链
                // 转发的场景；初始化阶段由 host 级通道（oauth_event_tx）直达。
                ExecutorEvent::OauthNeeded {
                    server_name,
                    auth_url,
                } => Some(AcpEvent::OauthNeeded {
                    server_name: server_name.clone(),
                    auth_url: auth_url.clone(),
                }),
                ExecutorEvent::OauthCompleted { server_name } => Some(AcpEvent::OauthCompleted {
                    server_name: server_name.clone(),
                }),
                ExecutorEvent::OauthFailed { server_name, error } => Some(AcpEvent::OauthFailed {
                    server_name: server_name.clone(),
                    error: error.clone(),
                }),
                // TurnSuspended：TUI 挂起信号（归档 current_turn + 停止 loading）。
                // v2 StateEvent::TurnSuspended 经 v1 兼容映射（events_v2::
                // state_event_to_executor）到达此处；双轨下线（2026-08-05-3.0-m-
                // event-chain-canonical）后此信号仅经 ACP 路径送达 TUI。
                ExecutorEvent::TurnSuspended { turn_id, agent_id } => {
                    Some(AcpEvent::TurnSuspended {
                        turn_id: turn_id.clone(),
                        agent_id: agent_id.clone(),
                    })
                }
                // StateSnapshotMeta：状态栏上下文消耗（budget_pct + 总量）。
                // v2 StateEvent::StateSnapshot 经 mapper_v2 → v1 StateSnapshotMeta
                // 到达此处；双轨下线（v2_bridge.rs 删除）后此信号仅经 ACP 路径
                // 送达 TUI（acp_notifier.rs 写 CONTEXT_USAGE atom）。此前该分支
                // 缺失落入 `_ => None` 静默丢弃，TUI status_bar ctx% 段永不渲染
                // （e2e compact-command 回归，2026-08-06 修复）。
                ExecutorEvent::StateSnapshotMeta {
                    message_count,
                    total_tokens,
                    current_step,
                    consecutive_failures,
                    budget_pct,
                    context_total_tokens,
                } => Some(AcpEvent::StateSnapshotMeta {
                    message_count: *message_count,
                    total_tokens: *total_tokens,
                    current_step: *current_step,
                    consecutive_failures: *consecutive_failures,
                    budget_pct: *budget_pct,
                    context_total_tokens: *context_total_tokens,
                }),
                // TurnCommitted：messages 载荷（全量消息快照）在本链路无消费者——
                // TUI 仅用 steps 做 ReAct 迭代边界刷新检查点（acp_events/mod.rs:331
                // 丢弃 messages_json），Langfuse bridge 亦不读取（bridge.rs:319）。
                // 序列化该载荷是纯浪费；`{ .. }` 通配字段绑定，兼容 peri-agent 侧
                // messages 改 Arc<Vec<BaseMessage>> 传递，本分支无需再改。
                ExecutorEvent::TurnCommitted { .. } => None,
                _ => None,
            };
            if let Some(acp_event) = acp_event {
                let event_json = match serde_json::to_string(&acp_event) {
                    Ok(json) => json,
                    Err(e) => {
                        error!(error = %e, "EventSink: serialize AcpEvent failed");
                        return;
                    }
                };
                let _ = self
                    .transport
                    .send_notification(
                        "peri/agent_event",
                        json!({
                            "sessionId": session_id,
                            "event_json": event_json,
                        }),
                    )
                    .await;
            }
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
        if caps.agent_event_done {
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
                let notif = SessionNotification::new(self.session_id.clone(), update);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::types::{AcpError, IncomingMessage, RequestId};
    use peri_acp_types::messages::{BaseMessage, MessageContent, MessageId};
    use serde_json::Value;
    use std::sync::Mutex;

    /// Mock transport：记录 send_notification 调用，供断言。
    #[derive(Debug, Default)]
    struct MockTransport {
        notifications: Mutex<Vec<(String, serde_json::Value)>>,
    }

    #[async_trait]
    impl AcpTransport for MockTransport {
        async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
            Ok(Value::Null)
        }
        async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
            self.notifications
                .lock()
                .unwrap()
                .push((method.to_string(), params));
            Ok(())
        }
        async fn recv(&self) -> Option<IncomingMessage> {
            None
        }
        async fn send_response(
            &self,
            _id: RequestId,
            _result: Result<Value, AcpError>,
        ) -> Result<(), AcpError> {
            Ok(())
        }
    }

    fn msg() -> BaseMessage {
        BaseMessage::Human {
            id: MessageId::new(),
            content: MessageContent::Text("hi".to_string()),
        }
    }

    /// 回归测试：RewindCompleted 必须经 peri/agent_event 通道送达 TUI。
    /// 缺失此映射时事件被 `_ => None` 静默丢弃，TUI 弹窗卡在执行中态。
    #[tokio::test]
    async fn push_event_forwards_rewind_completed() {
        let transport = Arc::new(MockTransport::default());
        let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
        caps.insert("s1".to_string(), PeriCaps::all_enabled());
        let sink = TransportEventSink::new(transport.clone(), caps);

        sink.push_event(
            "s1",
            &ExecutorEvent::RewindCompleted {
                summary: "已回滚 2 条消息".to_string(),
                messages: vec![msg()],
            },
            0,
        )
        .await;

        let notifications = transport.notifications.lock().unwrap();
        assert_eq!(
            notifications.len(),
            1,
            "应发出恰好 1 条通知: {:?}",
            notifications
        );
        let (method, params) = &notifications[0];
        assert_eq!(method, "peri/agent_event");

        let event_json = params
            .get("event_json")
            .and_then(|v| v.as_str())
            .expect("event_json 缺失");
        let parsed: serde_json::Value = serde_json::from_str(event_json).unwrap();
        // AcpEvent 是 internally-tagged 枚举：{"type":"rewind_completed","value":{...}}
        let value = parsed.get("value").unwrap();
        assert_eq!(value.get("summary").unwrap(), "已回滚 2 条消息");
        let messages_json = value.get("messages_json").and_then(|v| v.as_str()).unwrap();
        let msgs: Vec<BaseMessage> = serde_json::from_str(messages_json).unwrap();
        assert_eq!(msgs.len(), 1, "messages_json 应可反序列化回 BaseMessage");
    }

    /// 能力未声明（agent_event=false）时事件不发出。
    #[tokio::test]
    async fn push_event_drops_rewind_completed_without_cap() {
        let transport = Arc::new(MockTransport::default());
        let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
        caps.insert("s1".to_string(), PeriCaps::default());
        let sink = TransportEventSink::new(transport.clone(), caps);

        sink.push_event(
            "s1",
            &ExecutorEvent::RewindCompleted {
                summary: "s".to_string(),
                messages: vec![],
            },
            0,
        )
        .await;

        assert!(
            transport.notifications.lock().unwrap().is_empty(),
            "未声明 agent_event cap 时不应发出通知"
        );
    }
}
