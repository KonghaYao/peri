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
        pending_cache_usage: None,
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
        pending_cache_usage: None,
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
        pending_cache_usage: None,
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
        pending_cache_usage: None,
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

/// TurnDone 归档时，未收到 ToolEnded 的在途工具卡不得永久 loading（如工具不存在）。
#[test]
#[serial]
fn test_turn_done_archives_orphan_tool_not_running() {
    let mut state = super::make_fold_test_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "ghost".into(),
            tool_name: "NotRegistered".into(),
            input_summary: String::new(),
            raw_input: serde_json::Value::Null,
            agent_id: None,
        }),
    );
    let running = VIEW_MODELS.state().read().clone();
    assert!(
        matches!(&running.items[0], TuiRenderUnit::TuiToolCard(t) if t.is_running),
        "precondition: tool card running before TurnDone"
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let done = VIEW_MODELS.state().read().clone();
    match &done.items[0] {
        TuiRenderUnit::TuiToolCard(t) => {
            assert!(!t.is_running, "TurnDone 归档后工具卡不得继续 loading");
        }
        other => panic!("expected TuiToolCard, got {other:?}"),
    }
}

#[test]
#[serial]
fn test_cache_coverage_uses_latest_sample_once_and_commits_with_turn() {
    let mut state = super::make_fold_test_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "assistant reply".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    for (input, cached) in [
        (100_000, 70_000),
        (101_449, 70_000),
        (102_941, 70_000),
        (104_478, 70_000),
    ] {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::CacheUsageUpdated(Some(CacheUsageSample {
                input_tokens: input,
                cached_tokens: cached,
                request_id: Some("chatcmpl-cache".into()),
            })),
        );
    }

    turn::handle_turn_done(&mut state);
    let notes: Vec<_> = state
        .committed
        .iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiSystemNote(note) => Some(note.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        notes.len(),
        4,
        "each root usage_update should warn when coverage < 80%"
    );
    assert!(notes[3].contains("70000"));
    assert!(notes[3].contains("104478"));
    assert!(notes[3].contains("34478"));
    assert!(notes[3].contains("chatcmpl-cache"));
    assert!(state.current_turn.is_empty());
    assert!(state.pending_cache_usage.is_none());
}

#[test]
#[serial]
fn test_cache_coverage_final_healthy_sample_suppresses_earlier_low_sample() {
    let mut state = super::make_fold_test_state();
    for (input, cached) in [(100, 50), (100, 90)] {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::CacheUsageUpdated(Some(CacheUsageSample {
                input_tokens: input,
                cached_tokens: cached,
                request_id: None,
            })),
        );
    }
    assert_eq!(
        state
            .current_turn
            .view_models()
            .iter()
            .filter(|vm| matches!(vm, TuiRenderUnit::TuiSystemNote(_)))
            .count(),
        1,
        "only the low-coverage step should warn"
    );
    turn::handle_turn_done(&mut state);
    assert!(state.pending_cache_usage.is_none());
}

fn assert_root_cache_clear_suppresses_earlier_low_sample(observation: &str) {
    let mut state = super::make_fold_test_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::CacheUsageUpdated(Some(CacheUsageSample {
            input_tokens: 100,
            cached_tokens: 50,
            request_id: Some("earlier-low".into()),
        })),
    );
    dispatch_and_notify(&mut state, &AcpEventData::CacheUsageUpdated(None));
    assert!(
        state.pending_cache_usage.is_none(),
        "{observation} root observation must clear the earlier low sample"
    );

    turn::finalize_cache_coverage(&mut state, true);
    turn::handle_turn_done(&mut state);
    let note_count = state
        .committed
        .iter()
        .filter(|unit| matches!(unit, TuiRenderUnit::TuiSystemNote(_)))
        .count();
    assert_eq!(
        note_count, 1,
        "{observation}: low sample warns immediately; explicit clear only drops pending"
    );
}

#[test]
#[serial]
fn test_cache_coverage_final_missing_observation_clears_earlier_low_sample() {
    assert_root_cache_clear_suppresses_earlier_low_sample("missing cache usage");
}

#[test]
#[serial]
fn test_cache_coverage_final_zero_observation_clears_earlier_low_sample() {
    assert_root_cache_clear_suppresses_earlier_low_sample("zero cache usage");
}

#[test]
#[serial]
fn test_cache_coverage_final_invalid_observation_clears_earlier_low_sample() {
    assert_root_cache_clear_suppresses_earlier_low_sample("invalid cache usage");
}

#[test]
#[serial]
fn test_cache_coverage_survives_suspend_and_is_overwritten_before_done() {
    let mut state = super::make_fold_test_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::CacheUsageUpdated(Some(CacheUsageSample {
            input_tokens: 100,
            cached_tokens: 40,
            request_id: Some("before-suspend".into()),
        })),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    assert_eq!(
        state
            .pending_cache_usage
            .as_ref()
            .unwrap()
            .request_id
            .as_deref(),
        Some("before-suspend")
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::CacheUsageUpdated(Some(CacheUsageSample {
            input_tokens: 100,
            cached_tokens: 90,
            request_id: Some("after-wake".into()),
        })),
    );
    assert_eq!(
        state
            .pending_cache_usage
            .as_ref()
            .unwrap()
            .request_id
            .as_deref(),
        Some("after-wake")
    );
    let _ = state.pending_cache_usage.take();
    assert!(
        state
            .committed
            .iter()
            .chain(state.current_turn.view_models().iter())
            .any(|vm| matches!(vm, TuiRenderUnit::TuiSystemNote(_))),
        "suspend must not remove an already emitted low-coverage warning"
    );
}
