use super::AcpEventData;
use super::AcpEventWithEpoch;
use super::AcpNotification;
use super::CancellationToken;
use super::mpsc;
use super::{BridgeSink, forward_notification, spawn_kit_notifier};

use serde_json::json;
#[tokio::test]
async fn test_history_committed_notification_forwards_typed_event() {
    use peri_acp_types::history_window::HistoryCommittedTail;
    use peri_acp_types::messages::MessageId;
    use peri_acp_types::thread::ThreadId;

    let (bridge_tx, mut bridge_rx) = mpsc::channel(crate::kit::acp_bridge::BRIDGE_QUEUE_CAPACITY);
    let tail =
        HistoryCommittedTail::new(&ThreadId::from("s1".to_string()), MessageId::new(), 10).encode();
    let payload = json!({
        "sessionId": "s1",
        "sessionGeneration": "gen-1",
        "structureVersion": 7,
        "checkpointId": "cp-1",
        "summaryEntryId": null,
        "committedTail": tail,
        "hint": { "kind": "open", "anchor": { "kind": "latest" } }
    });
    let owner = forward_notification(
        &BridgeSink::direct(bridge_tx),
        AcpNotification::HistoryCommitted {
            session_id: "s1".into(),
            params: payload,
        },
    )
    .await;
    assert!(owner.is_none());
    let event = bridge_rx.try_recv().expect("commit event");
    assert_eq!(event.active_session_id, "s1");
    assert!(matches!(
        event.event,
        AcpEventData::HistoryCommitted(info)
            if info.checkpoint_id == "cp-1" && info.structure_version == 7
    ));
}

fn spawn_test_notifier() -> (
    mpsc::Sender<AcpNotification>,
    mpsc::Receiver<AcpEventWithEpoch>,
    CancellationToken,
) {
    let (notif_tx, notif_rx) =
        mpsc::channel(crate::acp_client::client::NOTIFICATION_QUEUE_CAPACITY);
    let (bridge_tx, bridge_rx) = mpsc::channel(crate::kit::acp_bridge::BRIDGE_QUEUE_CAPACITY);
    let shutdown = CancellationToken::new();
    let _handle = spawn_kit_notifier(notif_rx, BridgeSink::direct(bridge_tx), shutdown.clone());
    (notif_tx, bridge_rx, shutdown)
}

async fn assert_notifier_reached_barrier(
    notifications: &mpsc::Sender<AcpNotification>,
    events: &mut mpsc::Receiver<AcpEventWithEpoch>,
) {
    notifications
        .send(AcpNotification::AgentDone {
            session_id: "s1".into(),
            stop_reason: "end_turn".into(),
            request_id: None,
        })
        .await
        .expect(
            "assert notifier reached barrier: fixture task and request must complete successfully",
        );
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
        .await
        .expect("session update agent message chunk to text chunk: fixture task and request must complete successfully");

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
            .await
            .expect("interleaved sessions keep stream identity and payload together: fixture task and request must complete successfully");
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
        .await
        .expect("session update agent thought chunk: fixture task and request must complete successfully");

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
        .await
        .expect("session update tool call to tool started: fixture task and request must complete successfully");

    let ev = bridge_rx.recv().await.expect("expected one event");
    match ev.event {
        AcpEventData::ToolStarted(ts) => {
            assert_eq!(ts.tool_id, "tc-1");
            assert_eq!(ts.tool_name, "Read");
            assert_eq!(ts.input_summary, "/tmp/foo.rs");
            assert!(
                ts.origin_message_id.is_none(),
                "旧消息没有 originMessageId 时必须为 None（触发 TUI 启发式回退）"
            );
            assert!(
                ts.origin_readable.is_none(),
                "旧消息没有 originReadable 时必须为 None（语义未知，维持启发式）"
            );
        }
        other => panic!("expected ToolStarted, got {other:?}"),
    }

    shutdown.cancel();
}

/// 服务端经 ACP `_meta.peri.originMessageId` 给出真实所属 assistant 消息：
/// notifier 必须解析并带到事件上（纯工具轮次没有 chunk 可推断归属）。
#[tokio::test]
async fn test_session_update_tool_call_reads_origin_message_id_from_meta() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::SessionUpdate {
            session_id: "s1".into(),
            params: json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tc-pure",
                    "title": "Read",
                    "rawInput": {"file_path": "/tmp/foo.rs"},
                    "_meta": {
                        "peri": {
                            "originMessageId": "m-pure-tools",
                            "originReadable": true
                        }
                    }
                }
            }),
        })
        .await
        .expect("session update tool call reads origin message id from meta: fixture task and request must complete successfully");

    let ev = bridge_rx.recv().await.expect("expected one event");
    match ev.event {
        AcpEventData::ToolStarted(ts) => {
            assert_eq!(ts.tool_id, "tc-pure");
            assert_eq!(
                ts.origin_message_id.as_deref(),
                Some("m-pure-tools"),
                "真实所属 assistant 消息必须从 _meta 透传到 TUI 事件"
            );
            assert_eq!(
                ts.origin_readable,
                Some(true),
                "originReadable=true 表示 origin 已持久化可回读"
            );
        }
        other => panic!("expected ToolStarted, got {other:?}"),
    }

    shutdown.cancel();
}

/// 嵌套执行经 `_meta.peri.originReadable=false` 显式声明没有可回读 origin：
/// notifier 必须保留 `Some(false)`，不得降级成「未知」而触发启发式回退。
#[tokio::test]
async fn test_session_update_tool_call_reads_unreadable_origin_from_meta() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::SessionUpdate {
            session_id: "s1".into(),
            params: json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "tc-nested",
                    "title": "Read",
                    "rawInput": {"file_path": "/tmp/nested.rs"},
                    "_meta": {
                        "peri": { "originReadable": false }
                    }
                }
            }),
        })
        .await
        .expect("session update tool call reads unreadable origin from meta: fixture task and request must complete successfully");

    let ev = bridge_rx.recv().await.expect("expected one event");
    match ev.event {
        AcpEventData::ToolStarted(ts) => {
            assert_eq!(ts.tool_id, "tc-nested");
            assert!(ts.origin_message_id.is_none());
            assert_eq!(
                ts.origin_readable,
                Some(false),
                "嵌套执行必须保留不可回读三态"
            );
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
        .await
        .expect("session update tool call update flattened format: fixture task and request must complete successfully");

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

/// `ToolCallUpdateFields.content` 的标准 image 块进入 `TuiToolEnded.images`；
/// 文本块/未知块被忽略，旧事件（无 content）保持空列表。
#[tokio::test]
async fn test_session_update_tool_call_update_carries_standard_image_content() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::SessionUpdate {
            session_id: "s1".into(),
            params: json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "tc-img",
                    "rawOutput": "Image read\n[image: image/png]",
                    "status": "completed",
                    "content": [
                        {"type": "content", "content": {"type": "text", "text": "Image read"}},
                        {"type": "content", "content": {"type": "image", "data": "AQID", "mimeType": "image/png"}}
                    ]
                }
            }),
        })
        .await
        .expect("image content notification must be delivered");

    let ev = bridge_rx.recv().await.expect("expected one event");
    match ev.event {
        AcpEventData::ToolEnded(te) => {
            assert_eq!(te.tool_id, "tc-img");
            assert_eq!(te.images.len(), 1, "只提取 image 块");
            assert_eq!(te.images[0].mime_type, "image/png");
            assert_eq!(te.images[0].base64, "AQID");
        }
        other => panic!("expected ToolEnded, got {other:?}"),
    }

    shutdown.cancel();
}

/// 服务端展示预算省略的图片数（`_meta.peri.toolImagesOmitted`）进入事件；
/// 旧服务端缺省为 0。
#[tokio::test]
async fn test_session_update_tool_call_update_carries_omitted_image_count() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::SessionUpdate {
            session_id: "s1".into(),
            params: json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "tc-img-omitted",
                    "rawOutput": "Image read\n[image: image/png]",
                    "status": "completed",
                    "content": [
                        {"type": "content", "content": {"type": "text", "text": "Image read"}}
                    ],
                    "_meta": {"peri": {"toolImagesOmitted": 2}}
                }
            }),
        })
        .await
        .expect("omitted image count notification must be delivered");

    let ev = bridge_rx.recv().await.expect("expected one event");
    match ev.event {
        AcpEventData::ToolEnded(te) => {
            assert_eq!(te.tool_id, "tc-img-omitted");
            assert_eq!(te.images_omitted, 2, "省略计数必须透传到事件");
            assert!(te.images.is_empty(), "被省略的图片没有载荷");
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
        .await
        .expect("session update tool call update nested fields fallback: fixture task and request must complete successfully");

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
                    "messageId": "m-replay",
                    "_meta": {"periReplay": true}
                }
            }),
        })
        .await
        .expect("session replay agent message chunk to committed assistant text: fixture task and request must complete successfully");

    let ev = bridge_rx.recv().await.expect("expected one event");
    match ev.event {
        AcpEventData::CommittedAssistantText {
            text,
            reasoning,
            message_id,
        } => {
            assert_eq!(text, "历史回答");
            assert!(
                reasoning.is_none(),
                "agent_message_chunk replay 不应有 reasoning"
            );
            assert_eq!(
                message_id.as_deref(),
                Some("m-replay"),
                "回放必须保留 ACP messageId（reasoning 折叠键）"
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
        .await
        .expect(
            "unstable event unknown dropped: fixture task and request must complete successfully",
        );

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
        .await
        .expect("session replay tool call retains raw input for semantic card: fixture task and request must complete successfully");

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
        .await
        .expect("non terminal tool update is not forwarded as tool end: fixture task and request must complete successfully");

    assert_notifier_reached_barrier(&notif_tx, &mut bridge_rx).await;
    shutdown.cancel();
}

/// REQ-H01-6 / A20：`peri/unstable_event`（回放 reminder）的条目戳摊在 `data` 顶层，
/// 必须先于内容事件注入 `ReplayEntryMarked`——首屏「自有条目只有 reminder」时，
/// 这是区间索引的唯一来源（缺了它更早历史会越过继承前缀）。
#[tokio::test]
async fn test_unstable_event_reminder_entry_marks_precede_content() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();
    notif_tx
        .send(AcpNotification::UnstableEvent {
            session_id: "s1".into(),
            event: "system-reminder".into(),
            data: json!({
                "reminder": {
                    "version": 1,
                    "category": "security",
                    "source": "permission",
                    "kind": "mode_changed",
                    "severity": "warning",
                    "delivery": "required",
                    "audiences": ["tui"],
                    "body": "reminder body",
                    "summary": "权限模式已变更",
                    "metadata": {}
                },
                "replay": true,
                "periLoadGeneration": "gen-1",
                "periEntryId": "s1:own-reminder",
                "periPosition": 42
            }),
        })
        .await
        .expect("unstable event reminder entry marks precede content: fixture task and request must complete successfully");

    let mark = bridge_rx
        .recv()
        .await
        .expect("entry mark must arrive first");
    assert!(
        matches!(
            mark.event,
            AcpEventData::ReplayEntryMarked { ref entry_id, position }
                if entry_id == "s1:own-reminder" && position == 42
        ),
        "unexpected first event: {mark:?}"
    );
    let reminder = bridge_rx
        .recv()
        .await
        .expect("reminder must follow its mark");
    assert!(
        matches!(reminder.event, AcpEventData::SystemReminder { .. }),
        "unexpected second event: {reminder:?}"
    );
    shutdown.cancel();
}
