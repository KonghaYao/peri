use super::*;

#[test]
#[serial]
fn test_todo_snapshot_advances_only_after_successful_tool_end() {
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
    };

    let start = |id: &str, status: &str| {
        AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: id.into(),
            tool_name: "TodoWrite".into(),
            input_summary: "todos: 1".into(),
            raw_input: json!({
                "todos": [{"content": "任务", "status": status}]
            }),
            agent_id: None,
        })
    };
    let end = |id: &str, is_error: bool| {
        AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
            tool_id: id.into(),
            output_summary: "+[0]".into(),
            is_error,
            agent_id: None,
        })
    };

    dispatch_and_notify(&mut state, &start("todo-1", "pending"));
    dispatch_and_notify(&mut state, &end("todo-1", false));
    assert!(state.last_successful_todos.is_some());

    dispatch_and_notify(&mut state, &start("todo-2", "completed"));
    let TuiToolPresentation::Todo(second) = &state.current_turn.tool_cards[1].presentation else {
        panic!("expected semantic todo card");
    };
    assert_eq!(second.changes[0].kind, TuiTodoChangeKind::Completed);
    dispatch_and_notify(&mut state, &end("todo-2", true));

    dispatch_and_notify(&mut state, &start("todo-3", "in_progress"));
    let TuiToolPresentation::Todo(third) = &state.current_turn.tool_cards[2].presentation else {
        panic!("expected semantic todo card");
    };
    assert_eq!(
        third.changes[0].kind,
        TuiTodoChangeKind::Started,
        "失败的 TodoWrite 不得覆盖最近成功快照"
    );
}

#[test]
#[serial]
fn test_duplicate_todo_end_cannot_roll_back_newer_successful_snapshot() {
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
    };
    let start = |id: &str, status: &str| {
        AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: id.into(),
            tool_name: "TodoWrite".into(),
            input_summary: "todos: 1".into(),
            raw_input: json!({"todos": [{"content": "任务", "status": status}]}),
            agent_id: None,
        })
    };
    let end = |id: &str| {
        AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
            tool_id: id.into(),
            output_summary: "saved".into(),
            is_error: false,
            agent_id: None,
        })
    };

    dispatch_and_notify(&mut state, &start("todo-a", "pending"));
    dispatch_and_notify(&mut state, &end("todo-a"));
    dispatch_and_notify(&mut state, &start("todo-b", "completed"));
    dispatch_and_notify(&mut state, &end("todo-b"));
    dispatch_and_notify(&mut state, &end("todo-a"));
    dispatch_and_notify(&mut state, &start("todo-c", "in_progress"));

    let TuiToolPresentation::Todo(todo) = &state.current_turn.tool_cards[2].presentation else {
        panic!("expected semantic Todo card");
    };
    assert_eq!(
        todo.changes[0].kind,
        TuiTodoChangeKind::Reopened,
        "重复的旧结束事件不得把成功快照从 todo-b 回退到 todo-a"
    );
}

#[test]
#[serial]
fn test_replay_skill_card_hides_raw_skill_output() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::ReplayingHistory,
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
    };

    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReplayToolStarted {
            tool_id: "skill-replay".into(),
            tool_name: "Skill".into(),
            input_summary: "skill: using-superpowers".into(),
            raw_input: json!({"skill": "using-superpowers"}),
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReplayToolEnded {
            tool_id: "skill-replay".into(),
            output_summary: "---\nname: using-superpowers\n---\nfull SKILL.md body".into(),
            is_error: false,
        },
    );

    let TuiRenderUnit::TuiToolCard(card) = &state.committed[0] else {
        panic!("expected replay ToolCard");
    };
    assert!(matches!(
        &card.presentation,
        TuiToolPresentation::Skill(skill) if skill.name == "using-superpowers"
    ));
    assert!(
        card.output_summary.contains("full SKILL.md body"),
        "回放保留原始输出，但 Skill 专属 renderer 必须隐藏它"
    );
}

#[test]
#[serial]
fn test_later_started_todo_wins_when_successful_ends_arrive_out_of_order() {
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
    };
    let start = |id: &str, status: &str| {
        AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            tool_id: id.into(),
            tool_name: "TodoWrite".into(),
            input_summary: "todos: 1".into(),
            raw_input: json!({"todos": [{"content": "任务", "status": status}]}),
            agent_id: None,
        })
    };
    let end = |id: &str| {
        AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
            tool_id: id.into(),
            output_summary: "saved".into(),
            is_error: false,
            agent_id: None,
        })
    };

    dispatch_and_notify(&mut state, &start("todo-a", "pending"));
    dispatch_and_notify(&mut state, &start("todo-b", "completed"));
    dispatch_and_notify(&mut state, &end("todo-b"));
    dispatch_and_notify(&mut state, &end("todo-a"));
    dispatch_and_notify(&mut state, &start("todo-c", "in_progress"));

    let TuiToolPresentation::Todo(todo) = &state.current_turn.tool_cards[2].presentation else {
        panic!("expected semantic Todo card");
    };
    assert_eq!(
        todo.changes[0].kind,
        TuiTodoChangeKind::Reopened,
        "较早启动的 Todo 晚到成功结束时不得回退较新成功基线"
    );
}
