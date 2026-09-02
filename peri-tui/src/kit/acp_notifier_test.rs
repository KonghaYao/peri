//! Tests for acp_notifier

use super::*;
use crate::acp_client::AcpTuiClient;
use crate::kit::acp_types::CacheUsageSample;
use crate::kit::slash_completion::SlashActionKind;
use crate::kit::slash_projection::ArgKind;
use peri_acp::event::AcpEvent;
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair};
use peri_acp_types::event_data::PredictionAction;
use serde_json::json;
use serial_test::serial;

#[test]
#[serial]
fn available_commands_update_refreshes_mcp_slash_cache_immediately() {
    crate::kit::atoms::init_atoms();
    *AVAILABLE_SLASH_COMMANDS.state().write() = Vec::new();
    crate::kit::input_area::refresh_slash_items();

    let payload = json!({
        "sessionId": "s-mcp",
        "update": {
            "sessionUpdate": "available_commands_update",
            "availableCommands": [{
                "name": "demo:hello",
                "description": "MCP skill hello",
                "_meta": {"periKind": "mcp_skill", "periLevel": 2}
            }]
        }
    });
    let (bridge_tx, _bridge_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = handle_session_update(payload, &bridge_tx, "test");

    let items = crate::kit::input_area::get_cached_slash_items();
    assert!(
        items.iter().any(|item| item.insert_text == "demo:hello"),
        "MCP skill 应在 available_commands_update 后立即出现在 slash 缓存"
    );
}

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
async fn bridge_delivery_failure_claims_and_settles_registered_interaction() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, notification_rx) = AcpTuiClient::new(client_transport);
    client.force_stable_for_test("s1", true);
    client.spawn_pump(notification_tx);

    let (bridge_tx, bridge_rx) = mpsc::unbounded_channel();
    drop(bridge_rx);
    let shutdown = CancellationToken::new();
    let _notifier =
        spawn_kit_notifier_with_client(notification_rx, bridge_tx, shutdown.clone(), client);

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        server_transport.send_request(
            "session/request_permission",
            json!({"sessionId":"s1","toolCall":{"title":"Bash","rawInput":{}}}),
        ),
    )
    .await
    .expect("bridge rejection must not leave reverse request hanging")
    .expect("bridge rejection must settle the reverse request");
    let response: agent_client_protocol::schema::v1::RequestPermissionResponse =
        serde_json::from_value(response).unwrap();
    assert!(matches!(
        response.outcome,
        agent_client_protocol::schema::v1::RequestPermissionOutcome::Cancelled
    ));
    shutdown.cancel();
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

/// LlmRetrying 必须透传安全的重试进度，供 bridge 展示给用户。
#[tokio::test]
async fn test_agent_event_forwards_llm_retrying() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::AgentEvent {
            session_id: "s1".into(),
            event: AcpEvent::LlmRetrying {
                attempt: 1,
                max_attempts: 6,
                delay_ms: 500,
                error: "transport".into(),
            },
        })
        .unwrap();

    let bridge_event = bridge_rx.recv().await.expect("bridge 应收到 LlmRetrying");
    match bridge_event.event {
        AcpEventData::LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error,
        } => {
            assert_eq!(attempt, 1);
            assert_eq!(max_attempts, 6);
            assert_eq!(delay_ms, 500);
            assert_eq!(error, "transport");
        }
        other => panic!("expected LlmRetrying, got {other:?}"),
    }

    shutdown.cancel();
}

/// SubagentStopped 必须全量透传 instance_id/result/is_error——parent 终态
/// 唯一事实源在 TUI 边界不得丢弃（bug 回归：此前 `..` 吞掉 result/is_error，
/// TUI 只能从 child tool error 反推 block error）。
#[tokio::test]
async fn test_agent_event_forwards_subagent_stopped() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    // genuine parent error：is_error=true + result
    notif_tx
        .send(AcpNotification::AgentEvent {
            session_id: "s1".into(),
            event: AcpEvent::SubagentStopped {
                agent_name: "explore".into(),
                instance_id: "abc-123".into(),
                result: "loop failed: llm error".into(),
                is_error: true,
            },
        })
        .unwrap();
    let bridge_event = bridge_rx
        .recv()
        .await
        .expect("bridge 应收到 SubagentStopped");
    match bridge_event.event {
        AcpEventData::SubagentStopped {
            agent_id,
            result,
            is_error,
        } => {
            assert_eq!(agent_id, "abc-123", "agent_id 应从 instance_id 映射");
            assert_eq!(result, "loop failed: llm error", "result 应透传");
            assert!(is_error, "is_error=true 应透传");
        }
        other => panic!("expected SubagentStopped, got {other:?}"),
    }

    // completed parent：is_error=false
    notif_tx
        .send(AcpNotification::AgentEvent {
            session_id: "s1".into(),
            event: AcpEvent::SubagentStopped {
                agent_name: "explore".into(),
                instance_id: "abc-124".into(),
                result: "done".into(),
                is_error: false,
            },
        })
        .unwrap();
    let bridge_event = bridge_rx
        .recv()
        .await
        .expect("bridge 应收 到第二个 SubagentStopped");
    match bridge_event.event {
        AcpEventData::SubagentStopped {
            agent_id,
            result,
            is_error,
        } => {
            assert_eq!(agent_id, "abc-124");
            assert_eq!(result, "done");
            assert!(!is_error, "is_error=false 应透传");
        }
        other => panic!("expected SubagentStopped, got {other:?}"),
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

/// SystemNotification（MCP 上下线等）必须透传 text/level 到系统通知面。
#[tokio::test]
async fn test_agent_event_forwards_system_notification() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::AgentEvent {
            session_id: "s1".into(),
            event: AcpEvent::SystemNotification {
                text: "MCP: github connected (23 tools)".into(),
                level: "info".into(),
            },
        })
        .unwrap();

    let bridge_event = bridge_rx
        .recv()
        .await
        .expect("bridge 应收到 SystemNotification");
    match bridge_event.event {
        AcpEventData::SystemNotification(sn) => {
            assert_eq!(sn.text, "MCP: github connected (23 tools)");
            assert_eq!(sn.level, "info");
        }
        other => panic!("expected SystemNotification, got {other:?}"),
    }

    shutdown.cancel();
}

/// CommandFeedback（Phase 3 事件链路）必须解析 tag + level/message/channel 字段。
/// 实际交付形态：`AcpEvent::CommandFeedback`（peri/agent_event 通道，无标准
/// SessionUpdate tag）；level/channel 为 wire string 化 camelCase
/// （"warning" / "uiOnly"），解析为结构化枚举后推入 dual-bridge。
#[tokio::test]
async fn test_agent_event_parses_command_feedback() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::AgentEvent {
            session_id: "s1".into(),
            event: AcpEvent::CommandFeedback {
                level: "warning".into(),
                message: "命令执行失败：目标不存在".into(),
                channel: "uiOnly".into(),
            },
        })
        .unwrap();

    let bridge_event = bridge_rx
        .recv()
        .await
        .expect("bridge 应收到 CommandFeedback");
    match bridge_event.event {
        AcpEventData::CommandFeedback(fb) => {
            assert_eq!(fb.level, FeedbackLevel::Warning);
            assert_eq!(fb.message, "命令执行失败：目标不存在");
            assert_eq!(fb.channel, FeedbackChannel::UiOnly);
        }
        other => panic!("expected CommandFeedback, got {other:?}"),
    }

    shutdown.cancel();
}

/// CommandFeedback 的 channel=session 显式形态与未知 level/channel 回落
/// （Info / UiOnly）解析。
#[tokio::test]
async fn test_agent_event_command_feedback_session_channel_and_fallback() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::AgentEvent {
            session_id: "s1".into(),
            event: AcpEvent::CommandFeedback {
                level: "verbose".into(),
                message: "已写入系统消息".into(),
                channel: "session".into(),
            },
        })
        .unwrap();

    let bridge_event = bridge_rx
        .recv()
        .await
        .expect("bridge 应收到 CommandFeedback");
    match bridge_event.event {
        AcpEventData::CommandFeedback(fb) => {
            assert_eq!(fb.level, FeedbackLevel::Info, "未知 level 应回落 Info");
            assert_eq!(fb.channel, FeedbackChannel::Session);
        }
        other => panic!("expected CommandFeedback, got {other:?}"),
    }

    // 未知 channel（如 "broadcast"）应回落 UiOnly——与 level 侧未知值回落对称。
    notif_tx
        .send(AcpNotification::AgentEvent {
            session_id: "s1".into(),
            event: AcpEvent::CommandFeedback {
                level: "info".into(),
                message: "广播通道反馈".into(),
                channel: "broadcast".into(),
            },
        })
        .unwrap();

    let bridge_event = bridge_rx
        .recv()
        .await
        .expect("bridge 应收到 CommandFeedback");
    match bridge_event.event {
        AcpEventData::CommandFeedback(fb) => {
            assert_eq!(fb.level, FeedbackLevel::Info);
            assert_eq!(
                fb.channel,
                FeedbackChannel::UiOnly,
                "未知 channel 应回落 UiOnly"
            );
        }
        other => panic!("expected CommandFeedback, got {other:?}"),
    }

    shutdown.cancel();
}

#[tokio::test]
async fn test_agent_done_forwards_turn_done_to_bridges() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::AgentDone {
            session_id: "s1".into(),
            stop_reason: "end_turn".into(),
            request_id: None,
        })
        .unwrap();

    let bridge_event = bridge_rx.recv().await.expect("bridge 应收到 TurnDone");
    assert!(matches!(bridge_event.event, AcpEventData::TurnDone));

    shutdown.cancel();
}

/// Issue 2026-08-05 返工：AgentDone(cancelled) 应透传 request_id 到
/// TurnInterrupted，供 bridge 的 stale 配对判定。
#[tokio::test]
async fn test_agent_done_cancelled_forwards_request_id_to_turn_interrupted() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::AgentDone {
            session_id: "s1".into(),
            stop_reason: "cancelled".into(),
            request_id: Some("rid-1".into()),
        })
        .unwrap();

    let bridge_event = bridge_rx
        .recv()
        .await
        .expect("bridge 应收到 TurnInterrupted");
    match bridge_event.event {
        AcpEventData::TurnInterrupted { reason, request_id } => {
            assert_eq!(reason, "user cancelled");
            assert_eq!(request_id.as_deref(), Some("rid-1"), "request_id 应透传");
        }
        other => panic!("expected TurnInterrupted, got {other:?}"),
    }

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

/// Issue 2026-08-05: transport 断开（notification channel 关闭）后 notifier
/// 兜底复位 loading/排队输入并提示断连——事件流中断后 is_loading 不再有事件
/// 驱动复位路径，否则 spinner 空转且 Ctrl+C 退出/命令全被 loading 门禁拦截。
///
/// 注意：本测试的单 sender 语义与生产 wiring 一致（Issue 2 重接后 sender 由
/// pump task 独占，pump 退出即 drop sender → channel 关闭）。
#[tokio::test]
#[serial]
async fn test_channel_close_resets_loading_and_input_buffer() {
    crate::kit::atoms::init_atoms();
    // 模拟卡死状态：is_loading=true + 排队输入
    {
        let ref_guard = ACP_STATE.state();
        let mut acp = ref_guard.write();
        acp.is_loading = true;
    }
    INPUT_BUFFER.state().write().push_back("queued".into());
    *NOTIFICATION.state().write() = None;
    let hb_before = *RENDER_HEARTBEAT.state().read();

    let (notif_tx, _bridge_rx, shutdown) = spawn_test_notifier();
    // 模拟 transport 死亡：drop sender → notifier 的 recv() 返回 None
    drop(notif_tx);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert!(
        !ACP_STATE.state().read().is_loading,
        "channel 关闭后 is_loading 应复位为 false"
    );
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "排队输入应随事件流中断被清空"
    );
    assert!(
        NOTIFICATION.state().read().is_some(),
        "应提示断连（app-agent-disconnected）"
    );
    assert!(
        *RENDER_HEARTBEAT.state().read() > hb_before,
        "断连提示应触发重渲染心跳"
    );
    shutdown.cancel();
}

/// Issue 2 重接：真实生产 wiring（`AcpTuiClient::new` + `spawn_pump` +
/// `spawn_kit_notifier`）下，server transport 死亡 → pump 退出 → channel
/// 关闭 → notifier 兜底复位。
///
/// 实施前（v1）此测试失败（client struct 持有永活 sender，channel 不随 pump
/// 退出关闭）；实施后通过——同时是本方案的防回归守卫（任何把 sender 加回
/// struct/全局的改动都会使其失败）。
#[tokio::test]
#[serial]
async fn test_real_wiring_transport_death_resets_loading() {
    crate::kit::atoms::init_atoms();
    // 模拟卡死状态：is_loading=true + 排队输入
    ACP_STATE.state().write().is_loading = true;
    INPUT_BUFFER.state().write().push_back("queued".into());
    *NOTIFICATION.state().write() = None;
    let hb_before = *RENDER_HEARTBEAT.state().read();

    // 生产 wiring：client + pump + notifier（bridge 用 dummy rx 即可）
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, notification_rx) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);
    let (bridge_tx, _bridge_rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let _h = spawn_kit_notifier(notification_rx, bridge_tx, shutdown.clone());

    // 模拟生产 wiring 的额外 client 克隆（ACP_CLIENT_HANDLE / consumer / panel）
    let extra_clone = client.clone();
    drop(extra_clone);

    // 模拟 server task 死亡：drop server transport
    drop(server_transport);

    // 等待 pump 退出 → channel 关闭 → notifier 兜底
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    assert!(
        !ACP_STATE.state().read().is_loading,
        "transport 死亡后 is_loading 应复位"
    );
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "排队输入应随事件流中断被清空"
    );
    assert!(
        NOTIFICATION.state().read().is_some(),
        "应提示断连（app-agent-disconnected）"
    );
    assert!(
        *RENDER_HEARTBEAT.state().read() > hb_before,
        "断连提示应触发重渲染心跳"
    );
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
    // Phase 4 步骤 1：投影 DTO 结构化后按字段断言；缺 _meta 的条目
    // 回退 kind=Command / level=1（步骤 2 升级解析后再断言元数据字段）。
    assert_eq!(entries[0].fullname, "help");
    assert_eq!(entries[0].description, "Show help");
    assert_eq!(entries[0].kind, SlashActionKind::Command);
    assert_eq!(entries[0].level, 1);
    assert_eq!(entries[2].fullname, "archify");
    assert_eq!(entries[2].description, "Create architecture diagrams");
}

/// Phase 4 步骤 2：投影 _meta 全字段解析——periKind / periLevel /
/// periAliases / periCategory / periArgs（含 flags 为 **object 数组的
/// `FlagSpec` 往返**——wire 与本地镜像对齐，P1-5）。
#[test]
#[serial]
fn test_handle_session_update_parses_projection_fields() {
    crate::kit::atoms::init_atoms();
    *AVAILABLE_SLASH_COMMANDS.state().write() = Vec::new();
    let payload = json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "available_commands_update",
            "availableCommands": [
                {
                    "name": "demo:hello",
                    "description": "MCP skill hello",
                    "_meta": {
                        "periKind": "mcp_skill",
                        "periLevel": 2,
                        "periAliases": ["h", "hello"],
                        "periCategory": "mcp",
                        "periArgs": {
                            "positionals": [
                                {"name": "file", "kind": "Path", "required": true}
                            ],
                            "named": [
                                {"name": "out", "kind": "String", "required": false}
                            ],
                            "flags": [
                                {"name": "force", "short": "-f", "description": "force it"}
                            ]
                        }
                    }
                }
            ]
        }
    });
    let (dummy_tx, _dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = handle_session_update(payload, &dummy_tx, "test");
    let entries = AVAILABLE_SLASH_COMMANDS.state().read().clone();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.fullname, "demo:hello");
    assert_eq!(e.description, "MCP skill hello");
    assert_eq!(e.kind, SlashActionKind::McpSkill);
    assert_eq!(e.level, 2);
    assert_eq!(e.aliases, vec!["h".to_string(), "hello".to_string()]);
    assert_eq!(e.category.as_deref(), Some("mcp"));
    // ArgsSchema 全字段往返（flags 为 object 数组的 FlagSpec）
    let args = e.args.as_ref().expect("periArgs 应解析为 ArgsSchema");
    assert_eq!(args.positionals.len(), 1);
    assert_eq!(args.positionals[0].name, "file");
    assert_eq!(args.positionals[0].kind, ArgKind::Path);
    assert!(args.positionals[0].required);
    assert_eq!(args.named.len(), 1);
    assert_eq!(args.named[0].name, "out");
    assert_eq!(args.named[0].kind, ArgKind::String);
    assert!(!args.named[0].required);
    assert_eq!(args.flags.len(), 1);
    assert_eq!(args.flags[0].name, "force");
    assert_eq!(args.flags[0].short.as_deref(), Some("-f"));
    assert_eq!(args.flags[0].description.as_deref(), Some("force it"));
}

/// Phase 4 步骤 2：缺 _meta 元数据的投影条目回退 kind=Command / level=1 /
/// args=None / aliases=[]（R1：条目缺 kind 时分类整体退化 Command）。
#[test]
#[serial]
fn test_handle_session_update_projection_missing_meta_defaults() {
    crate::kit::atoms::init_atoms();
    *AVAILABLE_SLASH_COMMANDS.state().write() = Vec::new();
    let payload = json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "available_commands_update",
            "availableCommands": [
                {"name": "plaincmd", "description": "普通命令"},
                {
                    "name": "weird:entry",
                    "description": "未知 kind / 非法 level",
                    "_meta": {
                        "periKind": "unknown_kind",
                        "periLevel": 9
                    }
                }
            ]
        }
    });
    let (dummy_tx, _dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = handle_session_update(payload, &dummy_tx, "test");
    let entries = AVAILABLE_SLASH_COMMANDS.state().read().clone();
    assert_eq!(entries.len(), 2);
    for e in &entries {
        assert_eq!(
            e.kind,
            SlashActionKind::Command,
            "未知/缺失 kind 回退 Command"
        );
        assert_eq!(e.level, 1, "缺失/非法 level 回退 1");
        assert!(e.args.is_none(), "缺失 periArgs → args=None");
        assert!(e.aliases.is_empty(), "缺失 periAliases → aliases=[]");
        assert!(e.category.is_none(), "缺失 periCategory → category=None");
    }
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

/// usage_update 只解码为 bridge-owned sample；notifier 不再逐步推送 warning。
#[test]
#[serial]
fn test_usage_update_decodes_root_cache_sample_without_per_step_notification() {
    crate::kit::atoms::init_atoms();
    let payload = json!({
        "sessionId": "s1",
        "update": {
            "sessionUpdate": "usage_update",
            "_meta": {
                "inputTokens": 20000,
                "outputTokens": 100,
                "cacheReadTokens": 2000,
                "requestId": "req-1"
            }
        }
    });
    let (dummy_tx, mut dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let result = handle_session_update(payload, &dummy_tx, "test");
    match result {
        Some(AcpEventData::CacheUsageUpdated(Some(sample))) => {
            assert_eq!(sample.input_tokens, 20_000);
            assert_eq!(sample.cached_tokens, 2_000);
            assert_eq!(sample.request_id.as_deref(), Some("req-1"));
        }
        other => panic!("expected cache usage sample, got {other:?}"),
    }
    assert!(
        dummy_rx.try_recv().is_err(),
        "notifier must not push a per-step cache warning"
    );
}

#[test]
fn test_usage_update_ignores_auxiliary_and_preserves_explicit_zero_cache_read() {
    let (dummy_tx, _dummy_rx) = tokio::sync::mpsc::unbounded_channel();
    let payload = |usage_meta: serde_json::Value, params_meta: serde_json::Value| {
        json!({
            "_meta": params_meta,
            "update": {
                "sessionUpdate": "usage_update",
                "_meta": usage_meta
            }
        })
    };
    assert!(
        handle_session_update(
            payload(
                json!({"inputTokens": 100, "outputTokens": 1, "cacheReadTokens": 70}),
                json!({"peri": {"sourceAgentId": "child"}})
            ),
            &dummy_tx,
            "test"
        )
        .is_none()
    );
    let zero = handle_session_update(
        payload(
            json!({"inputTokens": 100, "outputTokens": 1, "cacheReadTokens": 0}),
            json!({}),
        ),
        &dummy_tx,
        "test",
    );
    assert!(matches!(
        zero,
        Some(AcpEventData::CacheUsageUpdated(Some(CacheUsageSample {
            input_tokens: 100,
            cached_tokens: 0,
            ..
        })))
    ));
    for invalid_meta in [json!({"inputTokens": 100, "outputTokens": 1, "cacheReadTokens": 101})] {
        assert!(
            matches!(
                handle_session_update(payload(invalid_meta, json!({})), &dummy_tx, "test"),
                Some(AcpEventData::CacheUsageUpdated(None))
            ),
            "inconsistent root usage must remain unavailable"
        );
    }
    assert!(
        handle_session_update(
            payload(json!({"inputTokens": 100, "outputTokens": 1}), json!({})),
            &dummy_tx,
            "test"
        )
        .is_none(),
        "missing cacheReadTokens must not clear a prior root sample"
    );
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
            actions: vec![PredictionAction::Summary {
                text: "修了 typo".into(),
            }],
        })
        .unwrap();

    let bridge_event = bridge_rx.recv().await.expect("bridge 应收到 Prediction");
    match bridge_event.event {
        AcpEventData::Prediction(p) => {
            assert_eq!(p.text, "next word");
            assert_eq!(p.actions.len(), 1);
            assert!(matches!(
                &p.actions[0],
                PredictionAction::Summary { text } if text == "修了 typo"
            ));
        }
        other => panic!("expected Prediction, got {other:?}"),
    }

    shutdown.cancel();
}

/// [回归测试] RequestPermission 的 payload 与 JSON-RPC RequestId 必须原子排队。
#[tokio::test]
#[serial]
async fn test_request_permission_forwards_hitl_pending_event() {
    crate::kit::atoms::init_atoms();
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::RequestPermission {
            owner: Default::default(),
            request_id_json: "\"req-123\"".into(),
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
        AcpEventData::HitlPending(pending) => {
            assert_eq!(pending.request_id_json, "\"req-123\"");
            assert_eq!(pending.payload.tool_name, "Bash");
            assert_eq!(pending.payload.tool_input["command"], "rm -rf /");
        }
        other => panic!("expected HitlPending, got {other:?}"),
    }

    shutdown.cancel();
}

/// [回归测试] Number(7) 与 String("7") 必须保留 variant，且连续通知保持 FIFO。
#[tokio::test]
async fn test_request_permission_queued_events_keep_number_and_string_ids_atomic() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();
    for (id, title) in [
        (peri_acp::transport::types::RequestId::Number(7), "A"),
        (
            peri_acp::transport::types::RequestId::String("7".to_string()),
            "B",
        ),
    ] {
        notif_tx
            .send(AcpNotification::RequestPermission {
                owner: Default::default(),
                request_id_json: serde_json::to_string(&id).unwrap(),
                params: json!({
                    "sessionId": "s1",
                    "toolCall": {"title": title, "rawInput": {"marker": title}},
                    "options": []
                }),
            })
            .unwrap();
    }
    let a = bridge_rx.recv().await.expect("应收到 A");
    let b = bridge_rx.recv().await.expect("应收到 B");
    let AcpEventData::HitlPending(a) = a.event else {
        panic!("expected A HITL")
    };
    let AcpEventData::HitlPending(b) = b.event else {
        panic!("expected B HITL")
    };
    assert_eq!(
        (a.request_id_json.as_str(), a.payload.tool_name.as_str()),
        ("7", "A")
    );
    assert_eq!(
        (b.request_id_json.as_str(), b.payload.tool_name.as_str()),
        ("\"7\"", "B")
    );
    shutdown.cancel();
}

/// [回归测试] Elicitation 的 Number/String RequestId 与问题 payload 保持 FIFO 同行。
#[tokio::test]
async fn test_elicitation_queued_events_keep_number_and_string_ids_atomic() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();
    for (id, question_id) in [
        (peri_acp::transport::types::RequestId::Number(7), "a"),
        (
            peri_acp::transport::types::RequestId::String("7".to_string()),
            "b",
        ),
    ] {
        notif_tx
            .send(AcpNotification::Elicitation {
                owner: crate::acp_client::InteractionOwner {
                    kind: crate::acp_client::ReverseInteractionKind::Elicitation,
                    ..Default::default()
                },
                request_id_json: serde_json::to_string(&id).unwrap(),
                params: json!({
                    "sessionId": "s1",
                    "requestedSchema": {"type": "object", "properties": {
                        (question_id): {"type": "string", "title": question_id}
                    }}
                }),
            })
            .unwrap();
    }
    let a = bridge_rx.recv().await.expect("应收到 A");
    let b = bridge_rx.recv().await.expect("应收到 B");
    let AcpEventData::AskUser(a) = a.event else {
        panic!("expected A AskUser")
    };
    let AcpEventData::AskUser(b) = b.event else {
        panic!("expected B AskUser")
    };
    assert_eq!(
        (
            a.request_id_json.as_str(),
            a.payload.questions[0].id.as_str()
        ),
        ("7", "a")
    );
    assert_eq!(
        (
            b.request_id_json.as_str(),
            b.payload.questions[0].id.as_str()
        ),
        ("\"7\"", "b")
    );
    shutdown.cancel();
}

/// [回归测试] notifier 在 bridge gate 前不得修改 interaction active state。
#[tokio::test]
#[serial]
async fn test_interaction_notifier_has_no_pending_atom_side_effect() {
    use crate::kit::acp_types::PendingInteraction;
    use peri_acp_types::event_data::{AskUser, HitlPending};
    let old_hitl = crate::kit::atoms::HITL_PENDING.state().read().clone();
    let old_ask = crate::kit::atoms::ASK_USER_PENDING.state().read().clone();
    *crate::kit::atoms::HITL_PENDING.state().write() = Some(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"sentinel-hitl\"".into(),
        payload: HitlPending {
            tool_name: "sentinel".into(),
            tool_input: json!(null),
            batch: None,
        },
    });
    *crate::kit::atoms::ASK_USER_PENDING.state().write() = Some(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"sentinel-ask\"".into(),
        payload: AskUser { questions: vec![] },
    });
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();
    notif_tx
        .send(AcpNotification::RequestPermission {
            owner: Default::default(),
            request_id_json: "1".into(),
            params: json!({"sessionId":"s1","toolCall":{"title":"new","rawInput":null}}),
        })
        .unwrap();
    bridge_rx.recv().await.expect("应完成 notifier conversion");
    notif_tx
        .send(AcpNotification::Elicitation {
            owner: crate::acp_client::InteractionOwner {
                kind: crate::acp_client::ReverseInteractionKind::Elicitation,
                ..Default::default()
            },
            request_id_json: "2".into(),
            params: json!({"sessionId":"s1","requestedSchema":{"type":"object","properties":{}}}),
        })
        .unwrap();
    bridge_rx
        .recv()
        .await
        .expect("应完成 elicitation conversion");
    assert_eq!(
        crate::kit::atoms::HITL_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"sentinel-hitl\""
    );
    assert_eq!(
        crate::kit::atoms::ASK_USER_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"sentinel-ask\""
    );
    shutdown.cancel();
    *crate::kit::atoms::HITL_PENDING.state().write() = old_hitl;
    *crate::kit::atoms::ASK_USER_PENDING.state().write() = old_ask;
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

    let result = tokio::time::timeout(std::time::Duration::from_millis(50), bridge_rx.recv()).await;
    assert!(
        result.is_err(),
        "non-terminal update must not end a tool: {result:?}"
    );
    shutdown.cancel();
}
