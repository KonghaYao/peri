use super::*;
use serde_json::json;
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

async fn assert_notifier_reached_barrier(
    notifications: &mpsc::UnboundedSender<AcpNotification>,
    events: &mut mpsc::UnboundedReceiver<AcpEventWithEpoch>,
) {
    notifications
        .send(AcpNotification::AgentDone {
            session_id: "s1".into(),
            stop_reason: "end_turn".into(),
            request_id: None,
        })
        .unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .expect("notifier must process the barrier")
        .expect("notifier must not close the bridge before the barrier");
    assert!(
        matches!(event.event, AcpEventData::TurnDone),
        "unexpected event before barrier: {event:?}"
    );
}

#[tokio::test]
async fn test_session_update_agent_message_chunk_to_text_chunk() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::SessionUpdate {
            session_id: "s1".into(),
            params: json!({
                "sessionId": "s1",
                "_meta": {"peri": {"sourceAgentId": "sa-1"}},
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
async fn interleaved_sessions_keep_stream_identity_and_payload_together() {
    let (notifications, mut events, shutdown) = spawn_test_notifier();
    let inputs = [
        ("session-a", "child-a", "第一条"),
        ("session-b", "child-b", "第二条"),
    ];
    for (session, child, text) in inputs {
        notifications
            .send(AcpNotification::SessionUpdate {
                session_id: session.into(),
                params: json!({
                    "sessionId": session,
                    "_meta": {"peri": {"sourceAgentId": child}},
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": text}
                    }
                }),
            })
            .unwrap();
    }
    for (session, child, text) in inputs {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
            .await
            .expect("notifier must process each session")
            .expect("bridge must remain open");
        assert_eq!(event.active_session_id, session);
        let AcpEventData::TextChunk(chunk) = event.event else {
            panic!("expected text chunk");
        };
        assert_eq!(chunk.agent_id.as_deref(), Some(child));
        assert_eq!(chunk.text, text);
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

    assert_notifier_reached_barrier(&notif_tx, &mut bridge_rx).await;

    shutdown.cancel();
}

#[tokio::test]
async fn test_session_replay_tool_call_retains_raw_input_for_semantic_card() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();
    let raw_input = json!({"skill": "using-superpowers"});
    notif_tx
        .send(AcpNotification::SessionUpdate {
            session_id: "s1".into(),
            params: json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "skill-1",
                    "title": "Skill",
                    "rawInput": raw_input,
                    "_meta": {"periReplay": true}
                }
            }),
        })
        .unwrap();

    let event = bridge_rx.recv().await.expect("expected replay event");
    match event.event {
        AcpEventData::ReplayToolStarted { raw_input, .. } => {
            assert_eq!(raw_input, json!({"skill": "using-superpowers"}));
        }
        other => panic!("expected ReplayToolStarted, got {other:?}"),
    }
    shutdown.cancel();
}

#[tokio::test]
async fn test_non_terminal_tool_update_is_not_forwarded_as_tool_end() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();
    notif_tx
        .send(AcpNotification::SessionUpdate {
            session_id: "s1".into(),
            params: json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "todo-1",
                    "status": "in_progress"
                }
            }),
        })
        .unwrap();

    assert_notifier_reached_barrier(&notif_tx, &mut bridge_rx).await;
    shutdown.cancel();
}
