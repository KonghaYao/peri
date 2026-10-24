//! Compact 生命周期用例（完成提示与 TurnDone 保窗语义）。

use super::*;

#[test]
#[serial]
fn test_auto_compact_completed_injects_detailed_system_note() {
    let mut state = BridgeState {
        variant: 0,
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
        pending_cache_usage: None,
        publication_intent: Default::default(),
        folded_history: Default::default(),
    };

    super::super::compact::handle_compact_completed(&mut state, "", "auto", "micro", 7, 2048, 2, 1);

    let notes: Vec<_> = state
        .current_turn
        .view_models()
        .iter()
        .filter_map(|unit| match unit {
            crate::kit::tui_render_unit::TuiRenderUnit::TuiSystemNote(note) => Some(&note.text),
            _ => None,
        })
        .collect();
    assert_eq!(notes.len(), 1);
    for expected in ["7", "2048", "2", "1"] {
        assert!(
            notes[0].contains(expected),
            "compact note 缺少 {expected}: {}",
            notes[0]
        );
    }
    assert!(!state.compact_just_completed);
}

#[test]
#[serial]
fn test_full_compact_completed_shows_unmeasured_token_saving() {
    let mut state = BridgeState {
        variant: 0,
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
        pending_cache_usage: None,
        publication_intent: Default::default(),
        folded_history: Default::default(),
    };

    super::super::compact::handle_compact_completed(&mut state, "", "auto", "full", 7, 0, 2, 1);

    let note = state
        .current_turn
        .view_models()
        .iter()
        .find_map(|unit| match unit {
            crate::kit::tui_render_unit::TuiRenderUnit::TuiSystemNote(note) => Some(&note.text),
            _ => None,
        })
        .expect("Full compact 应注入 system note");
    assert!(note.contains("unmeasured"), "Full 应显示未测量: {note}");
    assert!(
        !note.contains("0 tokens saved"),
        "legacy 0 不得显示为真实节省: {note}"
    );
}

#[test]
#[serial]
fn test_unknown_and_empty_compact_strategy_use_full_unmeasured_detail() {
    for strategy in ["unknown", ""] {
        let mut state = BridgeState {
            variant: 0,
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
            pending_cache_usage: None,
            publication_intent: Default::default(),
            folded_history: Default::default(),
        };

        super::super::compact::handle_compact_completed(
            &mut state, "", "auto", strategy, 7, 0, 2, 1,
        );

        let note = state
            .current_turn
            .view_models()
            .iter()
            .find_map(|unit| match unit {
                crate::kit::tui_render_unit::TuiRenderUnit::TuiSystemNote(note) => Some(&note.text),
                _ => None,
            })
            .expect("legacy Full compact 应注入 system note");
        assert!(
            note.contains("unmeasured"),
            "strategy={strategy:?} 应使用 Full 未测量模板: {note}"
        );
        assert!(
            !note.contains("0 tokens saved"),
            "strategy={strategy:?} 不得显示 legacy numeric 0: {note}"
        );
    }
}

/// REQ-C02 / A12: Compact 完成后 TurnDone **不再**触发 session/load 重放。
///
/// 场景 A：命令 compact（Immediate）后无流事件 → TurnDone 只清标志与桥接 note，
/// 已加载窗口与 Compact 提示原地保留，THREAD_LOAD_TX 保持空闲。
/// 场景 B：agent 内部 auto-compact 后有后续流事件 → 标志被清除，同样不触发。
///
/// 注：THREAD_LOAD_TX 是 OnceLock，两场景合并为单测试以避免 set 冲突；这里保留
/// dispatcher 的装配只为断言「没有任何 load 请求被派发」。
#[tokio::test]
#[serial]
async fn test_compact_turndone_keeps_window_without_reload() {
    use tokio::sync::mpsc;

    // ── 场景 A：命令 compact → 触发 reload ──────────────────────────
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();

    let (client_transport, _server_transport) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) =
        crate::acp_client::AcpTuiClient::new(client_transport);
    let (tx_a, mut rx_a) =
        mpsc::unbounded_channel::<crate::kit::thread_load_consumer::ThreadLoadRequest>();
    // THREAD_LOAD_TX 是 OnceLock：set 失败（已被占用）时 sender 不是本用例持有的
    // `tx_a`，下面两条 `rx_a` 空断言会退化成真空（rx 永远为空、怎么都通过）。装配
    // 必须显式确认成功，让「断言不再指向本用例的通道」成为可见失败。
    if THREAD_LOAD_TX
        .set(crate::kit::thread_load_consumer::ThreadLoadDispatcher::new(
            tx_a, client,
        ))
        .is_err()
    {
        panic!("THREAD_LOAD_TX 已被占用：本用例的「不触发 reload」断言会变成真空");
    }

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
        pending_cache_usage: None,
        publication_intent: Default::default(),
        folded_history: Default::default(),
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

    // REQ-C02 / A12: 已加载窗口必须先有一条旧消息——TurnDone 之后它必须还在。
    state.committed.push_back(TuiRenderUnit::TuiUserBubble(
        crate::kit::tui_render_unit::TuiUserBubble::new("compact 之前已加载的旧消息".into()),
    ));

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    assert!(
        rx_a.try_recv().is_err(),
        "场景 A: 手动 Compact 完成后不得再触发 session/load（REQ-C02）"
    );
    assert!(!state.compact_just_completed, "场景 A: flag 应清除");
    // 已加载窗口保留：旧消息仍在原位，Compact 提示按既有文案落在同一视图里。
    assert_eq!(
        state.committed.len(),
        2,
        "场景 A: 已加载窗口未被整体替换（旧消息 + Compact 提示）"
    );
    assert!(
        matches!(
            &state.committed[0],
            TuiRenderUnit::TuiUserBubble(bubble) if bubble.text.contains("已加载的旧消息")
        ),
        "场景 A: 旧消息仍在窗口首位"
    );
    assert!(
        state.committed.iter().any(|unit| matches!(
            unit,
            TuiRenderUnit::TuiSystemNote(note) if note.text.contains("已压缩 2 条消息")
        )),
        "场景 A: Compact 提示仍落在当前视图"
    );
    assert!(
        crate::kit::atoms::PENDING_COMPACT_NOTE
            .state()
            .read()
            .is_none(),
        "场景 A: 无 replay 可跨，桥接 note 不再残留"
    ); // ── 场景 B：agent 内部 compact → 不触发 reload ──────────────────
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
        pending_cache_usage: None,
        publication_intent: Default::default(),
        folded_history: Default::default(),
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
            strategy: "micro".into(),
            affected_count: 2,
            estimated_tokens_saved: 512,
            files: vec![],
            skills: vec![],
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
            strategy: "full".into(),
            affected_count: 4,
            estimated_tokens_saved: 1024,
            files: vec![],
            skills: vec![],
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
