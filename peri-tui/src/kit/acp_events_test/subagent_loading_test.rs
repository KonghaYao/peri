use super::*;

#[test]
#[serial]
fn test_dispatch_subagent_streaming_updates_current_turn_group() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
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
    };

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
    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::PromptRunning,
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
    };

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

/// SubagentStopped 在 TurnDone 之后不应重新激活 loading。
/// 场景：bg subagent 在 TurnDone 归档完成后才触发 SubagentStopped，
/// SubagentStopped 不应将 phase 覆盖为 PromptRunning（不再设 is_loading=true）。
#[test]
#[serial]
fn test_subagent_stopped_after_turn_done_does_not_set_loading() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
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
    };

    // 模拟 TurnDone：归档 + 重置 phase/loading
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(
        state.phase,
        SessionPhase::Idle,
        "TurnDone 后 phase 应为 Idle"
    );
    assert!(
        !ACP_STATE.state().read().is_loading,
        "TurnDone 后 is_loading 应为 false"
    );

    // SubagentStopped 到达——不应重新激活 loading
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "bg-agent-1".into(),
            result: String::new(),
            is_error: false,
        },
    );

    assert!(
        !ACP_STATE.state().read().is_loading,
        "SubagentStopped after TurnDone: is_loading 应保持 false"
    );
}

/// SubagentStopped 在 TurnSuspended 之后不应重新激活 loading。
#[test]
#[serial]
fn test_subagent_stopped_after_turn_suspended_does_not_set_loading() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
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
    };

    // 模拟 TurnSuspended：归档 + 重置 phase/loading
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    assert_eq!(
        state.phase,
        SessionPhase::Idle,
        "TurnSuspended 后 phase 应为 Idle"
    );
    assert!(
        !ACP_STATE.state().read().is_loading,
        "TurnSuspended 后 is_loading 应为 false"
    );

    // SubagentStopped 到达——不应重新激活 loading
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "bg-agent-2".into(),
            result: String::new(),
            is_error: false,
        },
    );

    assert!(
        !ACP_STATE.state().read().is_loading,
        "SubagentStopped after TurnSuspended: is_loading 应保持 false"
    );
}

/// [回归] bg subagent 信息外溢：TurnSuspended 后 SubAgentAccumulator 被清除，
/// bg 的 TextChunk/ReasoningChunk 不得回退到主 agent 分支（混入主回复气泡）——
/// 与 tool.rs 的 BG_AGENT_IDS 兜底同口径，命中 bg 集合的 chunk 直接跳过。
#[test]
#[serial]
fn test_bg_subagent_chunk_after_turn_suspended_does_not_leak_to_main() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_AGENT_IDS.state().write().clear();
    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
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
    };

    // bg subagent 启动（注册 BG_AGENT_IDS + current_turn 组）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "bg-agent-1".into(),
            agent_name: "researcher".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    assert!(
        BG_AGENT_IDS.state().read().contains("bg-agent-1"),
        "bg SubagentStarted 应注册 BG_AGENT_IDS"
    );

    // 主 turn 挂起：current_turn.reset() 清除 SubAgentAccumulator（既有语义，
    // flush_current_turn 的 running-subagent 守卫覆盖不到 TurnSuspended）
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    assert!(
        state.current_turn.subagent_ids().is_empty(),
        "TurnSuspended 后 current_turn 组应被清空（前置条件）"
    );

    // bg 继续流式输出——不得外溢到主文本/主推理
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "bg leaked text".into(),
            message_id: None,
            agent_id: Some("bg-agent-1".into()),
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(crate::kit::stream_data::TuiReasoningChunk {
            text: "bg leaked reasoning".into(),
            message_id: None,
            agent_id: Some("bg-agent-1".into()),
        }),
    );
    assert!(
        state.current_turn.text.is_empty(),
        "bg TextChunk 不得进入主 agent 文本（外溢）——实际: {:?}",
        state.current_turn.text
    );
    assert!(
        state.current_turn.reasoning.is_empty(),
        "bg ReasoningChunk 不得进入主 agent 推理（外溢）——实际: {:?}",
        state.current_turn.reasoning
    );

    // 对照组：无组且不在 BG_AGENT_IDS 的 chunk（主 agent 文本）仍正常回退
    // 主分支——修复不得破坏主 agent 回复显示。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "main reply".into(),
            message_id: Some("m1".into()),
            agent_id: Some("main-agent".into()),
        }),
    );
    assert_eq!(
        state.current_turn.text, "main reply",
        "主 agent 文本应正常回退主分支"
    );
    BG_AGENT_IDS.state().write().clear();
}

/// [回归] Issue 2026-08-12：bg subagent 运行期流式事件（TextChunk / ReasoningChunk /
/// ToolStarted / ToolEnded）不得把主 agent 的 phase 从 Idle 拉回 PromptRunning——
/// 主 agent 派发 bg 后已完成回复（TurnSuspended），bg 仍在运行，loading 应保持退出，
/// bg 运行状态由 BG 区域跟踪。主 agent 自身事件不受影响（对照组）。
#[test]
#[serial]
fn test_bg_events_after_turn_suspended_keep_idle_loading() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_AGENT_IDS.state().write().clear();
    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
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
    };

    // 前置：bg 启动（注册 BG_AGENT_IDS + SubAgentGroup）→ 主 turn 挂起
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "bg-agent-1".into(),
            agent_name: "researcher".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    assert_eq!(
        state.phase,
        SessionPhase::Idle,
        "前置：TurnSuspended 后 phase 应为 Idle"
    );

    // bg 运行期流式事件全链路到达——phase 不得离开 Idle，is_loading 保持 false
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "bg text".into(),
            message_id: None,
            agent_id: Some("bg-agent-1".into()),
        }),
    );
    assert_eq!(
        state.phase,
        SessionPhase::Idle,
        "bg TextChunk 不得把 phase 拉回 PromptRunning"
    );
    assert!(
        !ACP_STATE.state().read().is_loading,
        "bg TextChunk 后 is_loading 应保持 false"
    );

    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(crate::kit::stream_data::TuiReasoningChunk {
            text: "bg reasoning".into(),
            message_id: None,
            agent_id: Some("bg-agent-1".into()),
        }),
    );
    assert_eq!(
        state.phase,
        SessionPhase::Idle,
        "bg ReasoningChunk 不得把 phase 拉回 PromptRunning"
    );

    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            agent_id: Some("bg-agent-1".into()),
            tool_name: "Bash".into(),
            tool_id: "bg-tc-1".into(),
            input_summary: "ls".into(),
            raw_input: serde_json::Value::Null,
        }),
    );
    assert_eq!(
        state.phase,
        SessionPhase::Idle,
        "bg ToolStarted 不得把 phase 拉回 PromptRunning"
    );

    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
            agent_id: Some("bg-agent-1".into()),
            tool_id: "bg-tc-1".into(),
            output_summary: "ok".into(),
            is_error: false,
        }),
    );
    assert_eq!(
        state.phase,
        SessionPhase::Idle,
        "bg ToolEnded 不得把 phase 拉回 PromptRunning"
    );
    assert!(
        !ACP_STATE.state().read().is_loading,
        "bg 全链路事件后 is_loading 应保持 false"
    );

    // 对照组 1：主 agent 自身 chunk 仍正常点亮 phase（从 Idle → PromptRunning）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "main reply".into(),
            message_id: Some("m1".into()),
            agent_id: Some("main-agent".into()),
        }),
    );
    assert_eq!(
        state.phase,
        SessionPhase::PromptRunning,
        "主 agent chunk 应恢复 PromptRunning（修复不得误伤主 agent 事件）"
    );

    // 对照组 2：sync subagent（不在 BG_AGENT_IDS）的 chunk 路由后仍保持
    // PromptRunning——bg 判定不得误伤 sync subagent。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sync-1".into(),
            agent_name: "coder".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "sync text".into(),
            message_id: None,
            agent_id: Some("sync-1".into()),
        }),
    );
    assert_eq!(
        state.phase,
        SessionPhase::PromptRunning,
        "sync subagent chunk 应保持 PromptRunning（修复不得误伤 sync）"
    );
    BG_AGENT_IDS.state().write().clear();
}

/// SubagentStarted → SubagentStopped 路径（sync subagent）仍保持 loading。
/// 同步 subagent 的 SubagentStarted 已设 phase=PromptRunning，
/// SubagentStopped 不应破坏此状态。
#[test]
#[serial]
fn test_subagent_stopped_after_subagent_started_keeps_loading() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
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
    };

    // SubagentStarted 设置 phase=PromptRunning
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sync-agent-1".into(),
            agent_name: "researcher".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    assert_eq!(
        state.phase,
        SessionPhase::PromptRunning,
        "SubagentStarted 后 phase 应为 PromptRunning"
    );
    assert!(
        ACP_STATE.state().read().is_loading,
        "SubagentStarted 后 is_loading 应为 true"
    );

    // SubagentStopped 到达——应保持 loading
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "sync-agent-1".into(),
            result: String::new(),
            is_error: false,
        },
    );

    assert!(
        ACP_STATE.state().read().is_loading,
        "SubagentStopped after SubagentStarted: is_loading 应保持 true"
    );
}

/// PromptSubmitted 事件应设 phase=PromptRunning + variant=1，
/// push_acp_state 派生 is_loading=true。
#[test]
#[serial]
fn test_prompt_submitted_sets_loading() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
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
    };

    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("rid-1".into()),
        },
    );

    assert_eq!(state.phase, SessionPhase::PromptRunning);
    assert_eq!(state.variant, 1);
    assert!(ACP_STATE.state().read().is_loading);
    // 返工：PromptSubmitted 记录 current_request_id 与 last_prompt_generation 快照
    assert_eq!(state.current_request_id.as_deref(), Some("rid-1"));
    assert_eq!(state.last_prompt_generation, state.turn_generation);
}

/// 同步 sub-agent 的 ToolStarted/ToolEnded 事件应路由到 SubAgentAccumulator，
/// 并反映在当前 turn 的 TuiSubAgentGroup 中。
#[test]
#[serial]
fn test_dispatch_sync_subagent_tool_routed_to_group() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
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
    };

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

/// S4.2: LocalLoadingReset（cancel / /clear / prompt 失败兜底注入的内部事件）
/// 应将 phase 从 PromptRunning 复位为 Idle 并重推 ACP_STATE——修复
/// cancel_consumer 直接写 ACP_STATE 与 bridge phase 派生不同步（取消后
/// push_acp_state 用 phase 重算 is_loading=true 造成 loading 闪回）。
/// 幂等：phase 非 PromptRunning 时 no-op。
#[test]
#[serial]
fn test_loading_reset_event_resets_phase() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();

    let mut state = BridgeState {
        variant: 1,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
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
    };
    // 前置：PromptSubmitted 使 bridge 进入 PromptRunning，ACP_STATE 派生 loading
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("r1".into()),
        },
    );
    assert!(
        ACP_STATE.state().read().is_loading,
        "前置：PromptSubmitted 后 is_loading=true"
    );

    // 取消：LocalLoadingReset → phase 复位 + is_loading=false
    dispatch_and_notify(&mut state, &AcpEventData::LocalLoadingReset);

    assert_eq!(
        state.phase,
        SessionPhase::Idle,
        "LocalLoadingReset 后 phase 应为 Idle"
    );
    assert!(
        !ACP_STATE.state().read().is_loading,
        "LocalLoadingReset 后 is_loading 应为 false"
    );

    // 幂等：phase 已 Idle 时再次 dispatch 无副作用
    dispatch_and_notify(&mut state, &AcpEventData::LocalLoadingReset);
    assert_eq!(state.phase, SessionPhase::Idle, "幂等：phase 保持 Idle");
    assert!(
        !ACP_STATE.state().read().is_loading,
        "幂等：is_loading 保持 false"
    );
}

/// S4.2 回归：取消（LocalLoadingReset）后，服务端正常收尾的 TurnInterrupted
/// 到达不得把 loading 拉回。修复前 cancel_consumer 只直接写 ACP_STATE、
/// bridge phase 仍为 PromptRunning——TurnInterrupted 的 push_acp_state 会
/// 用 phase 重算 is_loading=true（取消后 loading 闪回 + 提交判定竞态）。
#[test]
#[serial]
fn test_loading_reset_then_turn_interrupted_keeps_idle() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();

    let mut state = BridgeState {
        variant: 1,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
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
    };
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("r1".into()),
        },
    );
    assert!(ACP_STATE.state().read().is_loading);

    // 取消：LocalLoadingReset
    dispatch_and_notify(&mut state, &AcpEventData::LocalLoadingReset);
    assert_eq!(state.phase, SessionPhase::Idle);

    // 服务端取消收尾：TurnInterrupted（request_id 配对 → 非 stale）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user-cancelled".into(),
            request_id: Some("r1".into()),
        },
    );
    assert_eq!(
        state.phase,
        SessionPhase::Idle,
        "取消后 TurnInterrupted 到达：phase 保持 Idle"
    );
    assert!(
        !ACP_STATE.state().read().is_loading,
        "取消后 loading 不得闪回"
    );
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
    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
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
    };

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
    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
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
    };

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
