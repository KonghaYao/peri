use super::*;
use crate::kit::acp_types::{FeedbackChannel, FeedbackLevel};
use peri_acp::event::AcpEvent;
use peri_acp_types::event_data::PredictionAction;
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
                parent_tool_call_id: Some("tool-7".into()),
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
            parent_tool_call_id,
            ..
        } => {
            assert_eq!(agent_id, "abc-123", "agent_id 应从 instance_id 映射");
            assert_eq!(agent_name, "explore");
            assert_eq!(
                parent_tool_call_id.as_deref(),
                Some("tool-7"),
                "父工具调用 id 必须透传到 TUI 事件（配对身份）"
            );
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
                diagnostic: None,
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
                subagent_failure: None,
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
                subagent_failure: None,
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

    notif_tx
        .send(AcpNotification::AgentDone {
            session_id: "s1".into(),
            stop_reason: "end_turn".into(),
            request_id: None,
        })
        .unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), bridge_rx.recv())
        .await
        .expect("notifier must process the barrier")
        .expect("notifier must keep the bridge open");
    assert!(
        matches!(event.event, AcpEventData::TurnDone),
        "unexpected event before barrier: {event:?}"
    );

    shutdown.cancel();
}

#[tokio::test]
async fn test_goal_snapshot_forwards_to_session_owned_bridge() {
    crate::kit::atoms::init_atoms();
    *crate::kit::atoms::GOAL_SNAPSHOT.state().write() = None;
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::AgentEvent {
            session_id: "s1".into(),
            event: AcpEvent::GoalSnapshot {
                objective: Some("ship status bar".into()),
                status: Some(peri_acp_types::goal::GoalStatus::Active),
                token_budget: Some(10_000),
                tokens_used: 2_000,
                time_used_seconds: 30,
                continuation_count: 3,
                blocked_reason: None,
            },
        })
        .unwrap();

    let bridge_event = bridge_rx.recv().await.expect("bridge should receive goal");
    assert_eq!(bridge_event.active_session_id, "s1");
    match bridge_event.event {
        AcpEventData::GoalSnapshot {
            objective,
            status,
            continuation_count,
            ..
        } => {
            assert_eq!(objective.as_deref(), Some("ship status bar"));
            assert_eq!(status, Some(peri_acp_types::goal::GoalStatus::Active));
            assert_eq!(continuation_count, 3);
        }
        other => panic!("expected GoalSnapshot, got {other:?}"),
    }
    assert!(crate::kit::atoms::GOAL_SNAPSHOT.state().read().is_none());

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

#[test]
fn test_user_input_notifier_retains_delivered_identity_and_content() {
    let content = peri_acp_types::messages::MessageContent::text("  中文\n");
    let event = convert_agent_event(AcpEvent::UserInputDelivered {
        generation: "g".into(),
        input_id: "i".into(),
        content: content.clone(),
    });
    assert!(
        matches!(event, Some(AcpEventData::UserInputDelivered { generation, input_id, content:received })
        if generation == "g" && input_id == "i" && received == content),
        "canonical 消息必须保留实例、稳定身份和完整正文"
    );
}

#[test]
fn test_user_input_notifier_opens_existing_prompt_lifecycle() {
    let event = convert_agent_event(AcpEvent::UserInputRunStarted {
        generation: "g".into(),
        request_id: "run".into(),
    });
    assert!(
        matches!(event, Some(AcpEventData::PromptSubmitted { request_id:Some(id) }) if id == "run"),
        "运行标记必须复用已有 loading 生命周期并携带配对 ID"
    );
}

#[test]
fn test_user_input_notifier_preserves_queue_snapshot_revision() {
    let snapshot = peri_acp_types::session::UserInputQueueSnapshot {
        session_id: "s".into(),
        generation: "g".into(),
        revision: 9,
        active_request_id: Some("run".into()),
        items: vec![],
    };
    let event = convert_agent_event(AcpEvent::UserInputQueueChanged {
        snapshot: snapshot.clone(),
    });
    assert!(
        matches!(event, Some(AcpEventData::UserInputQueueChanged { snapshot:received }) if serde_json::to_value(&received).unwrap() == serde_json::to_value(&snapshot).unwrap()),
        "队列通知必须保留完整权威快照"
    );
}

#[test]
fn test_user_input_replay_keeps_message_id_for_canonical_dedup() {
    let update = session_update::decode_stream_update(
        &json!({
            "sessionId":"s", "update":{
                "sessionUpdate":"user_message_chunk", "messageId":"i",
                "content":{"type":"text","text":"历史输入"}
            }
        }),
        "s",
    );
    assert!(
        matches!(update.event, Some(AcpEventData::ReplayedUserBubble { input_id, text })
        if input_id == "i" && text == "历史输入"),
        "重放原用户气泡时必须保存 messageId，供迟到 Delivered 去重"
    );
}
