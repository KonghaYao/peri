//! Bridge 生命周期指标与 v2 事件映射回归。

use super::*;
use peri_agent::agent::LangfuseBridgeLike;

fn make_bridge() -> (
    LangfuseBridge,
    std::sync::Arc<crate::langfuse::fake_session::FakeLangfuseSession>,
) {
    // FakeLangfuseSession::new() 已返回 Arc<Self>
    let session = crate::langfuse::fake_session::FakeLangfuseSession::new("sess_c4");
    let config = crate::langfuse::config::LangfuseConfig {
        public_key: None,
        secret_key: None,
        host: "https://cloud.langfuse.com".to_string(),
        trace_sampling: 0.0,
        error_span_always: true,
        batch_max_events: 50,
        batch_flush_interval_secs: 10,
        user_id: None,
        ..Default::default()
    };
    let tracer = crate::langfuse::tracer::LangfuseTracer::new(
        session.clone(),
        "sess_c4".to_string(),
        config,
    );
    let bridge = LangfuseBridge::new(
        Arc::new(parking_lot::Mutex::new(tracer)),
        "test-provider".to_string(),
        None,
    );
    (bridge, session)
}

/// C4: v2 SubagentStart/Stop → Unified 映射字段完整（child/parent/name/bg/result/error）
#[test]
fn test_from_observe_event_subagent_start_stop_mapping() {
    use peri_acp_types::identity::AgentId;
    use peri_agent::session::turn::TurnId;

    let turn_id = TurnId::new();
    let parent = AgentId::new();
    let child = AgentId::new();

    let start = ObserveEvent::SubagentStart {
        turn_id,
        agent_id: parent,
        child_agent_id: child,
        agent_name: "code-reviewer".to_string(),
        is_background: true,
        parent_tool_call_id: None,
    };
    match UnifiedLangfuseEvent::from_observe_event(start) {
        Some(UnifiedLangfuseEvent::SubagentStart {
            parent_agent_id,
            child_agent_id,
            agent_name,
            is_background,
        }) => {
            assert_eq!(parent_agent_id, parent.to_string());
            assert_eq!(child_agent_id, child.to_string());
            assert_eq!(agent_name, "code-reviewer");
            assert!(is_background);
        }
        other => panic!("应为 SubagentStart，实际 {:?}", other),
    }

    let stop = ObserveEvent::SubagentStop {
        turn_id,
        agent_id: parent,
        child_agent_id: child,
        agent_name: "code-reviewer".to_string(),
        result: "done".to_string(),
        is_error: false,
        subagent_failure: None,
    };
    match UnifiedLangfuseEvent::from_observe_event(stop) {
        Some(UnifiedLangfuseEvent::SubagentStop {
            parent_agent_id,
            child_agent_id,
            agent_name,
            result,
            is_error,
        }) => {
            assert_eq!(parent_agent_id, parent.to_string());
            assert_eq!(child_agent_id, child.to_string());
            assert_eq!(agent_name, "code-reviewer");
            assert_eq!(result, "done");
            assert!(!is_error);
        }
        other => panic!("应为 SubagentStop，实际 {:?}", other),
    }
}

/// C4: process_event 的 Start 注册 / Stop 注销 + 计数（归属逻辑未动）
#[test]
fn test_process_event_registers_and_deregisters() {
    use peri_acp_types::identity::AgentId;

    let (bridge, _session) = make_bridge();
    let mut active_stage = HashMap::new();
    let parent = AgentId::new();
    let child = AgentId::new();

    // Start → 注册 + 计数
    bridge.process_event(
        &UnifiedLangfuseEvent::SubagentStart {
            parent_agent_id: parent.to_string(),
            child_agent_id: child.to_string(),
            agent_name: "explorer".to_string(),
            is_background: false,
        },
        &mut active_stage,
    );
    assert_eq!(
        bridge.active_subagent_count(),
        1,
        "Start 后应有 1 个活跃注册"
    );
    assert_eq!(
        bridge.subagent_event_counts(),
        (1, 0),
        "Start 计数应为 (1, 0)"
    );

    // 重复 Start → 覆盖注册（不增加条目），计数仍递增
    bridge.process_event(
        &UnifiedLangfuseEvent::SubagentStart {
            parent_agent_id: parent.to_string(),
            child_agent_id: child.to_string(),
            agent_name: "explorer".to_string(),
            is_background: false,
        },
        &mut active_stage,
    );
    assert_eq!(
        bridge.active_subagent_count(),
        1,
        "重复 Start 不增加注册条目"
    );

    // Stop → 注销 + 计数
    bridge.process_event(
        &UnifiedLangfuseEvent::SubagentStop {
            parent_agent_id: parent.to_string(),
            child_agent_id: child.to_string(),
            agent_name: "explorer".to_string(),
            result: "found".to_string(),
            is_error: false,
        },
        &mut active_stage,
    );
    assert_eq!(bridge.active_subagent_count(), 0, "Stop 后注册应清空");
    assert_eq!(bridge.subagent_event_counts(), (2, 1));

    // 无对应 Start 的 Stop → 不 panic，计数仍递增（阶段② incomplete 分支）
    bridge.process_event(
        &UnifiedLangfuseEvent::SubagentStop {
            parent_agent_id: parent.to_string(),
            child_agent_id: AgentId::new().to_string(),
            agent_name: "ghost".to_string(),
            result: "lost".to_string(),
            is_error: true,
        },
        &mut active_stage,
    );
    assert_eq!(bridge.active_subagent_count(), 0);
    assert_eq!(bridge.subagent_event_counts(), (2, 2));
}

/// C4: 经 LangfuseBridgeLike 完整链路（forwarder 同入口）Start/Stop 可达
#[test]
fn test_bridge_like_process_observe_start_stop() {
    use peri_acp_types::identity::AgentId;
    use peri_agent::session::turn::TurnId;

    let (bridge, _session) = make_bridge();
    let parent = AgentId::new();
    let child = AgentId::new();
    let turn_id = TurnId::new();

    bridge.process_observe_event(&ObserveEvent::SubagentStart {
        turn_id,
        agent_id: parent,
        child_agent_id: child,
        agent_name: "plan".to_string(),
        is_background: false,
        parent_tool_call_id: None,
    });
    assert_eq!(bridge.active_subagent_count(), 1);

    bridge.process_observe_event(&ObserveEvent::SubagentStop {
        turn_id,
        agent_id: parent,
        child_agent_id: child,
        agent_name: "plan".to_string(),
        result: "done".to_string(),
        is_error: false,
        subagent_failure: None,
    });
    assert_eq!(bridge.active_subagent_count(), 0);
    assert_eq!(bridge.subagent_event_counts(), (1, 1));
}

/// v2 观测字段映射：reasoning 分片保留来源 agent，Receive 排空保留最早入队时间。
#[test]
fn test_from_observe_event_reasoning_and_queue_wait_fields() {
    use peri_acp_types::identity::AgentId;
    use peri_agent::session::turn::TurnId;

    let agent = AgentId::new();
    let turn_id = TurnId::new();

    match UnifiedLangfuseEvent::from_observe_event(ObserveEvent::AiReasoningChunk {
        turn_id,
        agent_id: agent,
        text: "thinking".to_string(),
        source_agent_id: None,
    }) {
        Some(UnifiedLangfuseEvent::AiReasoningChunk { agent_id, text }) => {
            assert_eq!(
                agent_id,
                agent.to_string(),
                "reasoning 分片须保留 agent 身份"
            );
            assert_eq!(text, "thinking");
        }
        other => panic!("应为 AiReasoningChunk，实际 {:?}", other),
    }

    match UnifiedLangfuseEvent::from_observe_event(ObserveEvent::MessageQueueDrained {
        turn_id,
        agent_id: agent,
        prompt: 1,
        defer: 2,
        info: 3,
        oldest_enqueued_at: Some("2026-07-14T10:00:00.000000Z".to_string()),
    }) {
        Some(UnifiedLangfuseEvent::MessageQueueDrained {
            oldest_enqueued_at,
            prompt,
            defer,
            info,
            ..
        }) => {
            assert_eq!(prompt, 1);
            assert_eq!(defer, 2);
            assert_eq!(info, 3);
            assert_eq!(
                oldest_enqueued_at.as_deref(),
                Some("2026-07-14T10:00:00.000000Z")
            );
        }
        other => panic!("应为 MessageQueueDrained，实际 {:?}", other),
    }
}
