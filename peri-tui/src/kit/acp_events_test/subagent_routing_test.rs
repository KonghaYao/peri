use super::*;

fn make_state(phase: SessionPhase, variant: u8) -> BridgeState {
    BridgeState {
        variant,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase,
        popup_kind: None,
        generation: 0,
        active_session_id: String::new(),
        compact_just_completed: false,
        last_submitted_text: None,
        last_pushed_text_len: 0,
        last_pushed_reasoning_len: 0,
        last_successful_todos: None,
        last_successful_todo_sequence: None,
        next_todo_sequence: 0,
        todo_call_inputs: std::collections::HashMap::new(),
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
        pending_cache_usage: None,
        publication_intent: Default::default(),
        folded_history: Default::default(),
    }
}

#[test]
#[serial]
fn test_dispatch_subagent_streaming_updates_current_turn_group() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = make_state(SessionPhase::Idle, 0);

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "agent-1".into(),
            agent_name: "researcher".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "child text".into(),
            message_id: None,
            agent_id: Some("agent-1".into()),
        }),
    );

    let current_turn = state.current_turn.view_models().clone();
    assert_eq!(current_turn.len(), 1);
    match &current_turn[0] {
        TuiRenderUnit::TuiSubAgentGroup(group) => {
            assert_eq!(group.agent_id, "agent-1");
            assert_eq!(group.view_models.len(), 1);
        }
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    }
}

/// [§6.7] `stop_subagent` 冻结子 turn 的 trailing 流式段（review MED-3 回归）：
/// 子 bubble 的 `started_at` 清除、`duration_ms` 冻结——子 turn 不经过快照折叠
/// pass，不冻结则 trailing bubble 保持 Running 形态（elapsed 持续增长），详情
/// 面板对已完成 subagent 渲染永久的 `◐ Thinking… Ns`。
#[test]
#[serial]
fn test_subagent_stopped_freezes_child_trailing_bubble() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = make_state(SessionPhase::PromptRunning, 0);

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "agent-1".into(),
            agent_name: "researcher".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "child text".into(),
            message_id: Some("c1".into()),
            agent_id: Some("agent-1".into()),
        }),
    );

    // 流式期间：子 turn trailing bubble 持有 started_at（Running 形态）。
    let running_vms = state.current_turn.view_models().clone();
    let b = match &running_vms[0] {
        TuiRenderUnit::TuiSubAgentGroup(g) => match &g.view_models[0] {
            TuiRenderUnit::TuiAssistantBubble(b) => b,
            other => panic!("expected child TuiAssistantBubble, got {other:?}"),
        },
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    };
    assert!(
        b.started_at.is_some(),
        "子 turn 流式段应持有 started_at（Running 形态）"
    );
    assert_eq!(b.duration_ms, None, "流式期间无冻结值");

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "agent-1".into(),
            result: String::new(),
            is_error: false,
        },
    );

    // stop 后：started_at 清除 + duration_ms 冻结（详情面板不再显示增长中的
    // `◐ Thinking… Ns`）。
    let stopped_vms = state.current_turn.view_models().clone();
    let b = match &stopped_vms[0] {
        TuiRenderUnit::TuiSubAgentGroup(g) => match &g.view_models[0] {
            TuiRenderUnit::TuiAssistantBubble(b) => b,
            other => panic!("expected child TuiAssistantBubble, got {other:?}"),
        },
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    };
    assert_eq!(
        b.started_at, None,
        "stop_subagent 后子 trailing 段 started_at 清除"
    );
    assert!(
        b.duration_ms.is_some(),
        "stop_subagent 后子 trailing 段 duration_ms 冻结"
    );
}

/// 同步 sub-agent 的 ToolStarted/ToolEnded 事件应路由到 SubAgentAccumulator，
/// 并反映在当前 turn 的 TuiSubAgentGroup 中。
#[test]
#[serial]
fn test_dispatch_sync_subagent_tool_routed_to_group() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = make_state(SessionPhase::Idle, 0);

    // 启动同步 sub-agent
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sync-1".into(),
            agent_name: "coder".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    // 工具开始
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            agent_id: Some("sync-1".into()),
            tool_name: "Read".into(),
            tool_id: "tc-1".into(),
            input_summary: "path: foo.rs".into(),
            raw_input: serde_json::Value::Null,
        }),
    );
    // 工具结束
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
            agent_id: Some("sync-1".into()),
            tool_id: "tc-1".into(),
            output_summary: "10 lines".into(),
            is_error: false,
        }),
    );

    let current_turn = state.current_turn.view_models().clone();
    // current_turn 中应有 1 个 TuiSubAgentGroup
    assert_eq!(current_turn.len(), 1, "current_turn 应包含 1 个元素");
    match &current_turn[0] {
        TuiRenderUnit::TuiSubAgentGroup(group) => {
            assert_eq!(group.agent_id, "sync-1");
            assert!(
                !group.view_models.is_empty(),
                "group.view_models 应至少包含 1 个工具卡片，实际 {} 个",
                group.view_models.len()
            );
            let has_tool_card = group
                .view_models
                .iter()
                .any(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_)));
            assert!(
                has_tool_card,
                "group.view_models 应包含至少一个 TuiToolCard"
            );
        }
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    }
}

/// [回归测试] 并行多 Agent 批次（multitask）里，非首个工具调用的 `ToolStarted`
/// 只在 dispatch 阶段发出（经 forwarder hop），会与子 Agent invoke 内直发的
/// `SubagentStarted` 竞争而晚到 TUI。晚到的 Agent 卡片必须接管早到的子分组：
/// 否则第二个 subagent 的工具行会挂在上一个仍在 loading 的 Agent 调用之下。
#[test]
#[serial]
fn test_late_agent_tool_card_adopts_early_subagent_group() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = make_state(SessionPhase::Idle, 0);

    let agent_tool = |tool_id: &str, summary: &str| {
        AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            agent_id: None,
            tool_name: "Agent".into(),
            tool_id: tool_id.into(),
            input_summary: summary.into(),
            raw_input: serde_json::json!({ "prompt": summary }),
        })
    };
    let child_tool = |agent_id: &str, tool_id: &str, name: &str| {
        AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            agent_id: Some(agent_id.into()),
            tool_name: name.into(),
            tool_id: tool_id.into(),
            input_summary: format!("{name} input"),
            raw_input: serde_json::Value::Null,
        })
    };

    // 第一个 Agent 调用：卡片先到（流式提前 ToolStarted），子分组紧随其后。
    dispatch_and_notify(&mut state, &agent_tool("agent-call-1", "start coder"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "child-1".into(),
            agent_name: "coder".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(&mut state, &child_tool("child-1", "child-tool-1", "Read"));

    // 第二个 Agent 调用的子 Agent 先启动（SubagentStarted 抢在父卡片之前到达）。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "child-2".into(),
            agent_name: "reviewer".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(&mut state, &child_tool("child-2", "child-tool-2", "Grep"));
    // 迟到的第二个 Agent 卡片
    dispatch_and_notify(&mut state, &agent_tool("agent-call-2", "start reviewer"));

    let vms: Vec<_> = state.current_turn.view_models().iter().cloned().collect();
    let layout: Vec<String> = vms
        .iter()
        .map(|vm| match vm {
            TuiRenderUnit::TuiToolCard(card) => format!("card:{}", card.tool_id),
            TuiRenderUnit::TuiSubAgentGroup(group) => format!("group:{}", group.agent_id),
            other => format!("other:{other:?}"),
        })
        .collect();
    assert_eq!(
        layout,
        vec![
            "card:agent-call-1",
            "group:child-1",
            "card:agent-call-2",
            "group:child-2",
        ],
        "每个 Agent 调用必须紧跟自己的子分组，不得挂到上一个 loading 的 Agent 调用之下"
    );

    // 已发布快照同样保持该顺序（用户可见的消息区投影）。
    let published = VIEW_MODELS.state().read().clone();
    let published_layout: Vec<String> = published
        .items
        .iter()
        .map(|vm| match vm {
            TuiRenderUnit::TuiToolCard(card) => format!("card:{}", card.tool_id),
            TuiRenderUnit::TuiSubAgentGroup(group) => format!("group:{}", group.agent_id),
            other => format!("other:{other:?}"),
        })
        .collect();
    assert_eq!(published_layout, layout, "发布快照应保持增量投影的段顺序");
}

/// [回归测试] 并发批次下的身份配对：`SubagentStarted.parent_tool_call_id` 是
/// 配对权威依据。子 Agent 启动顺序与工具卡片顺序相反时，两个分组不得互换
/// （到达顺序兜底无法判定并发批次的真实归属）。
#[test]
#[serial]
fn test_concurrent_agent_calls_pair_by_parent_tool_call_id() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = make_state(SessionPhase::Idle, 0);

    // 并行批次：两张 Agent 卡片都先到（ToolStarted 阶段）。
    for (tool_id, summary) in [
        ("agent-call-1", "start coder"),
        ("agent-call-2", "start reviewer"),
    ] {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
                agent_id: None,
                tool_name: "Agent".into(),
                tool_id: tool_id.into(),
                input_summary: summary.into(),
                raw_input: serde_json::json!({ "prompt": summary }),
            }),
        );
    }

    // 子 Agent 启动顺序与卡片顺序相反：第二个调用先启动。
    for (agent_id, agent_name, parent) in [
        ("child-2", "reviewer", "agent-call-2"),
        ("child-1", "coder", "agent-call-1"),
    ] {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::SubagentStarted {
                agent_id: agent_id.into(),
                agent_name: agent_name.into(),
                is_background: false,
                parent_tool_call_id: Some(parent.into()),
            },
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
                agent_id: Some(agent_id.into()),
                tool_name: "Read".into(),
                tool_id: format!("{agent_id}-tool"),
                input_summary: "file.rs".into(),
                raw_input: serde_json::Value::Null,
            }),
        );
    }

    let layout: Vec<String> = state
        .current_turn
        .view_models()
        .iter()
        .map(|vm| match vm {
            TuiRenderUnit::TuiToolCard(card) => format!("card:{}", card.tool_id),
            TuiRenderUnit::TuiSubAgentGroup(group) => format!("group:{}", group.agent_id),
            other => format!("other:{other:?}"),
        })
        .collect();
    assert_eq!(
        layout,
        vec![
            "card:agent-call-1",
            "group:child-1",
            "card:agent-call-2",
            "group:child-2",
        ],
        "身份配对：并发批次里子分组必须落在自己的 Agent 调用之下，不得按到达顺序互换"
    );
}
