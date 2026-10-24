use super::*;

#[test]
#[serial]
fn test_transcript_publication_preserves_stable_history_and_resets_hint() {
    crate::kit::atoms::init_atoms();
    push_view_models_for_reset();
    let mut state = make_fold_test_state();
    state.phase = SessionPhase::PromptRunning;
    state.committed = (0..256)
        .map(|index| TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!("history-{index}"))))
        .collect();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "first 中文".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    let previous = VIEW_MODELS.state().read().generation;
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "🙂 tail".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    let publication = crate::kit::atoms::TRANSCRIPT_PUBLICATION.get();
    assert_eq!(publication.previous_generation, previous);
    assert_eq!(
        publication.generation,
        VIEW_MODELS.state().read().generation
    );
    assert!(
        publication.changed_from >= 255,
        "unchanged history should not be scanned by render"
    );
    push_view_models_for_reset();
    assert_eq!(
        crate::kit::atoms::TRANSCRIPT_PUBLICATION.get().changed_from,
        0
    );
    assert!(VIEW_MODELS.state().read().items.is_empty());
}

#[test]
#[serial]
fn test_replay_events_defer_publication_until_scheduler_boundary() {
    let mut state = make_fold_test_state();
    state.phase = SessionPhase::ReplayingHistory;

    for text in ["history one", "history two"] {
        let intent = dispatch_for_bridge(
            &mut state,
            &AcpEventData::CommittedAssistantText {
                text: text.into(),
                reasoning: None,
            },
        );
        assert_eq!(intent, PublicationIntent::Deferred);
    }

    assert_eq!(state.committed.len(), 2);
    assert_eq!(state.generation, 0, "逐条 replay 不应完整发布 VIEW_MODELS");

    let intent = dispatch_for_bridge(&mut state, &AcpEventData::SessionReplayDone);
    assert_eq!(intent, PublicationIntent::Published);
    assert_eq!(state.generation, 1, "completion 边界必须发布完整历史");
    assert_eq!(VIEW_MODELS.state().read().items.len(), 2);
}

/// push_view_models 以 BridgeState 为准，不再 fallback 到 atom 旧值。
#[test]
#[serial]
fn test_push_view_models_uses_bridge_state() {
    crate::kit::atoms::init_atoms();
    // atom 中有旧数据
    let old_items = im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
        "old data".into(),
    ))]);
    *VIEW_MODELS.state().write() = ViewModelsSnapshot {
        items: old_items,
        generation: 0,
    };

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

    // push_view_models: 用 BridgeState 数据（空 committed + 空 current_turn）→ 空 items
    push_view_models(&mut state);

    let snapshot = VIEW_MODELS.state().read().clone();
    assert!(
        snapshot.items.is_empty(),
        "push_view_models with empty BridgeState should produce empty items"
    );
}

#[test]
#[serial]
fn test_handle_plan_update_multiple_entries() {
    crate::kit::atoms::init_atoms();
    *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();

    let plan_json = json!({
        "entries": [
            {"content": "Task 1", "status": "in_progress", "priority": "medium"},
            {"content": "Task 2", "status": "pending", "priority": "medium"},
            {"content": "Task 3", "status": "completed", "priority": "medium"}
        ]
    });

    handle_plan_update(&plan_json);

    let items = crate::kit::atoms::TODO_ITEMS.state().read().clone();
    assert_eq!(items.len(), 3, "应包含 3 个条目");
    assert!(matches!(items[0].status, TodoStatus::InProgress));
    assert!(matches!(items[1].status, TodoStatus::Pending));
    assert!(matches!(items[2].status, TodoStatus::Completed));
}

#[test]
#[serial]
fn test_handle_plan_update_empty_entries() {
    crate::kit::atoms::init_atoms();
    *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();

    let plan_json = json!({
        "entries": []
    });

    handle_plan_update(&plan_json);

    let items = crate::kit::atoms::TODO_ITEMS.state().read().clone();
    assert!(items.is_empty(), "空 entries 应产出空列表");
}

#[test]
#[serial]
fn test_handle_plan_update_missing_entries() {
    crate::kit::atoms::init_atoms();
    // 写入一个非空值，确认不被覆盖
    *crate::kit::atoms::TODO_ITEMS.state().write() = vec![crate::kit::message_area::TodoItem {
        status: crate::kit::message_area::TodoStatus::InProgress,
        content: "existing".into(),
    }];

    let plan_json = json!({});
    handle_plan_update(&plan_json);

    // Plan 缺少 entries 字段 → deserialize 失败 → 不覆盖 TODO_ITEMS
    let items = crate::kit::atoms::TODO_ITEMS.state().read().clone();
    assert_eq!(items.len(), 1, "缺少 entries 不应覆盖已有列表");
    assert_eq!(items[0].content, "existing");
}

/// M4: dispatch_and_notify 对 Prediction 事件写入 PREDICTION atom。
#[test]
#[serial]
fn test_prediction_writes_prediction_atom() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    // 预清 PREDICTION
    *PREDICTION.state().write() = PredictionState::default();
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

    use peri_acp_types::event_data::{Prediction, PredictionAction};
    dispatch_and_notify(
        &mut state,
        &AcpEventData::Prediction(Prediction {
            text: "type this".into(),
            actions: vec![
                PredictionAction::Placeholder {
                    text: "type this".into(),
                },
                PredictionAction::Summary {
                    text: "修复了认证问题".into(),
                },
            ],
        }),
    );

    let pred = PREDICTION.state().read().clone();
    assert_eq!(pred.text, "type this");
    assert_eq!(pred.summary.as_deref(), Some("修复了认证问题"));
    assert!(pred.received_at.is_some(), "received_at 应被设置");
}

// 子模块：本文件经 acp_events 聚合根的 `#[path]` 挂载；此路径加载方式下，rustc 不会
// 为聚合根派生子目录，子模块需显式 `#[path]` 指向（与本文件同目录）。
#[path = "session_compact_events_test.rs"]
mod session_compact_events_test;
#[path = "session_rewind_events_test.rs"]
mod session_rewind_events_test;
