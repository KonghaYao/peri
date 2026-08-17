use super::*;

#[test]
#[serial]
fn test_two_turn_done_accumulates_committed() {
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
    };

    // 第一轮：stream one text → TurnDone
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "first turn".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    assert_eq!(
        state.committed.len(),
        1,
        "first TurnDone: committed should have 1 VM"
    );

    // 第二轮：stream another text → TurnDone
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "second turn".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    let snapshot = VIEW_MODELS.state().read().clone();
    assert_eq!(
        snapshot.items.len(),
        2,
        "two TurnDones: committed should have 2 VMs"
    );
}

/// TurnDone 归档 assistant VM 到 committed，不再代为搬运 buffered_text。
#[test]
#[serial]
fn test_turndone_archives_assistant_to_committed() {
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
    };

    // 往 current_turn 写入一条 assistant 文本
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "assistant reply".into(),
            message_id: None,
            agent_id: None,
        }),
    );

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    // TurnDone 后 assistant VM 被归档到 committed
    assert_eq!(
        state.committed.len(),
        1,
        "committed 应有 1 个 VM：TuiAssistantBubble"
    );
    match &state.committed[0] {
        TuiRenderUnit::TuiAssistantBubble(d) => assert_eq!(d.text, "assistant reply"),
        other => panic!("expected TuiAssistantBubble at [0], got {other:?}"),
    }
}

/// TurnInterrupted 空 current_turn 不归档
#[test]
#[serial]
fn test_turn_interrupted_empty_skips_archive() {
    crate::kit::atoms::init_atoms();
    // 预置一条 committed 数据
    let pre_existing = im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
        "existing".into(),
    ))]);
    *VIEW_MODELS.state().write() = ViewModelsSnapshot {
        items: pre_existing.clone(),
        generation: 0,
    };
    let mut state = BridgeState {
        variant: 1,
        committed: pre_existing,
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
    };

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "test".into(),
            request_id: None,
        },
    );

    assert_eq!(
        state.committed.len(),
        1,
        "空 current_turn → TurnInterrupted 不应归档，committed 长度不变"
    );
    match &state.committed[0] {
        TuiRenderUnit::TuiUserBubble(d) => assert_eq!(d.text, "existing"),
        other => panic!("committed[0] 应为原始 TuiUserBubble, got {other:?}"),
    }
    assert!(state.current_turn.is_empty(), "current_turn 应已重置");
    assert_eq!(state.phase, SessionPhase::Idle, "phase 应为 Idle");
}

/// 次要项 (b)：TurnDone 后 last_submitted_text 应清空，避免跨 turn 残留
/// 导致后续 stale TurnInterrupted 误删不相关气泡。
#[test]
#[serial]
fn test_turn_done_clears_last_submitted_text() {
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
    };

    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "hello".into(),
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted { request_id: None },
    );
    assert_eq!(state.last_submitted_text.as_deref(), Some("hello"));

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    assert!(
        state.last_submitted_text.is_none(),
        "TurnDone 后 last_submitted_text 应清空（防跨 turn 残留）"
    );
}
