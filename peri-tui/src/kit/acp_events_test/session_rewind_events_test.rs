//! RewindCompleted 与跨 turn reasoning 视图用例（会话历史重建）。

use super::*;

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
        child_runs: Default::default(),
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
        child_runs: Default::default(),
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
        child_runs: Default::default(),
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
        child_runs: Default::default(),
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
        child_runs: Default::default(),
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
    };

    // === Turn 1: user bubble, reasoning + text → TurnDone ===
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "第一个问题".into(),
            attachments: Vec::new(),
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
            attachments: Vec::new(),
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
    assert_eq!(snapshot.items.len(), 5);
    assert!(matches!(snapshot.items[2], TuiRenderUnit::TuiDivider(_)));

    // [Slice 2] 折叠语义重定义：TurnDone 后 phase 离开 PromptRunning →
    // 全部 reasoning 状态 Completed → spec §7 表折叠为单行（Collapsed）。
    // 旧语义"仅最后一个 reasoning 展开"已由状态机取代。
    for (idx, label) in [(1usize, "Turn 1"), (4usize, "Turn 2")] {
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
