//! ACP sink wire、capability 与来源投影回归测试。

use super::*;
use crate::event::AcpEvent;
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

fn compact_test_sink() -> (Arc<MockTransport>, TransportEventSink) {
    let transport = Arc::new(MockTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert(
        "s1".to_string(),
        PeriCaps {
            agent_event: true,
            ..PeriCaps::default()
        },
    );
    let sink = TransportEventSink::new(transport.clone(), caps);
    (transport, sink)
}

#[tokio::test]
async fn test_user_input_events_use_queue_capability_without_duplicate_chat_projection() {
    let (transport, sink) = compact_test_sink();
    sink.caps_registry.insert(
        "s1".into(),
        PeriCaps {
            user_input_queue: true,
            agent_activity: true,
            ..PeriCaps::default()
        },
    );
    let snapshot = peri_acp_types::session::UserInputQueueSnapshot {
        session_id: "s1".into(),
        active_request_id: None,
        generation: "generation-1".into(),
        revision: 7,
        items: vec![peri_acp_types::session::UserInputQueueItem {
            input_id: "input-1".into(),
            content: MessageContent::text("用户原文"),
            original_draft: "用户原文".into(),
            state: peri_acp_types::session::UserInputState::Queued,
        }],
    };
    sink.push_event("s1", &ExecutorEvent::UserInputQueueChanged(snapshot), 0)
        .await;
    sink.push_event(
        "s1",
        &ExecutorEvent::UserInputDelivered {
            input_id: "input-1".into(),
            generation: "generation-1".into(),
            content: MessageContent::text("用户原文"),
        },
        0,
    )
    .await;
    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(
        notifications.len(),
        2,
        "每个状态只投影一次，不额外生成标准聊天块或 activity"
    );
    assert!(
        notifications
            .iter()
            .all(|(method, _)| method == "peri/agent_event"),
        "专属能力可独立于 legacy agent_event 能力使用"
    );
    let changed: AcpEvent =
        serde_json::from_str(notifications[0].1["event_json"].as_str().unwrap()).unwrap();
    let delivered: AcpEvent =
        serde_json::from_str(notifications[1].1["event_json"].as_str().unwrap()).unwrap();
    assert!(
        matches!(changed, AcpEvent::UserInputQueueChanged { snapshot } if snapshot.revision == 7 && snapshot.generation == "generation-1"),
        "快照 wire 保留顺序与实例身份"
    );
    assert!(
        matches!(delivered, AcpEvent::UserInputDelivered { input_id, generation, content } if input_id == "input-1" && generation == "generation-1" && content.text_content() == "用户原文"),
        "接收事件保留稳定身份及完整内容"
    );
}

#[tokio::test]
async fn test_user_input_events_fail_closed_without_queue_capability() {
    let (transport, sink) = compact_test_sink();
    let event = ExecutorEvent::UserInputDelivered {
        input_id: "input-1".into(),
        generation: "generation-1".into(),
        content: MessageContent::text("不应泄漏的输入"),
    };
    sink.push_event("s1", &event, 0).await;
    sink.push_event("unregistered", &event, 0).await;
    assert!(
        transport.notifications.lock().unwrap().is_empty(),
        "legacy 能力或缺失 session caps 均不得隐式开启队列正文事件"
    );
}

#[tokio::test]
async fn test_user_input_queue_capability_includes_paired_done() {
    let (transport, sink) = compact_test_sink();
    sink.caps_registry.insert(
        "s1".into(),
        PeriCaps {
            user_input_queue: true,
            ..PeriCaps::default()
        },
    );
    sink.push_user_input_started("s1", "generation-1".into(), "ticket-1".into())
        .await
        .unwrap();
    sink.push_done("s1", "end_turn", Some("ticket-1")).await;
    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(
        notifications.len(),
        2,
        "仅声明队列能力也必须收到启动与结束配对"
    );
    assert_eq!(notifications[1].0, "peri/agent_event_done");
    assert_eq!(
        notifications[1].1["requestId"], "ticket-1",
        "终态必须使用同一执行身份"
    );
}

#[tokio::test]
async fn push_event_forwards_compact_started() {
    let (transport, sink) = compact_test_sink();
    sink.push_event(
        "s1",
        &ExecutorEvent::CompactStarted {
            turn_id: "turn-1".into(),
            agent_id: "agent-1".into(),
            step: 3,
            strategy: peri_acp_types::event::CompactStrategy::Micro,
            trigger: peri_acp_types::event::CompactTrigger::Auto,
        },
        0,
    )
    .await;

    let notifications = transport.notifications.lock().unwrap();
    let event_json = notifications[0].1["event_json"].as_str().unwrap();
    let event: AcpEvent = serde_json::from_str(event_json).unwrap();
    assert!(matches!(event, AcpEvent::CompactStarted));
}

#[tokio::test]
async fn push_event_forwards_compact_completed_details() {
    let (transport, sink) = compact_test_sink();
    sink.push_event(
        "s1",
        &ExecutorEvent::CompactCompleted {
            summary: String::new(),
            messages: vec![],
            trigger: peri_acp_types::event::CompactTrigger::Auto,
            strategy: peri_acp_types::event::CompactStrategy::Micro,
            affected_count: 7,
            estimated_tokens_saved: 2048,
            files: vec![],
            skills: vec!["tdd".into()],
        },
        0,
    )
    .await;

    let notifications = transport.notifications.lock().unwrap();
    let event_json = notifications[0].1["event_json"].as_str().unwrap();
    let event: AcpEvent = serde_json::from_str(event_json).unwrap();
    assert!(matches!(
        event,
        AcpEvent::CompactCompleted {
            strategy,
            affected_count: 7,
            estimated_tokens_saved: 2048,
            skills,
            ..
        } if strategy == "micro" && skills == ["tdd"]
    ));
}

#[tokio::test]
async fn push_event_forwards_goal_snapshot_details() {
    let transport = Arc::new(MockTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert(
        "s1".to_string(),
        PeriCaps {
            agent_event: true,
            ..PeriCaps::default()
        },
    );
    let sink = TransportEventSink::new(transport.clone(), caps);
    sink.push_event(
        "s1",
        &ExecutorEvent::GoalSnapshot {
            objective: Some("ship goal panel".into()),
            status: Some(peri_acp_types::goal::GoalStatus::Active),
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: 0,
            continuation_count: 3,
            blocked_reason: None,
        },
        0,
    )
    .await;

    let notifications = transport.notifications.lock().unwrap();
    let event_json = notifications[0].1["event_json"].as_str().unwrap();
    let event: AcpEvent = serde_json::from_str(event_json).unwrap();
    assert!(matches!(
        event,
        AcpEvent::GoalSnapshot {
            objective,
            status: Some(peri_acp_types::goal::GoalStatus::Active),
            continuation_count: 3,
            ..
        } if objective.as_deref() == Some("ship goal panel")
    ));
}

/// 回归测试：RewindCompleted 必须经 peri/agent_event 通道送达 TUI。
/// 缺失此映射时事件被 `_ => None` 静默丢弃，TUI 弹窗卡在执行中态。
#[tokio::test]
async fn push_event_forwards_rewind_completed() {
    let transport = Arc::new(MockTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert(
        "s1".to_string(),
        PeriCaps {
            agent_event: true,
            ..PeriCaps::default()
        },
    );
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

/// LLM retry 必须经 peri/agent_event 通道送达 TUI，否则客户端无法展示
/// attempt/max_attempts/delay，用户会误以为首次失败即终止。
#[tokio::test]
async fn push_event_forwards_llm_retrying() {
    let transport = Arc::new(MockTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert(
        "s1".to_string(),
        PeriCaps {
            agent_event: true,
            ..PeriCaps::default()
        },
    );
    let sink = TransportEventSink::new(transport.clone(), caps);

    sink.push_event(
        "s1",
        &ExecutorEvent::LlmRetrying {
            attempt: 1,
            max_attempts: 6,
            delay_ms: 500,
            error: "transport".into(),
        },
        0,
    )
    .await;

    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(notifications.len(), 1);
    let (method, params) = &notifications[0];
    assert_eq!(method, "peri/agent_event");
    let event_json = params
        .get("event_json")
        .and_then(|value| value.as_str())
        .expect("event_json 缺失");
    let parsed: serde_json::Value = serde_json::from_str(event_json).unwrap();
    assert_eq!(
        parsed,
        serde_json::json!({
            "type": "llm_retrying",
            "value": {
                "attempt": 1,
                "max_attempts": 6,
                "delay_ms": 500,
                "error": "transport",
            }
        })
    );
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

/// CommandFeedback 分支 wire 断言：level/channel 经 to_serde_str 透传 Phase 1
/// camelCase 输出（"info" / "uiOnly"），message 原文透传（核对点 7）。
#[tokio::test]
async fn push_event_forwards_command_feedback_wire() {
    let transport = Arc::new(MockTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert(
        "s1".to_string(),
        PeriCaps {
            agent_event: true,
            ..PeriCaps::default()
        },
    );
    let sink = TransportEventSink::new(transport.clone(), caps);

    sink.push_event(
        "s1",
        &ExecutorEvent::CommandFeedback(peri_acp_types::command::CommandFeedback {
            level: peri_acp_types::command::FeedbackLevel::Info,
            message: "命令完成".to_string(),
            channel: peri_acp_types::command::FeedbackChannel::UiOnly,
        }),
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
    assert_eq!(
        parsed,
        serde_json::json!({
            "type": "command_feedback",
            "value": {
                "level": "info",
                "message": "命令完成",
                "channel": "uiOnly",
            }
        }),
        "CommandFeedback wire 形态必须与 Phase 1 serde camelCase 输出一致"
    );
}

#[tokio::test]
async fn push_event_emits_only_safe_activity_when_cap_is_declared() {
    let transport = Arc::new(MockTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert(
        "s1".to_string(),
        PeriCaps {
            agent_activity: true,
            ..PeriCaps::default()
        },
    );
    let sink = TransportEventSink::new(transport.clone(), caps);

    sink.push_event(
        "s1",
        &ExecutorEvent::SubagentStopped {
            agent_name: "reviewer".into(),
            result: "SECRET_RESULT_SENTINEL".into(),
            is_error: false,
            instance_id: "raw-instance-id".into(),
        },
        0,
    )
    .await;

    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].0, "peri/agent_activity");
    assert_eq!(notifications[0].1["activity"]["schemaVersion"], 1);
    assert_eq!(notifications[0].1["activity"]["kind"], "subagent");
    let serialized = notifications[0].1.to_string();
    assert!(!serialized.contains("SECRET_RESULT_SENTINEL"));
    assert!(!serialized.contains("raw-instance-id"));
}

#[tokio::test]
async fn test_subagent_completion_keeps_activity_safe_before_legacy_output() {
    let transport = Arc::new(MockTransport::default());
    let caps = Arc::new(DashMap::new());
    caps.insert(
        "s1".to_string(),
        PeriCaps {
            agent_activity: true,
            agent_event: true,
            ..PeriCaps::default()
        },
    );
    let sink = TransportEventSink::new(transport.clone(), caps);
    sink.push_event(
        "s1",
        &ExecutorEvent::SubagentStopped {
            agent_name: "explorer".into(),
            result: "PRIVATE_RESULT_SENTINEL".into(),
            is_error: false,
            instance_id: "private-instance-id".into(),
        },
        0,
    )
    .await;

    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(notifications.len(), 2, "两个已协商的事件面均应收到通知");
    assert_eq!(
        notifications[0].0, "peri/agent_activity",
        "安全摘要应先发送"
    );
    assert_eq!(notifications[1].0, "peri/agent_event", "兼容事件应后发送");
    let activity = notifications[0].1.to_string();
    assert!(
        !activity.contains("PRIVATE_RESULT_SENTINEL"),
        "摘要不得携带结果正文"
    );
    assert!(
        !activity.contains("private-instance-id"),
        "摘要不得携带原始实例标识"
    );
    let event: AcpEvent =
        serde_json::from_str(notifications[1].1["event_json"].as_str().unwrap()).unwrap();
    assert!(
        matches!(event, AcpEvent::SubagentStopped { result, instance_id, .. }
            if result == "PRIVATE_RESULT_SENTINEL" && instance_id == "private-instance-id"),
        "兼容事件应保留客户端消费的结果和实例标识"
    );
}

#[tokio::test]
async fn push_event_does_not_emit_activity_without_exact_cap() {
    let transport = Arc::new(MockTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert(
        "s1".to_string(),
        PeriCaps {
            agent_event: true,
            agent_activity: false,
            ..PeriCaps::default()
        },
    );
    let sink = TransportEventSink::new(transport.clone(), caps);
    sink.push_event(
        "s1",
        &ExecutorEvent::ContextWarning {
            used_tokens: 90,
            total_tokens: 100,
            percentage: 0.9,
        },
        0,
    )
    .await;
    assert!(transport.notifications.lock().unwrap().is_empty());
}

#[test]
fn session_notification_preserves_source_identity_through_typed_roundtrip() {
    let update = SessionUpdate::ToolCall(agent_client_protocol::schema::v1::ToolCall::new(
        "call-1", "Read",
    ));
    let notification =
        session_notification(SdkSessionId::from("s1"), update, Some("child-agent-1"));

    let wire = serde_json::to_value(notification).unwrap();
    let decoded: SessionNotification = serde_json::from_value(wire).unwrap();
    assert_eq!(
        decoded
            .meta
            .as_ref()
            .and_then(|meta| meta.get("peri"))
            .and_then(|peri| peri.get("sourceAgentId"))
            .and_then(Value::as_str),
        Some("child-agent-1")
    );
}

#[tokio::test]
async fn push_event_places_source_identity_in_acp_meta() {
    let transport = Arc::new(MockTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert("s1".to_string(), PeriCaps::default());
    let sink = TransportEventSink::new(transport.clone(), caps);

    sink.push_event(
        "s1",
        &ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "child output".to_string(),
            source_agent_id: Some("child-agent-1".to_string()),
        },
        0,
    )
    .await;

    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(
        notifications[0].1["_meta"]["peri"]["sourceAgentId"],
        "child-agent-1"
    );
    assert!(notifications[0].1.get("_peri").is_none());
}

#[tokio::test]
async fn auxiliary_usage_places_source_identity_in_top_level_acp_meta() {
    let transport = Arc::new(MockTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert(
        "s1".to_string(),
        PeriCaps {
            token_stats: true,
            ..PeriCaps::default()
        },
    );
    let sink = TransportEventSink::new(transport.clone(), caps);
    sink.push_event(
        "s1",
        &ExecutorEvent::LlmCallEnd {
            step: 0,
            model: "aux-model".into(),
            output: String::new(),
            usage: Some(peri_acp_types::model::TokenUsage::new(100, 1)),
            stop_reason: None,
            request_id: Some("aux-request".into()),
            source_agent_id: Some("workflow-agent".into()),
        },
        200_000,
    )
    .await;

    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(notifications[0].0, "session/update");
    assert_eq!(
        notifications[0].1["_meta"]["peri"]["sourceAgentId"],
        "workflow-agent"
    );
    assert_eq!(
        notifications[0].1["update"]["sessionUpdate"],
        "usage_update"
    );
}

/// push_unstable_event 通道 method 命名统一为 snake_case（2026-08-14 整顿）。
#[tokio::test]
async fn push_unstable_event_uses_snake_case_method() {
    let transport = Arc::new(MockTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert("s1".to_string(), PeriCaps::all_enabled());
    let sink = TransportEventSink::new(transport.clone(), caps);

    let _ = sink
        .push_unstable_event(
            "s1",
            "plugin-action-result".into(),
            serde_json::json!({"ok": true}),
        )
        .await;

    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(notifications.len(), 1, "应发出恰好 1 条通知");
    let (method, _params) = &notifications[0];
    assert_eq!(method, "peri/unstable_event");
}
