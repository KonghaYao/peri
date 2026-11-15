use super::*;

fn make_state(phase: SessionPhase, variant: u8) -> BridgeState {
    BridgeState {
        child_runs: Default::default(),
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
        todo_baseline_sequence: None,
        next_todo_sequence: 0,
        todo_call_inputs: std::collections::HashMap::new(),
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
        pending_cache_usage: None,
        publication_intent: Default::default(),
        folded_history: Default::default(),
        window: Default::default(),
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

#[test]
#[serial]
fn test_flush_retains_unpaired_completed_subagent_until_running_group_finishes() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = make_state(SessionPhase::PromptRunning, 0);
    for (agent_id, text) in [("completed", "done"), ("running", "still running")] {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::SubagentStarted {
                agent_id: agent_id.into(),
                agent_name: agent_id.into(),
                is_background: false,
                parent_tool_call_id: None,
            },
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: text.into(),
                message_id: Some(agent_id.into()),
                agent_id: Some(agent_id.into()),
            }),
        );
        if agent_id == "completed" {
            dispatch_and_notify(
                &mut state,
                &AcpEventData::SubagentStopped {
                    agent_id: agent_id.into(),
                    result: String::new(),
                    is_error: false,
                },
            );
        }
    }
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(state.current_turn.has_running_subagent());
    assert_eq!(
        state.current_turn.subagents.len(),
        2,
        "未配对的已完成子 Agent 不能越过仍在运行的分组单独归档（R05）"
    );
    assert!(
        !state.committed.iter().any(|unit| {
            matches!(unit, TuiRenderUnit::TuiSubAgentGroup(group) if group.agent_id == "completed")
        }),
        "committed 渲染在 current_turn 之前，提前归档会打乱分组顺序"
    );
    let archived_len = state.committed.len();
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(state.committed.len(), archived_len, "重复 flush 不重复归档");
    // 移除 index 0 后，剩余容器与待配对表都必须指向新的 index 0。
    state
        .current_turn
        .start_tool(crate::kit::acp_types::ToolCardAccumulator::new(
            "late-launcher".into(),
            "Agent".into(),
            String::new(),
        ));
    assert!(
        state
            .current_turn
            .append_subagent_text("running", " continued")
    );
    let units = state.current_turn.view_models();
    let group = units
        .iter()
        .find_map(|unit| match unit {
            TuiRenderUnit::TuiSubAgentGroup(group) if group.agent_id == "running" => Some(group),
            _ => None,
        })
        .expect("运行中的分组必须仍在容器里");
    assert_eq!(group.agent_id, "running");
    assert!(group.view_models.iter().any(|unit| {
        matches!(unit, TuiRenderUnit::TuiAssistantBubble(b) if b.text.ends_with(" continued"))
    }));
    state.current_turn.stop_subagent("running", false, "");
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(state.current_turn.is_empty());
    assert_eq!(
        state
            .committed
            .iter()
            .filter(|unit| { matches!(unit, TuiRenderUnit::TuiSubAgentGroup(_)) })
            .count(),
        2
    );
}

#[test]
#[serial]
fn test_flush_resets_after_completed_subagent_is_archived() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = make_state(SessionPhase::PromptRunning, 0);
    state
        .current_turn
        .append_text("before child", Some("root-before"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "done-agent".into(),
            agent_name: "done-agent".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "done-agent".into(),
            result: String::new(),
            is_error: false,
        },
    );
    state
        .current_turn
        .append_text("after child", Some("root-after"));
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(state.current_turn.is_empty());
    assert_eq!(state.committed.len(), 3);
    assert!(
        matches!(&state.committed[0], TuiRenderUnit::TuiAssistantBubble(b) if b.text == "before child")
    );
    assert!(
        matches!(&state.committed[1], TuiRenderUnit::TuiSubAgentGroup(group) if group.agent_id == "done-agent")
    );
    assert!(
        matches!(&state.committed[2], TuiRenderUnit::TuiAssistantBubble(b) if b.text == "after child")
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
            origin_message_id: None,
            origin_readable: None,
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
            origin_message_id: None,
            origin_readable: None,
        })
    };
    let child_tool = |agent_id: &str, tool_id: &str, name: &str| {
        AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            agent_id: Some(agent_id.into()),
            tool_name: name.into(),
            tool_id: tool_id.into(),
            input_summary: format!("{name} input"),
            raw_input: serde_json::Value::Null,
            origin_message_id: None,
            origin_readable: None,
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
                origin_message_id: None,
                origin_readable: None,
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
                origin_message_id: None,
                origin_readable: None,
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

#[test]
fn completed_subagent_is_consumed_only_at_archive_boundary() {
    let mut turn = crate::kit::acp_types::CurrentTurn::new();
    turn.start_subagent("done".into(), "done".into(), None);
    turn.start_subagent("running".into(), "running".into(), None);
    assert!(turn.subagents.iter().all(|subagent| !subagent.archived));
    turn.stop_subagent("done", false, "");
    assert_eq!(turn.subagents.len(), 2, "stop 本身保留路由窗口");
    let completed = turn.take_completed_subagents();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].agent_id, "done");
    assert!(completed[0].archived);
    assert!(!completed[0].is_running);
    assert_eq!(turn.subagents.len(), 1);
    assert!(turn.has_running_subagent());
    assert!(turn.take_completed_subagents().is_empty());
}

/// [回归测试 R05/M2] 后台 A 分组仍在运行时，已完成 B 分组随整轮一起归档，
/// 展示顺序不变；不能只抽 B 或把整轮留在 current_turn。
///
/// committed 整段渲染在 current_turn 之前；归档时必须保持 segment 顺序，
/// 否则用户看到的时间线会变成 `A 调用卡 → B 分组 → A 分组 → B 调用卡`。
#[test]
#[serial]
fn test_flush_does_not_reorder_completed_subagent_after_running_group() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = make_state(SessionPhase::PromptRunning, 0);

    // A：后台子 Agent，启动卡已完成，分组仍运行。
    // B：同步子 Agent，卡片与分组都已完成，但其分组位于 A 分组之后。
    for (tool_id, agent_id, stop) in [
        ("agent-call-a", "agent-a", false),
        ("agent-call-b", "agent-b", true),
    ] {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
                agent_id: None,
                tool_name: "Agent".into(),
                tool_id: tool_id.into(),
                input_summary: format!("start {agent_id}"),
                raw_input: serde_json::json!({ "prompt": agent_id }),
                origin_message_id: None,
                origin_readable: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
                agent_id: None,
                tool_id: tool_id.into(),
                output_summary: "started".into(),
                is_error: false,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::SubagentStarted {
                agent_id: agent_id.into(),
                agent_name: agent_id.into(),
                is_background: agent_id == "agent-a",
                parent_tool_call_id: Some(tool_id.into()),
            },
        );
        if stop {
            dispatch_and_notify(
                &mut state,
                &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                    text: "done".into(),
                    message_id: Some(format!("{agent_id}-msg")),
                    agent_id: Some(agent_id.into()),
                }),
            );
            dispatch_and_notify(
                &mut state,
                &AcpEventData::SubagentStopped {
                    agent_id: agent_id.into(),
                    result: String::new(),
                    is_error: false,
                },
            );
        }
    }

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(
        state.current_turn.is_empty(),
        "只有后台 A 仍在运行，普通 TurnDone 必须整段归档并 reset（M2）"
    );

    let layout: Vec<String> = state
        .committed
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
            "card:agent-call-a",
            "group:agent-a",
            "card:agent-call-b",
            "group:agent-b",
        ],
        "整段归档后展示顺序不变：B 分组仍必须位于运行中的 A 分组之后"
    );
}

/// [回归测试 M2] 固定后台 child 长期 running 时，父会话连续 TurnDone 必须
/// 整段归档；CurrentTurn 的 raw/VM/segments/tool_cards 不随轮数增长。
#[test]
#[serial]
fn test_turn_done_with_only_background_child_keeps_current_turn_bounded() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
    let mut state = make_state(SessionPhase::PromptRunning, 0);

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "bg-long".into(),
            agent_name: "bg-long".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    assert!(state.current_turn.has_running_subagent());
    assert!(!state.current_turn.has_running_sync_subagent());

    for round in 0..1000 {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: format!("parent round {round}"),
                message_id: Some(format!("parent-{round}")),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
                agent_id: None,
                tool_name: "Read".into(),
                tool_id: format!("tool-{round}"),
                input_summary: "file.rs".into(),
                raw_input: serde_json::Value::Null,
                origin_message_id: None,
                origin_readable: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
                agent_id: None,
                tool_id: format!("tool-{round}"),
                output_summary: "ok".into(),
                is_error: false,
            }),
        );
        dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        assert!(
            state.current_turn.is_empty(),
            "round {round}: TurnDone 后 CurrentTurn 必须为空"
        );
        assert_eq!(
            state.current_turn.view_model_count(),
            0,
            "round {round}: CurrentTurn VM 不得随轮数增长"
        );
        assert_eq!(
            state.current_turn.raw_memory_bytes().body,
            0,
            "round {round}: CurrentTurn raw 正文不得随轮数增长"
        );
        assert!(
            state.current_turn.tool_cards.is_empty(),
            "round {round}: 工具卡不得滞留"
        );
    }
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
}

/// [回归测试 M2] 后台 A 长期 running、同步 B 已完成时：
/// - 用户气泡边界与普通 TurnDone 都整段归档父消息，展示顺序不变；
/// - 迟到的 A 文本/Stop 只进 BG_LIVE_DETAIL，不进入主回复。
#[test]
#[serial]
fn test_turn_boundary_archives_parent_and_routes_late_background_events() {
    use crate::kit::atoms::{BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS};
    use crate::kit::stream_data::TuiTextChunk;

    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
    crate::kit::atoms::BG_TASK_REVISION.set(None);
    let mut state = make_state(SessionPhase::PromptRunning, 0);

    let root_text = |text: &str, message_id: &str| {
        AcpEventData::TextChunk(TuiTextChunk {
            text: text.into(),
            message_id: Some(message_id.into()),
            agent_id: None,
        })
    };
    let child_text = |agent_id: &str, text: &str, message_id: &str| {
        AcpEventData::TextChunk(TuiTextChunk {
            text: text.into(),
            message_id: Some(message_id.into()),
            agent_id: Some(agent_id.into()),
        })
    };

    // 后台 A 的任务身份：迟到事件需要能落进 BG_LIVE_DETAIL。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "task-a".into(),
            kind: "agent".into(),
            summary: "bg".into(),
            started_at: String::new(),
            pid: None,
            revision: None,
            status: None,
            child_thread_id: Some("bg-a".into()),
        }),
    );
    dispatch_and_notify(&mut state, &root_text("p0", "m0"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "bg-a".into(),
            agent_name: "bg-a".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(&mut state, &child_text("bg-a", "a-live", "bg-live"));

    // 用户气泡边界：A 仍在运行也要把前面的父正文与 A 一起归档。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "u1".into(),
            attachments: Vec::new(),
        },
    );
    assert!(
        state.current_turn.is_empty(),
        "用户气泡边界必须整段归档，后台 A 不得保留容器"
    );

    // 同步 B 在本轮内启动并完成；父正文仍排在 B 之后。
    dispatch_and_notify(&mut state, &root_text("p1", "m1"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sync-b".into(),
            agent_name: "sync-b".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(&mut state, &child_text("sync-b", "b-done", "b-msg"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "sync-b".into(),
            result: String::new(),
            is_error: false,
        },
    );
    dispatch_and_notify(&mut state, &root_text("p1-after", "m1b"));
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    dispatch_and_notify(&mut state, &root_text("p2", "m2"));
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(
        state.current_turn.is_empty(),
        "多轮 TurnDone 后 CurrentTurn 必须保持为空"
    );

    let layout: Vec<String> = state
        .committed
        .iter()
        .map(|unit| match unit {
            TuiRenderUnit::TuiAssistantBubble(bubble) => format!("text:{}", bubble.text),
            TuiRenderUnit::TuiUserBubble(bubble) => format!("user:{}", bubble.text),
            TuiRenderUnit::TuiSubAgentGroup(group) => format!("group:{}", group.agent_id),
            other => format!("other:{other:?}"),
        })
        .collect();
    assert_eq!(
        layout,
        vec![
            "text:p0",
            "group:bg-a",
            "user:u1",
            "text:p1",
            "group:sync-b",
            "text:p1-after",
            "text:p2",
        ],
        "整段归档后父消息与子分组顺序不变"
    );

    let committed_len = state.committed.len();
    fn group_texts(state: &BridgeState, agent_id: &str) -> Vec<String> {
        state
            .committed
            .iter()
            .find_map(|unit| match unit {
                TuiRenderUnit::TuiSubAgentGroup(group) if group.agent_id == agent_id => Some(
                    group
                        .view_models
                        .iter()
                        .filter_map(|vm| match vm {
                            TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.clone()),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    }
    assert_eq!(
        group_texts(&state, "bg-a"),
        vec!["a-live".to_string()],
        "归档组冻结在 turn 边界那一刻"
    );

    // 迟到的 A 文本：只累积到 BG_LIVE_DETAIL，committed/current_turn 都不变。
    dispatch_and_notify(&mut state, &child_text("bg-a", "a-late", "bg-live"));
    assert_eq!(
        state.committed.len(),
        committed_len,
        "迟到后台文本不得进入主回复"
    );
    assert!(state.current_turn.is_empty());
    assert_eq!(group_texts(&state, "bg-a"), vec!["a-live".to_string()]);

    // 迟到的 A Stop：只结算后台预览，不改主回复。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "bg-a".into(),
            result: "done".into(),
            is_error: false,
        },
    );
    assert_eq!(
        state.committed.len(),
        committed_len,
        "迟到后台 Stop 不得进入主回复"
    );
    assert!(state.current_turn.is_empty());

    let live_store = BG_LIVE_DETAIL.state();
    let live = live_store.read();
    let detail = live.get("task-a").expect("bg live detail");
    let live_texts: Vec<String> = detail
        .nested_units
        .iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(live_texts, vec!["a-livea-late".to_string()]);
    assert_eq!(detail.status, crate::kit::atoms::BgLiveStatus::Succeeded);
    drop(live);

    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
}

/// [回归 M2 / CodeRabbit] `SubagentStarted` 先于 `BgTaskStarted`：任务行缺失期间
/// 的 child 预览由占位 owner 承接，行到达后绑定并接管后续文本/终态；turn reset
/// 不得吞掉迟到内容，父 `CurrentTurn` 仍保持为空。
#[test]
#[serial]
fn test_background_child_started_before_task_row_keeps_late_preview() {
    use crate::kit::atoms::{BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS};
    use crate::kit::stream_data::TuiTextChunk;

    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
    crate::kit::atoms::BG_TASK_REVISION.set(None);
    let mut state = make_state(SessionPhase::PromptRunning, 0);
    let child_text = |text: &str, message_id: &str| {
        AcpEventData::TextChunk(TuiTextChunk {
            text: text.into(),
            message_id: Some(message_id.into()),
            agent_id: Some("bg-early".into()),
        })
    };

    // 1) 后台 child 先启动；此时还没有任何任务行。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "bg-early".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );

    // 2) 父 turn 在任务行到达前结束：父正文归档，CurrentTurn 清空。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "parent".into(),
            message_id: Some("parent-msg".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(
        state.current_turn.is_empty(),
        "turn 边界必须清空父 CurrentTurn（后台 child 不阻塞归档）"
    );

    // 3) 任务行之前到达的 child 预览不能丢。
    dispatch_and_notify(&mut state, &child_text("early", "child-msg"));

    // 4) 任务行迟到：占位 owner 绑定到真实 task 并接管预览。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "task-late".into(),
            kind: "agent".into(),
            summary: "worker".into(),
            started_at: String::new(),
            pid: None,
            revision: None,
            status: Some("running".into()),
            child_thread_id: Some("bg-early".into()),
        }),
    );

    // 5) 绑定后的迟到文本与终态。
    dispatch_and_notify(&mut state, &child_text("-late", "child-msg"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "bg-early".into(),
            result: "done".into(),
            is_error: false,
        },
    );

    assert!(
        state.current_turn.is_empty(),
        "迟到 child 事件不得重新占用父 CurrentTurn"
    );
    assert!(
        !state.committed.iter().any(|unit| matches!(
            unit,
            TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text.contains("early")
        )),
        "child 正文不得进入主回复"
    );

    let live_store = BG_LIVE_DETAIL.state();
    let live = live_store.read();
    assert!(
        !live.contains_key("bg-early"),
        "绑定后占位 owner 必须移交给真实 task"
    );
    let detail = live.get("task-late").expect("bound live detail");
    let live_texts: Vec<String> = detail
        .nested_units
        .iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        live_texts,
        vec!["early-late".to_string()],
        "任务行到达前后的 child 预览都要保留"
    );
    assert_eq!(detail.status, crate::kit::atoms::BgLiveStatus::Succeeded);
    assert_eq!(detail.subagent_result.as_deref(), Some("done"));
    drop(live);

    let display = BG_DISPLAY.state();
    let task = display
        .read()
        .iter()
        .find(|row| row.id == "task-late")
        .cloned()
        .expect("task row");
    assert_eq!(
        task.linked_agent_id.as_deref(),
        Some("bg-early"),
        "迟到的任务行必须认领对应 child"
    );

    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
}

/// [回归 / CodeRabbit Major] 任务行乱序到达时按 child_thread_id 精确配对，
/// 不能按占位创建顺序把 A 的预览挂到 B 的行上。
#[test]
#[serial]
fn test_reordered_task_rows_bind_by_child_thread_identity() {
    use crate::kit::atoms::{BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS};
    use crate::kit::stream_data::TuiTextChunk;

    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
    crate::kit::atoms::BG_TASK_REVISION.set(None);
    let mut state = make_state(SessionPhase::PromptRunning, 0);
    let child_text = |agent_id: &str, text: &str| {
        AcpEventData::TextChunk(TuiTextChunk {
            text: text.into(),
            message_id: Some(format!("{agent_id}-msg")),
            agent_id: Some(agent_id.into()),
        })
    };
    let task_started = |task_id: &str, child_thread_id: &str| {
        AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: task_id.into(),
            kind: "agent".into(),
            summary: "worker".into(),
            started_at: String::new(),
            pid: None,
            revision: None,
            status: Some("running".into()),
            child_thread_id: Some(child_thread_id.into()),
        })
    };

    for agent in ["child-a", "child-b"] {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::SubagentStarted {
                agent_id: agent.into(),
                agent_name: "coder".into(),
                is_background: true,
                parent_tool_call_id: None,
            },
        );
    }
    dispatch_and_notify(&mut state, &child_text("child-a", "body-a"));
    dispatch_and_notify(&mut state, &child_text("child-b", "body-b"));

    // 任务行按相反顺序到达：B 的行先出现，A 的行后出现。
    dispatch_and_notify(&mut state, &task_started("task-b", "child-b"));
    dispatch_and_notify(&mut state, &task_started("task-a", "child-a"));

    let links: Vec<(String, Option<String>)> = BG_DISPLAY
        .state()
        .read()
        .iter()
        .map(|row| (row.id.clone(), row.linked_agent_id.clone()))
        .collect();
    assert!(
        links.contains(&("task-a".into(), Some("child-a".into()))),
        "A 的行必须绑定 child-a，实际: {links:?}"
    );
    assert!(
        links.contains(&("task-b".into(), Some("child-b".into()))),
        "B 的行必须绑定 child-b，实际: {links:?}"
    );

    crate::kit::bg_task_live::publish_pending_streams();
    let live_store = BG_LIVE_DETAIL.state();
    let live = live_store.read();
    assert!(!live.contains_key("child-a") && !live.contains_key("child-b"));
    let text = |task_id: &str| {
        live[task_id]
            .nested_units
            .iter()
            .filter_map(|unit| match unit {
                TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.clone()),
                _ => None,
            })
            .collect::<String>()
    };
    assert_eq!(text("task-a"), "body-a", "预览不得串到 B 的任务行");
    assert_eq!(text("task-b"), "body-b", "预览不得串到 A 的任务行");
    drop(live);

    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
}

/// [回归 / CodeRabbit Minor] resume 复用 agent_id 时，当前 occurrence 的占位
/// owner 必须优先于旧任务行/旧 identity，旧终态明细不能吞掉新一轮文本。
#[test]
#[serial]
fn test_resumed_background_occurrence_uses_current_pending_owner() {
    use crate::kit::atoms::{BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS};
    use crate::kit::stream_data::TuiTextChunk;

    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
    crate::kit::atoms::BG_TASK_REVISION.set(None);
    let mut state = make_state(SessionPhase::PromptRunning, 0);
    let child_text = |text: &str| {
        AcpEventData::TextChunk(TuiTextChunk {
            text: text.into(),
            message_id: Some("child-msg".into()),
            agent_id: Some("child-x".into()),
        })
    };
    let task_started = |task_id: &str| {
        AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: task_id.into(),
            kind: "agent".into(),
            summary: "worker".into(),
            started_at: String::new(),
            pid: None,
            revision: None,
            status: Some("running".into()),
            child_thread_id: Some("child-x".into()),
        })
    };

    // 旧 occurrence：任务行与 child 正常配对并完成（行保留在显示区，BG_TASKS 移除）。
    dispatch_and_notify(&mut state, &task_started("task-old"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "child-x".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(&mut state, &child_text("old-body"));
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "child-x".into(),
            result: "old done".into(),
            is_error: false,
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskCompleted {
            task_id: "task-old".into(),
            kind: Some("agent".into()),
            success: true,
            duration_ms: 10,
            output_preview: None,
            revision: None,
        },
    );
    assert!(
        BG_TASKS
            .state()
            .read()
            .iter()
            .all(|task| task.task_id != "task-old"),
        "旧任务应从活动任务中移除"
    );

    // resume：同一 agent_id 的新 occurrence，任务行尚未出现。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "child-x".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(&mut state, &child_text("new-body"));
    crate::kit::bg_task_live::publish_pending_streams();

    {
        let live_store = BG_LIVE_DETAIL.state();
        let live = live_store.read();
        let pending = live
            .get("child-x")
            .expect("current occurrence pending owner");
        assert_eq!(pending.status, crate::kit::atoms::BgLiveStatus::Running);
        let pending_text: String = pending
            .nested_units
            .iter()
            .filter_map(|unit| match unit {
                TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(pending_text, "new-body", "新一轮文本必须进入当前占位 owner");
        assert_eq!(
            live["task-old"].status,
            crate::kit::atoms::BgLiveStatus::Succeeded,
            "旧任务明细保持旧终态"
        );
    }

    // 新任务行到达：占位 owner 精确移交。
    dispatch_and_notify(&mut state, &task_started("task-new"));
    assert!(!BG_LIVE_DETAIL.state().read().contains_key("child-x"));
    assert_eq!(
        BG_LIVE_DETAIL.state().read()["task-new"].status,
        crate::kit::atoms::BgLiveStatus::Running
    );
    assert_eq!(
        BG_DISPLAY
            .state()
            .read()
            .iter()
            .find(|row| row.id == "task-new")
            .and_then(|row| row.linked_agent_id.clone())
            .as_deref(),
        Some("child-x")
    );

    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
}

/// [回归 / CodeRabbit Minor] Stop 早于任务行时，终态预览仍要在行到达后移交，
/// 不能因为 child 已离开 BG_AGENT_IDS 就留在无法关联的占位 owner。
#[test]
#[serial]
fn test_background_child_stop_before_task_row_still_attaches_terminal_preview() {
    use crate::kit::atoms::{BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS};
    use crate::kit::stream_data::TuiTextChunk;

    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
    crate::kit::atoms::BG_TASK_REVISION.set(None);
    let mut state = make_state(SessionPhase::PromptRunning, 0);

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "child-early".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "early-body".into(),
            message_id: Some("early-msg".into()),
            agent_id: Some("child-early".into()),
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "child-early".into(),
            result: "early done".into(),
            is_error: false,
        },
    );

    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "task-late".into(),
            kind: "agent".into(),
            summary: "worker".into(),
            started_at: String::new(),
            pid: None,
            revision: None,
            status: Some("running".into()),
            child_thread_id: Some("child-early".into()),
        }),
    );

    assert!(!BG_LIVE_DETAIL.state().read().contains_key("child-early"));
    let live_store = BG_LIVE_DETAIL.state();
    let live = live_store.read();
    let detail = live.get("task-late").expect("terminal preview attached");
    assert_eq!(detail.status, crate::kit::atoms::BgLiveStatus::Succeeded);
    assert_eq!(detail.subagent_result.as_deref(), Some("early done"));
    let texts: String = detail
        .nested_units
        .iter()
        .filter_map(|unit| match unit {
            TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, "early-body");
    drop(live);

    assert_eq!(
        BG_DISPLAY
            .state()
            .read()
            .iter()
            .find(|row| row.id == "task-late")
            .and_then(|row| row.linked_agent_id.clone())
            .as_deref(),
        Some("child-early")
    );

    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
}
