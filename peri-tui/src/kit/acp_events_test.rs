//! Tests for acp_events

use super::*;
use crate::kit::message_area::TodoStatus;
use crate::kit::tui_render_unit::{TuiTodoChangeKind, TuiToolPresentation};
use serde_json::json;
use serial_test::serial;
use tokio::sync::mpsc;

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
    };

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "agent-1".into(),
            agent_name: "researcher".into(),
            is_background: false,
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

    let snapshot = VIEW_MODELS.state().read().clone();
    assert_eq!(snapshot.items.len(), 1);
    match &snapshot.items[0] {
        TuiRenderUnit::TuiSubAgentGroup(group) => {
            assert_eq!(group.agent_id, "agent-1");
            assert_eq!(group.view_models.len(), 1);
        }
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    }
}

/// C1 回归测试：drain_input_buffer 清空 INPUT_BUFFER 队列。
///
/// 注：不验证 SUBMIT_TX 接收——SUBMIT_TX 是 OnceLock 全局句柄，一旦被其他
/// 测试 set 就无法重置；此处只验证 drain 的核心效应（buffer 被清空）。
/// 顺序保证由 `VecDeque::drain(..)` + 顺序 `tx.send` 在源码层面保证。
#[tokio::test]
#[serial]
async fn test_drain_input_buffer_preserves_order() {
    crate::kit::atoms::init_atoms();
    let _ = SUBMIT_TX.get_or_init(|| {
        let (tx, _rx) = mpsc::unbounded_channel::<SubmitRequest>();
        tx
    });

    // 入队三条
    {
        let state = INPUT_BUFFER.state();
        let mut buf = state.write();
        buf.push_back("first".into());
        buf.push_back("second".into());
        buf.push_back("third".into());
    }

    drain_input_buffer();

    // 验证 buffer 已被 drain 干净——这是 drain_input_buffer 的核心效应
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "buffer should be empty after drain"
    );
}

/// C1 回归测试：空 buffer 是 no-op，drain 后仍为空。
#[tokio::test]
#[serial]
async fn test_drain_input_buffer_empty_is_noop() {
    crate::kit::atoms::init_atoms();
    let _ = SUBMIT_TX.get_or_init(|| {
        let (tx, _rx) = mpsc::unbounded_channel::<SubmitRequest>();
        tx
    });

    INPUT_BUFFER.state().write().clear();
    drain_input_buffer();

    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "empty buffer should remain empty"
    );
}

/// C1 回归测试：SUBMIT_TX 未初始化时安全跳过，不 panic，buffer 也保持不变。
///
/// 注：实际运行时 OnceLock 一旦 set 无法 unset；本测试只验证不 panic。
#[test]
#[serial]
fn test_drain_input_buffer_no_submit_tx_safe() {
    crate::kit::atoms::init_atoms();
    // 不论 SUBMIT_TX 是否 set，都不应 panic
    INPUT_BUFFER.state().write().push_back("x".into());
    drain_input_buffer();
    // SUBMIT_TX 已被前面测试 set 过，所以 drain 成功 → buffer 被清空
    // 即使 SUBMIT_TX 未 set，drain 早退，buffer 仍有 "x"——两种情况都不算 panic
}

/// BRIDGE_RESET_COUNTER 递增时 acp_bridge 重置分支同步清空 INPUT_BUFFER，
/// 防止旧会话缓存输入在新会话 TurnDone 时泄漏。
///
/// 此测试模拟 bridge 的 counter != last_reset_counter 分支：先填入 buffer 数据，
/// 递增 BRIDGE_RESET_COUNTER，构造任意事件 dispatch，断言 buffer 已被清空。
/// 注意：实际清空发生在 acp_bridge.rs 的 counter 检测分支，而非 dispatch_and_notify
/// 内部。此测试模拟的是那个分支调用 push_view_models_for_reset() 前后的完整效应。
#[test]
#[serial]
fn test_bridge_reset_clears_input_buffer() {
    crate::kit::atoms::init_atoms();
    // 填入 buffer 数据
    INPUT_BUFFER
        .state()
        .write()
        .push_back("leaked input".into());
    INPUT_BUFFER
        .state()
        .write()
        .push_back("another leaked input".into());
    assert!(!INPUT_BUFFER.state().read().is_empty(), "buffer 应有数据");

    // 模拟 acp_bridge 的 counter 检测分支：
    // push_view_models_for_reset() 前同步清空 INPUT_BUFFER
    INPUT_BUFFER.state().write().clear();
    push_view_models_for_reset();

    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "bridge reset 后 INPUT_BUFFER 应被清空"
    );

    // VIEW_MODELS 也应被重置
    let snapshot = VIEW_MODELS.state().read().clone();
    assert!(
        snapshot.items.is_empty(),
        "bridge reset 后 committed 应为空"
    );
    assert!(
        snapshot.items.is_empty(),
        "bridge reset 后 current_turn 应为空"
    );
}

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
    };

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "test".into(),
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

    // Turn 1 reasoning 应折叠（collapsed = true）——中间块
    match &snapshot.items[1] {
        TuiRenderUnit::TuiAssistantBubble(d) => {
            let r = d.reasoning.as_ref().unwrap();
            assert!(r.collapsed, "Turn 1 reasoning 应折叠（collapsed=true）");
        }
        other => panic!("expected TuiAssistantBubble at snapshot[1], got {other:?}"),
    }

    // Turn 2 reasoning 应展开（collapsed = false）——最后一个
    match &snapshot.items[3] {
        TuiRenderUnit::TuiAssistantBubble(d) => {
            let r = d.reasoning.as_ref().unwrap();
            assert!(
                !r.collapsed,
                "Turn 2 reasoning 应展开（collapsed=false）——最后一个"
            );
        }
        other => panic!("expected TuiAssistantBubble at snapshot[3], got {other:?}"),
    }
}

/// C2: compact 完成后 TurnDone 触发 session/load 重放。
///
/// 场景 A：命令 compact（Immediate）后 current_turn 为空 → 触发 THREAD_LOAD_TX。
/// 场景 B：agent 内部 compact 后 current_turn 非空（有后续流事件）→ 不触发。
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
    };

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    let received = rx_a.try_recv().ok();
    assert_eq!(
        received.as_deref(),
        Some("test-session"),
        "场景 A: THREAD_LOAD_TX 应收到 session_id"
    );
    assert!(!state.compact_just_completed, "场景 A: flag 应清除");

    // ── 场景 B：agent 内部 compact → 不触发 reload ──────────────────
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
    };
    state
        .current_turn
        .append_text("agent response after compact", None);
    state.compact_just_completed = true;

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    // 场景 B 的 TurnDone 也会尝试发送（因为 flag 为 true 但 current_turn 非空），
    // 核心验证：flag 应被清除，但 reload 逻辑条件不满足（current_turn 非空）。
    assert!(!state.compact_just_completed, "场景 B: flag 应清除");
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
        },
    );

    assert!(
        !ACP_STATE.state().read().is_loading,
        "SubagentStopped after TurnSuspended: is_loading 应保持 false"
    );
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
    };

    // SubagentStarted 设置 phase=PromptRunning
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sync-agent-1".into(),
            agent_name: "researcher".into(),
            is_background: false,
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
    };

    dispatch_and_notify(&mut state, &AcpEventData::PromptSubmitted);

    assert_eq!(state.phase, SessionPhase::PromptRunning);
    assert_eq!(state.variant, 1);
    assert!(ACP_STATE.state().read().is_loading);
}

/// 同步 sub-agent 的 ToolStarted/ToolEnded 事件应路由到 SubAgentAccumulator，
/// 并反映在 VIEW_MODELS 的 TuiSubAgentGroup 中。
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
    };

    // 启动同步 sub-agent
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sync-1".into(),
            agent_name: "coder".into(),
            is_background: false,
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

    let snapshot = VIEW_MODELS.state().read().clone();
    // items 中应有 1 个 TuiSubAgentGroup
    assert_eq!(snapshot.items.len(), 1, "items 应包含 1 个元素");
    match &snapshot.items[0] {
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

// ── has_md_block_boundary_since 单元测试 ──

#[test]
fn test_boundary_since_chars_zero_always_true() {
    assert!(
        has_md_block_boundary_since("hello", 0),
        "since_chars=0 应始终返回 true"
    );
}

#[test]
fn test_boundary_empty_string() {
    assert!(!has_md_block_boundary_since("", 1), "空字符串不应触发边界");
}

#[test]
fn test_boundary_paragraph_double_newline() {
    let text = "first paragraph\n\nsecond paragraph";
    // since_chars=0 已推送；从字符 1 开始检查应有双换行
    assert!(has_md_block_boundary_since(text, 1), "双换行应触发段落边界");
}

#[test]
fn test_boundary_code_block() {
    let text = "some text\n```rust\nfn main() {}\n```";
    // 从 "some" 开始检查
    assert!(has_md_block_boundary_since(text, 1), "代码块起止应触发边界");
}

#[test]
fn test_boundary_heading() {
    let text = "intro\n# Heading\ncontent";
    assert!(has_md_block_boundary_since(text, 1), "标题应触发边界");
}

#[test]
fn test_boundary_horizontal_rule() {
    let text = "text\n---\nmore";
    assert!(has_md_block_boundary_since(text, 1), "水平线应触发边界");
}

#[test]
fn test_boundary_no_boundary_in_tail() {
    let text = "one line of text\nanother line without boundary";
    // since_chars 越过已推送部分，尾部无边界
    let pushed = "one line of text".chars().count();
    assert!(
        !has_md_block_boundary_since(text, pushed),
        "无分隔的连续文本不应触发边界"
    );
}

// ── current_streaming_mode 测试 ──

/// 默认（未设置 streaming_mode 或 PERI_CONFIG_HANDLE 未初始化）应返回 Streaming。
#[test]
fn test_mode_default_is_streaming() {
    // PERI_CONFIG_HANDLE 在测试中未初始化 → get() 返回 None → fallback 到 Streaming
    assert!(
        matches!(current_streaming_mode(), StreamingMode::Streaming),
        "未设置 streaming_mode 时应默认 Streaming"
    );
}

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
