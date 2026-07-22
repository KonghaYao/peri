//! Tests for acp_notifier

use super::*;
use peri_acp::event::AcpEvent;
use serde_json::json;
use serial_test::serial;

fn spawn_test_notifier() -> (
    mpsc::UnboundedSender<AcpNotification>,
    mpsc::UnboundedReceiver<AcpEventWithEpoch>,
    CancellationToken,
) {
    let (notif_tx, notif_rx) = mpsc::unbounded_channel::<AcpNotification>();
    let (bridge_tx, bridge_rx) = mpsc::unbounded_channel::<AcpEventWithEpoch>();
    let shutdown = CancellationToken::new();
    let _handle = spawn_kit_notifier(notif_rx, bridge_tx, shutdown.clone());
    (notif_tx, bridge_rx, shutdown)
}

#[tokio::test]
async fn test_session_update_agent_message_chunk_to_text_chunk() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::SessionUpdate {
            session_id: "s1".into(),
            params: json!({
                "sessionId": "s1",
                "_peri": {"sourceAgentId": "sa-1"},
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "hi"}
                }
            }),
        })
        .unwrap();

    let ev = bridge_rx.recv().await.expect("expected one event");
    match ev.event {
        AcpEventData::TextChunk(tc) => {
            assert_eq!(tc.text, "hi");
            assert_eq!(tc.agent_id.as_deref(), Some("sa-1"));
        }
        other => panic!("expected TextChunk, got {other:?}"),
    }

    shutdown.cancel();
}

#[tokio::test]
async fn test_session_update_agent_thought_chunk() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::SessionUpdate {
            session_id: "s1".into(),
            params: json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": {"type": "text", "text": "thinking..."}
                }
            }),
        })
        .unwrap();

    let ev = bridge_rx.recv().await.expect("expected one event");
    match ev.event {
        AcpEventData::ReasoningChunk(rc) => {
            assert_eq!(rc.text, "thinking...");
            assert!(rc.agent_id.is_none());
        }
        other => panic!("expected ReasoningChunk, got {other:?}"),
    }

    shutdown.cancel();
}

#[tokio::test]
async fn test_session_update_tool_call_to_tool_started() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::SessionUpdate {
            session_id: "s1".into(),
            params: json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tc-1",
                    "title": "Read",
                    "rawInput": {"file_path": "/tmp/foo.rs"}
                }
            }),
        })
        .unwrap();

    let ev = bridge_rx.recv().await.expect("expected one event");
    match ev.event {
        AcpEventData::ToolStarted(ts) => {
            assert_eq!(ts.tool_id, "tc-1");
            assert_eq!(ts.tool_name, "Read");
            assert_eq!(ts.input_summary, "/tmp/foo.rs");
        }
        other => panic!("expected ToolStarted, got {other:?}"),
    }

    shutdown.cancel();
}

/// 验证 tool_call_update 的顶层 flatten 格式（ACP SDK 实际序列化格式）：
/// rawOutput/status 被 #[serde(flatten)] 合并到 update 顶层。
#[tokio::test]
async fn test_session_update_tool_call_update_flattened_format() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::SessionUpdate {
            session_id: "s1".into(),
            params: json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "tc-1",
                    "rawOutput": "output content",
                    "status": "failed"
                }
            }),
        })
        .unwrap();

    let ev = bridge_rx.recv().await.expect("expected one event");
    match ev.event {
        AcpEventData::ToolEnded(te) => {
            assert_eq!(te.tool_id, "tc-1");
            assert!(te.output_summary.contains("output content"));
            assert!(te.is_error);
        }
        other => panic!("expected ToolEnded, got {other:?}"),
    }

    shutdown.cancel();
}

/// 验证 tool_call_update 的 fields 嵌套格式（fallback 兼容路径）：
/// rawOutput/status 在 fields 子对象内。
#[tokio::test]
async fn test_session_update_tool_call_update_nested_fields_fallback() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::SessionUpdate {
            session_id: "s1".into(),
            params: json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "tc-2",
                    "fields": {
                        "rawOutput": "nested output",
                        "status": "failed"
                    }
                }
            }),
        })
        .unwrap();

    let ev = bridge_rx.recv().await.expect("expected one event");
    match ev.event {
        AcpEventData::ToolEnded(te) => {
            assert_eq!(te.tool_id, "tc-2");
            assert!(te.output_summary.contains("nested output"));
            assert!(te.is_error);
        }
        other => panic!("expected ToolEnded from nested fields, got {other:?}"),
    }

    shutdown.cancel();
}

#[tokio::test]
async fn test_session_replay_agent_message_chunk_to_committed_assistant_text() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::SessionUpdate {
            session_id: "s1".into(),
            params: json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "历史回答"},
                    "_meta": {"periReplay": true}
                }
            }),
        })
        .unwrap();

    let ev = bridge_rx.recv().await.expect("expected one event");
    match ev.event {
        AcpEventData::CommittedAssistantText { text, reasoning } => {
            assert_eq!(text, "历史回答");
            assert!(
                reasoning.is_none(),
                "agent_message_chunk replay 不应有 reasoning"
            );
        }
        other => panic!("expected CommittedAssistantText, got {other:?}"),
    }

    shutdown.cancel();
}

#[tokio::test]
async fn test_unstable_event_unknown_dropped() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::UnstableEvent {
            session_id: "s1".into(),
            event: "future-event".into(),
            data: json!({"x": 1}),
        })
        .unwrap();

    // Unknown 事件被丢弃——bridge_rx 在短时间内应无数据
    let result = tokio::time::timeout(std::time::Duration::from_millis(50), bridge_rx.recv()).await;
    assert!(
        matches!(result, Ok(None)) || result.is_err(),
        "expected no event (channel idle or timeout), got {result:?}"
    );

    shutdown.cancel();
}

/// 验证 AgentEvent 的 SubagentStarted 变体被正确转换并转发到 bridge。
/// SubagentStopped 同理（此处仅覆盖 SubagentStarted 作为 smoke test）。
#[tokio::test]
async fn test_agent_event_forwards_subagent_started() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::AgentEvent {
            session_id: "s1".into(),
            event: AcpEvent::SubagentStarted {
                agent_name: "explore".into(),
                instance_id: "abc-123".into(),
                is_background: false,
            },
        })
        .unwrap();

    let bridge_event = bridge_rx
        .recv()
        .await
        .expect("bridge 应收 到 SubagentStarted");
    match bridge_event.event {
        AcpEventData::SubagentStarted {
            agent_id,
            agent_name,
            ..
        } => {
            assert_eq!(agent_id, "abc-123", "agent_id 应从 instance_id 映射");
            assert_eq!(agent_name, "explore");
        }
        other => panic!("expected SubagentStarted, got {other:?}"),
    }

    shutdown.cancel();
}

/// 验证未映射的 AcpEvent 变体被静默丢弃（防御性测试）。
#[tokio::test]
async fn test_agent_event_unknown_variant_dropped() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::AgentEvent {
            session_id: "s1".into(),
            event: AcpEvent::StateSnapshotMeta {
                message_count: 0,
                total_tokens: 0,
                current_step: 0,
                consecutive_failures: 0,
                budget_pct: None,
                context_total_tokens: None,
            },
        })
        .unwrap();

    // StateSnapshotMeta 只写 CONTEXT_USAGE atom（供 StatusBar），不转发 bridge 事件
    let result = tokio::time::timeout(std::time::Duration::from_millis(50), bridge_rx.recv()).await;
    assert!(
        matches!(result, Ok(None)) || result.is_err(),
        "expected unmapped AgentEvent to be dropped, got {result:?}"
    );

    shutdown.cancel();
}

#[tokio::test]
async fn test_agent_done_forwards_turn_done_to_bridges() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::AgentDone {
            session_id: "s1".into(),
            stop_reason: "end_turn".into(),
        })
        .unwrap();

    let bridge_event = bridge_rx.recv().await.expect("bridge 应收到 TurnDone");
    assert!(matches!(bridge_event.event, AcpEventData::TurnDone));

    shutdown.cancel();
}

#[tokio::test]
async fn test_channel_close_exits_cleanly() {
    let (notif_tx, _bridge_rx, shutdown) = spawn_test_notifier();

    // 模拟 transport 断开：drop sender 让 recv() 返回 None
    drop(notif_tx);

    // 给任务一点时间退出
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // shutdown 仍可正常调用（任务已退出，cancel 信号无害）
    shutdown.cancel();
}

/// 验证 handle_session_update 能正确解析 ACP SessionUpdate 的 JSON 格式。
/// SessionUpdate 使用 #[serde(tag = "sessionUpdate")] 内部标签，字段名 camelCase。
#[test]
#[serial]
fn test_handle_session_update_parses_available_commands() {
    crate::kit::atoms::init_atoms();
    let payload = json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "available_commands_update",
            "availableCommands": [
                {"name": "help", "description": "Show help"},
                {"name": "clear", "description": "Clear conversation"},
                {"name": "archify", "description": "Create architecture diagrams"}
            ]
        }
    });
    let (dummy_tx, _dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = handle_session_update(payload, &dummy_tx, "test");
    let entries = AVAILABLE_SLASH_COMMANDS.state().read().clone();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], ("help".to_string(), "Show help".to_string()));
    assert_eq!(
        entries[2],
        (
            "archify".to_string(),
            "Create architecture diagrams".to_string()
        )
    );
}

/// 验证非 available_commands_update 的 session/update 不会错误写入 atom。
#[test]
#[serial]
fn test_handle_session_update_skips_non_command_update() {
    crate::kit::atoms::init_atoms();
    // 重置 atom 状态，避免跨测试污染
    *AVAILABLE_SLASH_COMMANDS.state().write() = Vec::new();
    let payload = json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "usage_update",
            "used": 1000,
            "total": 200000
        }
    });
    let (dummy_tx, _dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = handle_session_update(payload, &dummy_tx, "test");
    let entries = AVAILABLE_SLASH_COMMANDS.state().read().clone();
    assert_eq!(entries.len(), 0, "非 commands update 不应写入 atom");
}

/// 验证 handle_session_update 能正确解析 plan update 并写入 TODO_ITEMS atom。
#[test]
#[serial]
fn test_handle_session_update_parses_plan() {
    use crate::kit::message_area::TodoStatus;
    crate::kit::atoms::init_atoms();
    *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();

    let payload = json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "plan",
            "entries": [
                {"content": "Fix bug", "status": "in_progress", "priority": "medium"},
                {"content": "Write tests", "status": "pending", "priority": "medium"},
                {"content": "Document", "status": "completed", "priority": "medium"}
            ]
        }
    });

    let (dummy_tx, _dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = handle_session_update(payload, &dummy_tx, "test");

    let items = crate::kit::atoms::TODO_ITEMS.state().read().clone();
    assert_eq!(items.len(), 3, "应包含 3 个条目，实际: {items:?}");
    assert_eq!(items[0].content, "Fix bug");
    assert_eq!(items[1].content, "Write tests");
    assert_eq!(items[2].content, "Document");
    assert!(matches!(items[0].status, TodoStatus::InProgress));
    assert!(matches!(items[1].status, TodoStatus::Pending));
    assert!(matches!(items[2].status, TodoStatus::Completed));
}

/// M4: PredictionReady 不再被丢弃，转换为 AcpEventData::Prediction 推入 bridge channel。
#[tokio::test]
#[serial]
async fn test_prediction_ready_forwards_prediction_event() {
    crate::kit::atoms::init_atoms();
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::PredictionReady {
            session_id: "s1".into(),
            text: "next word".into(),
        })
        .unwrap();

    let bridge_event = bridge_rx.recv().await.expect("bridge 应收到 Prediction");
    match bridge_event.event {
        AcpEventData::Prediction(p) => assert_eq!(p.text, "next word"),
        other => panic!("expected Prediction, got {other:?}"),
    }

    shutdown.cancel();
}

/// H2: RequestPermission 转换为 HitlPending 事件并写入 HITL_REQUEST_ID atom。
#[tokio::test]
#[serial]
async fn test_request_permission_forwards_hitl_pending_event() {
    crate::kit::atoms::init_atoms();
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    let request_id = peri_acp::transport::types::RequestId::String("req-123".to_string());
    notif_tx
        .send(AcpNotification::RequestPermission {
            id: request_id,
            params: json!({
                "sessionId": "s1",
                "toolCall": {
                    "title": "Bash",
                    "rawInput": {"command": "rm -rf /"}
                },
                "options": []
            }),
        })
        .unwrap();

    let bridge_event = bridge_rx.recv().await.expect("bridge 应收到 HitlPending");
    match bridge_event.event {
        AcpEventData::HitlPending(hp) => {
            assert_eq!(hp.tool_name, "Bash");
            assert_eq!(hp.tool_input["command"], "rm -rf /");
        }
        other => panic!("expected HitlPending, got {other:?}"),
    }

    // HITL_REQUEST_ID 应被写入
    let id_str = HITL_REQUEST_ID.state().read().clone();
    assert!(id_str.is_some(), "HITL_REQUEST_ID 应被写入");
    assert!(
        id_str.unwrap().contains("req-123"),
        "HITL_REQUEST_ID 应包含原始 id"
    );

    shutdown.cancel();
}
