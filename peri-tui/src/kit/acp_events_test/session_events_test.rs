use super::*;

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

/// H3: RewindCompleted 反序列化 messages_json 替换 state.committed，
/// 并同步重建 REWIND_PREVIEW（支持 rewind 后连续回滚）。
#[test]
#[serial]
fn test_rewind_completed_replaces_committed() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    // 预置 committed 旧数据
    let pre_existing = im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
        "old".into(),
    ))]);
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

    let messages_json = serde_json::json!([
        {"role": "user", "id": "msg-1", "content": "rewound user msg"},
        {"role": "assistant", "id": "msg-2", "content": [{"type": "text", "text": "rewound assistant"}]}
    ])
    .to_string();

    dispatch_and_notify(&mut state, &AcpEventData::RewindCompleted { messages_json });

    // committed 应被替换为 2 条（user + assistant）
    assert_eq!(state.committed.len(), 2, "rewind 后 committed 应有 2 条 VM");
    match &state.committed[0] {
        TuiRenderUnit::TuiUserBubble(d) => assert_eq!(d.text, "rewound user msg"),
        other => panic!("expected TuiUserBubble, got {other:?}"),
    }
    match &state.committed[1] {
        TuiRenderUnit::TuiAssistantBubble(d) => assert_eq!(d.text, "rewound assistant"),
        other => panic!("expected TuiAssistantBubble, got {other:?}"),
    }

    // H4: REWIND_PREVIEW 应重建为回滚后的消息列表（id/role/preview），
    // 保证连续第二次回滚的目标 id 有效。
    // P1：重建只含 user 消息、最新在前（与 rewind-candidates 口径一致）
    let preview = crate::kit::atoms::REWIND_PREVIEW.state().read().clone();
    let preview = preview.expect("rewind 后 REWIND_PREVIEW 应被重建");
    assert_eq!(
        preview.messages.len(),
        1,
        "preview 只含 user 候选（assistant 被过滤）"
    );
    assert_eq!(preview.messages[0].id, "msg-1");
    assert_eq!(preview.messages[0].role, "user");
    assert_eq!(preview.messages[0].preview, "rewound user msg");
}

/// H4-b: RewindCompleted 重建 preview 时剥离 `<system-reminder>` 注入块——
/// 带尾部注入的用户输入保留（与服务端 rewind-candidates 口径一致），
/// 纯系统注入消息不进候选。
#[test]
#[serial]
fn test_rewind_completed_rebuild_preview_strips_reminder() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = BridgeState {
        variant: 1,
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
    };
    let messages_json = serde_json::json!([
        {
            "role": "user",
            "id": "msg-1",
            "content": "请用 Write 工具创建文件\n<system-reminder>\nCurrent permission mode: Bypass: All tool calls are allowed without approval.\n</system-reminder>",
        },
        {
            "role": "user",
            "id": "msg-2",
            "content": "<system-reminder>后台任务完成通知</system-reminder>",
        },
    ])
    .to_string();
    dispatch_and_notify(&mut state, &AcpEventData::RewindCompleted { messages_json });

    let preview = crate::kit::atoms::REWIND_PREVIEW.state().read().clone();
    let preview = preview.expect("rewind 后 REWIND_PREVIEW 应被重建");
    assert_eq!(
        preview.messages.len(),
        1,
        "带尾部注入的用户消息剥离后保留；纯注入消息丢弃"
    );
    assert_eq!(preview.messages[0].id, "msg-1");
    assert_eq!(
        preview.messages[0].preview, "请用 Write 工具创建文件",
        "preview 不得携带 system-reminder 注入文本"
    );
}

/// RewindCompleted 后：目标文本回填输入框（INPUT_RESTORE_TEXT）并触发心跳。
#[test]
#[serial]
fn test_rewind_completed_restores_target_text_to_input() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    // 清空回填通道残留（OnceLock 全局单例，InputArea effect 的 take 在测试中不执行）
    if let Some(mu) = crate::kit::atoms::INPUT_RESTORE_TEXT.get() {
        mu.lock().take();
    }
    // 预置 REWIND_TARGET_TEXT（候选 Enter 时暂存）
    *crate::kit::atoms::REWIND_TARGET_TEXT.state().write() = Some("需要重新编辑的问题".to_string());
    let hb_before = *crate::kit::atoms::RENDER_HEARTBEAT.state().read();

    let mut state = BridgeState {
        variant: 1,
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
    };
    let messages_json = serde_json::json!([
        {"role": "user", "id": "msg-1", "content": "历史用户消息"},
    ])
    .to_string();
    dispatch_and_notify(&mut state, &AcpEventData::RewindCompleted { messages_json });

    // 回填通道被写入
    let restore = crate::kit::atoms::INPUT_RESTORE_TEXT
        .get()
        .and_then(|mu| mu.lock().clone());
    assert_eq!(restore.as_deref(), Some("需要重新编辑的问题"));
    // 心跳递增触发 InputArea use_effect
    assert!(
        *crate::kit::atoms::RENDER_HEARTBEAT.state().read() > hb_before,
        "回填必须触发 RENDER_HEARTBEAT"
    );
    // 消费后清空暂存
    assert!(
        crate::kit::atoms::REWIND_TARGET_TEXT
            .state()
            .read()
            .is_none(),
        "REWIND_TARGET_TEXT 消费后应清空"
    );
}

/// RewindCompleted 无目标文本（如直接执行路径异常）时不写回填。
#[test]
#[serial]
fn test_rewind_completed_without_target_text_no_restore() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    *crate::kit::atoms::REWIND_TARGET_TEXT.state().write() = None;
    // 清空回填通道残留（OnceLock 全局单例）
    if let Some(mu) = crate::kit::atoms::INPUT_RESTORE_TEXT.get() {
        mu.lock().take();
    }

    let mut state = BridgeState {
        variant: 1,
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
    };
    let messages_json = serde_json::json!([]).to_string();
    dispatch_and_notify(&mut state, &AcpEventData::RewindCompleted { messages_json });

    let restore = crate::kit::atoms::INPUT_RESTORE_TEXT
        .get()
        .and_then(|mu| mu.lock().clone());
    assert!(restore.is_none(), "无目标文本不应触发回填");
}

/// 跨 turn 场景：第一轮 reasoning 在 committed 中保留，第二轮为最后一个展开。
#[test]
#[serial]
fn test_multi_turn_reasoning_preserved_in_committed() {
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

    // === Turn 1: user bubble, reasoning + text → TurnDone ===
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "第一个问题".into(),
        },
    );
    dispatch_and_notify(&mut state, &AcpEventData::PromptStarted);
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(crate::kit::stream_data::TuiReasoningChunk {
            text: "Turn 1 的思考内容".into(),
            message_id: Some("msg_1".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "Turn 1 的回复".into(),
            message_id: Some("msg_1".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    // === Turn 2: user bubble, reasoning + text → TurnDone ===
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "第二个问题".into(),
        },
    );
    dispatch_and_notify(&mut state, &AcpEventData::PromptStarted);
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(crate::kit::stream_data::TuiReasoningChunk {
            text: "Turn 2 的思考内容".into(),
            message_id: Some("msg_2".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "Turn 2 的回复".into(),
            message_id: Some("msg_2".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    // 验证 committed 有 4 个 VM：User1, Assistant1, User2, Assistant2
    assert_eq!(
        state.committed.len(),
        4,
        "committed 应有 User1 + Assistant1 + User2 + Assistant2 = 4 个 VM"
    );

    // Turn 1 Assistant 应有 reasoning
    let user1 = &state.committed[0];
    assert!(
        matches!(user1, TuiRenderUnit::TuiUserBubble(_)),
        "committed[0] 应为 TuiUserBubble，实际是 {user1:?}"
    );
    match &state.committed[1] {
        TuiRenderUnit::TuiAssistantBubble(d) => {
            assert!(d.reasoning.is_some(), "Turn 1 Assistant 应有 reasoning 块");
            assert_eq!(d.reasoning.as_ref().unwrap().text, "Turn 1 的思考内容");
        }
        other => panic!("expected TuiAssistantBubble at [1], got {other:?}"),
    }

    // Turn 2 Assistant 应有 reasoning
    let user2 = &state.committed[2];
    assert!(
        matches!(user2, TuiRenderUnit::TuiUserBubble(_)),
        "committed[2] 应为 TuiUserBubble，实际是 {user2:?}"
    );
    match &state.committed[3] {
        TuiRenderUnit::TuiAssistantBubble(d) => {
            assert!(d.reasoning.is_some(), "Turn 2 Assistant 应有 reasoning 块");
            assert_eq!(d.reasoning.as_ref().unwrap().text, "Turn 2 的思考内容");
        }
        other => panic!("expected TuiAssistantBubble at [3], got {other:?}"),
    }

    // 验证 VIEW_MODELS snapshot
    let snapshot = VIEW_MODELS.state().read().clone();
    assert_eq!(snapshot.items.len(), 4);

    // [Slice 2] 折叠语义重定义：TurnDone 后 phase 离开 PromptRunning →
    // 全部 reasoning 状态 Completed → spec §7 表折叠为单行（Collapsed）。
    // 旧语义"仅最后一个 reasoning 展开"已由状态机取代。
    for (idx, label) in [(1usize, "Turn 1"), (3usize, "Turn 2")] {
        match &snapshot.items[idx] {
            TuiRenderUnit::TuiAssistantBubble(d) => {
                let r = d.reasoning.as_ref().unwrap();
                assert_eq!(
                    r.status,
                    crate::kit::tui_render_unit::EntryStatus::Completed,
                    "{label} reasoning 应已完成（Completed）"
                );
                assert!(
                    !r.is_running,
                    "{label} reasoning 不应再流式（is_running=false）"
                );
                assert!(
                    r.collapsed(),
                    "{label} reasoning 应折叠（fold=Collapsed，spec §7 completed 行）"
                );
            }
            other => panic!("expected TuiAssistantBubble at snapshot[{idx}], got {other:?}"),
        }
    }
}

/// C2: compact 完成后 TurnDone 触发 session/load 重放。
///
/// 场景 A：命令 compact（Immediate）后无流事件 → 触发 THREAD_LOAD_TX。
/// 场景 B：agent 内部 auto-compact 后有后续流事件 → 标志被清除，不触发。
///
/// 注：THREAD_LOAD_TX 是 OnceLock，两场景合并为单测试以避免 set 冲突。
#[test]
#[serial]
fn test_compact_turndone_reload() {
    use tokio::sync::mpsc;

    // ── 场景 A：命令 compact → 触发 reload ──────────────────────────
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();

    let (tx_a, mut rx_a) = mpsc::unbounded_channel::<String>();
    let _ = THREAD_LOAD_TX.set(tx_a);

    let mut state = BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
        popup_kind: None,
        generation: 0,
        active_session_id: "test-session".to_string(),
        compact_just_completed: true,
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

    // Phase 5 Step 7 补遗（Step 8 回归修复）：manual compact 场景的 UiOnly
    // CommandFeedback 写入 PENDING_COMPACT_NOTE（跨 replay 存活桥接，
    // 沿袭 aecc2834；replay reset 清空 committed 后由 acp_bridge reset 分支
    // 重建 SystemNote 到 current_turn）。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::CommandFeedback(TuiCommandFeedback {
            level: FeedbackLevel::Info,
            message: "已压缩 2 条消息".into(),
            channel: FeedbackChannel::UiOnly,
        }),
    );
    assert_eq!(
        crate::kit::atoms::PENDING_COMPACT_NOTE
            .state()
            .read()
            .clone(),
        Some("已压缩 2 条消息".to_string()),
        "场景 A: manual compact 的 UiOnly CommandFeedback 应写入 PENDING_COMPACT_NOTE"
    );

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    let received = rx_a.try_recv().ok();
    assert_eq!(
        received.as_deref(),
        Some("test-session"),
        "场景 A: THREAD_LOAD_TX 应收到 session_id"
    );
    assert!(!state.compact_just_completed, "场景 A: flag 应清除"); // ── 场景 B：agent 内部 compact → 不触发 reload ──────────────────
    // S4.1 红测试改造：按真实时序（CompactCompleted → 流事件 → TurnDone）
    // 构造，并补 rx 空断言。修复前流事件不清除 compact_just_completed 标志
    // → TurnDone 误发送（rx 非空，测试红）；修复后标志被流事件清除 → rx 空。
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();

    let mut state = BridgeState {
        variant: 1,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::PromptRunning,
        popup_kind: None,
        generation: 0,
        active_session_id: "test-session".to_string(),
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

    // ① CompactCompleted（auto）：不置标志（S4.1 方案 A——服务端透传 trigger，
    //    auto compact 不置位，zero-output 后重放旧消息的边缘洞根治）。
    //    同时注入 SystemNote（current_turn 非空）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::CompactCompleted {
            summary: "compact summary".into(),
            messages_json: String::new(),
            trigger: "auto".into(),
        },
    );
    assert!(
        !state.compact_just_completed,
        "场景 B: auto compact 后标志不应置位（方案 A）"
    );

    // ② agent 继续产出——流事件到达（标志从未置位，防御逻辑无操作）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "agent response after compact".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    assert!(
        !state.compact_just_completed,
        "场景 B: 流事件到达后标志仍不应置位"
    );

    // ③ turn 结束——不得触发 session/load 重放
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    assert!(!state.compact_just_completed, "场景 B: flag 应清除");
    assert!(
        rx_a.try_recv().is_err(),
        "场景 B: THREAD_LOAD_TX 不应收到消息（auto-compact 不触发重放）"
    );

    // ── 场景 B 补遗（Step 8）：auto compact 场景（标志未置位）的 UiOnly
    // CommandFeedback 不得写 PENDING_COMPACT_NOTE——无 replay 触发，避免
    // 残留串到后续 thread 切换的 reset。
    crate::kit::atoms::PENDING_COMPACT_NOTE.set(None);
    dispatch_and_notify(
        &mut state,
        &AcpEventData::CommandFeedback(TuiCommandFeedback {
            level: FeedbackLevel::Info,
            message: "已压缩 2 条消息".into(),
            channel: FeedbackChannel::UiOnly,
        }),
    );
    assert!(
        crate::kit::atoms::PENDING_COMPACT_NOTE
            .state()
            .read()
            .is_none(),
        "场景 B: auto compact 的 CommandFeedback 不应写 PENDING_COMPACT_NOTE"
    );

    // ── 场景 B2：manual compact + 流事件（方案 B 防御路径保留）────────
    // manual compact 置位后，若 agent 继续产出流事件（理论上 manual 是
    // Immediate 命令无流事件，但防御逻辑保留），标志被清除，TurnDone
    // 不触发 reload。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::CompactCompleted {
            summary: "manual compact summary".into(),
            messages_json: String::new(),
            trigger: "manual".into(),
        },
    );
    assert!(
        state.compact_just_completed,
        "场景 B2: manual compact 后标志应置位"
    );
    // B2 补充（issue 2026-08-08-e2e-compact-command-screenshot-too-early）：
    // Phase 5 Step 7 文案移交 CommandFeedback 渲染后，SystemNote 注入职责
    // 由 handle_command_feedback 承担（manual 场景写入 PENDING_COMPACT_NOTE
    // 跨 replay 存活——见场景 A/B 补遗断言）；此处仅保留标志链断言。

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "defensive stream after manual compact".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    assert!(
        !state.compact_just_completed,
        "场景 B2: 流事件到达后标志应清除（方案 B 防御）"
    );

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(!state.compact_just_completed, "场景 B2: flag 应清除");
    assert!(
        rx_a.try_recv().is_err(),
        "场景 B2: 防御路径下也不应触发重放"
    );
}
