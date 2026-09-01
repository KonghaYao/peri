use super::*;

/// 竞态防护（Issue 2026-08-05）：新提交（LocalUserBubble）先入队后，
/// stale TurnInterrupted（旧 turn）到达时跳过零产出回滚——不删新气泡、
/// 不恢复文本；排队输入（用户已提交的新请求，不得静默丢弃）在复位后
/// **立即 drain 提交**（遗留项修复：不再滞留悬挂至下一 TurnDone）。
/// 归档旧产出 + 复位。
/// 本测试走排队分支（B 无 PromptSubmitted、事件 request_id=None）→ 代际判定兜底。
#[test]
#[serial]
fn test_stale_turn_interrupted_does_not_rollback_new_turn() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    INPUT_BUFFER.state().write().clear();
    if let Some(mu) = crate::kit::atoms::INPUT_RESTORE_TEXT.get() {
        mu.lock().take();
    }
    // 确保 SUBMIT_TX 已初始化（stale 分支 drain 依赖）；若本次成功安装
    // 可观察 channel，则顺带验证提交消息确实发出。
    let mut drain_rx = ensure_submit_tx_observable();

    // turn A：LocalUserBubble + PromptSubmitted（gen=1, last_prompt=1）
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
        &AcpEventData::LocalUserBubble { text: "A".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted { request_id: None },
    );

    // 竞态：用户提交 B——LocalUserBubble 已到达（gen=2），
    // PromptSubmitted 尚未发出（B 排队中或 submit_consumer 仍在等 A 的 RPC）。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "B".into() },
    );
    INPUT_BUFFER.state().write().push_back("queued".into());
    state.pending_cache_usage = Some(CacheUsageSample {
        input_tokens: 100,
        cached_tokens: 90,
        request_id: Some("new-turn".into()),
    });
    assert_eq!(state.turn_generation, 2);
    assert_eq!(state.last_prompt_generation, 1, "B 的 prompt RPC 尚未发出");
    let committed_before = state.committed.len(); // A + B 两个气泡

    // stale TurnInterrupted（turn A 的取消事件晚到）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: None,
        },
    );

    // 不污染新 turn：B 气泡保留、文本不恢复、loading 复位；
    // 排队输入立即 drain 提交（遗留项修复——旧 turn 已取消、TurnDone
    // 永不到达，滞留会悬挂或顺序反转，复位后必须主动提交）
    assert_eq!(
        state.committed.len(),
        committed_before,
        "stale TurnInterrupted 不得删除新 turn 的用户气泡"
    );
    match &state.committed[1] {
        TuiRenderUnit::TuiUserBubble(d) => assert_eq!(d.text, "B"),
        other => panic!("committed[1] 应为 B 的气泡, got {other:?}"),
    }
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "排队输入属于用户已提交的新请求：stale 复位后应立即 drain 提交（不得滞留悬挂）"
    );
    if let Some(mut rx) = drain_rx.take() {
        match rx.try_recv() {
            Ok(SubmitRequest::AgentText(t)) => assert_eq!(t, "queued"),
            Ok(other) => panic!("stale drain 应提交 AgentText, got {other:?}"),
            Err(e) => panic!("stale drain 应发出排队输入, got {e:?}"),
        }
    }
    assert!(
        crate::kit::atoms::INPUT_RESTORE_TEXT
            .get()
            .and_then(|mu| mu.lock().clone())
            .is_none(),
        "stale TurnInterrupted 不得恢复旧输入文本"
    );
    assert_eq!(
        state.phase,
        SessionPhase::Idle,
        "phase 应复位（loading 解除）"
    );
    // 返工：stale 分支保留 last_submitted_text——它是最近一次提交（B）的回滚锚点，
    // 后续 B 被取消（连续取消）时零产出回滚仍需恢复 B 的输入文本。
    assert_eq!(
        state.last_submitted_text.as_deref(),
        Some("B"),
        "stale TurnInterrupted 应保留最近一次提交的文本锚点"
    );
    assert_eq!(
        state
            .pending_cache_usage
            .as_ref()
            .unwrap()
            .request_id
            .as_deref(),
        Some("new-turn"),
        "stale old interruption must preserve the new prompt's cache sample"
    );
}

/// 非 stale 的零产出回滚：删气泡 + 恢复文本；排队项（Slice 3 D4）不吞——
/// 取消后立即 drain 提交（用户已提交的请求，composer 上方队列可见）。
#[test]
#[serial]
fn test_turn_interrupted_zero_output_rollback_still_works() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    INPUT_BUFFER.state().write().clear();
    if let Some(mu) = crate::kit::atoms::INPUT_RESTORE_TEXT.get() {
        mu.lock().take();
    }
    // 确保 SUBMIT_TX 已初始化（drain 依赖）；若本次成功安装可观察 channel，
    // 则顺带验证排队项确实被提交。
    let mut drain_rx = ensure_submit_tx_observable();

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
        &AcpEventData::LocalUserBubble { text: "A".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted { request_id: None },
    );
    state.pending_cache_usage = Some(CacheUsageSample {
        input_tokens: 100,
        cached_tokens: 50,
        request_id: None,
    });
    INPUT_BUFFER.state().write().push_back("queued".into());
    assert_eq!(state.committed.len(), 1);

    // 无新提交 → 非 stale → 正常回滚
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: None,
        },
    );

    assert!(
        state.committed.is_empty(),
        "零产出回滚应删除本 turn 的用户气泡"
    );
    assert_eq!(
        crate::kit::atoms::INPUT_RESTORE_TEXT
            .get()
            .and_then(|mu| mu.lock().clone())
            .as_deref(),
        Some("A"),
        "零产出回滚应恢复输入文本"
    );
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "排队项应被 drain（取消不吞排队输入——Slice 3 D4）"
    );
    if let Some(mut rx) = drain_rx.take() {
        match rx.try_recv() {
            Ok(SubmitRequest::AgentText(t)) => assert_eq!(t, "queued"),
            Ok(other) => panic!("取消后 drain 应提交 AgentText, got {other:?}"),
            Err(e) => panic!("取消后 drain 应发出排队输入, got {e:?}"),
        }
    }
    assert_eq!(state.phase, SessionPhase::Idle);
    assert!(
        state.pending_cache_usage.is_none(),
        "non-stale interruption must clear pending cache coverage"
    );
}

/// 次要项 (a)：TurnInterrupted 归档分支（current_turn 非空）drain 排队项——
/// 排队输入是用户已提交的请求（Slice 3 D4），取消后立即提交而非丢弃
/// （也不得滞留到下一 TurnDone 意外提交——drain 即时清空 buffer）。
#[test]
#[serial]
fn test_turn_interrupted_archive_branch_drains_input_buffer() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    INPUT_BUFFER.state().write().clear();
    let mut drain_rx = ensure_submit_tx_observable();

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
        &AcpEventData::LocalUserBubble { text: "A".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted { request_id: None },
    );
    // A 已产出部分内容
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "partial".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    INPUT_BUFFER.state().write().push_back("queued".into());

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: None,
        },
    );

    assert_eq!(
        state.committed.len(),
        2,
        "归档分支：A 气泡 + 已产出内容应归档到 committed"
    );
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "归档分支应 drain 排队项（取消不吞排队输入——Slice 3 D4）"
    );
    if let Some(mut rx) = drain_rx.take() {
        match rx.try_recv() {
            Ok(SubmitRequest::AgentText(t)) => assert_eq!(t, "queued"),
            Ok(other) => panic!("归档分支 drain 应提交 AgentText, got {other:?}"),
            Err(e) => panic!("归档分支 drain 应发出排队输入, got {e:?}"),
        }
    }
    assert_eq!(state.phase, SessionPhase::Idle);
}

/// Issue 2026-08-05 返工核心验收（主导排序）：新提交 B 已发 RPC
/// （PromptSubmitted 先到）后，旧 turn A 的 TurnInterrupted 晚到——
/// request_id 配对判定（A1 ≠ B1）应识别为 stale：不删 B 气泡、不恢复文本；
/// 排队输入（用户已提交的新请求，不得随旧 turn 取消作废）复位后立即
/// drain 提交（遗留项修复）。
#[test]
#[serial]
fn test_stale_turn_interrupted_request_id_mismatch() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    INPUT_BUFFER.state().write().clear();
    if let Some(mu) = crate::kit::atoms::INPUT_RESTORE_TEXT.get() {
        mu.lock().take();
    }
    // 确保 SUBMIT_TX 已初始化（stale 分支 drain 依赖）；若本次成功安装
    // 可观察 channel，则顺带验证提交消息确实发出。
    let mut drain_rx = ensure_submit_tx_observable();

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
    // turn A：LocalUserBubble + PromptSubmitted(A1)
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "A".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("A1".into()),
        },
    );
    // turn B：LocalUserBubble + PromptSubmitted(B1)——B 走完整 RPC 路径（主导排序）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "B".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("B1".into()),
        },
    );
    // 排队输入（B 提交之后用户又输入的排队请求）——stale 复位后立即 drain 提交
    INPUT_BUFFER.state().write().push_back("queued".into());
    assert_eq!(state.current_request_id.as_deref(), Some("B1"));
    assert_eq!(state.turn_generation, 2);
    assert_eq!(
        state.last_prompt_generation, 2,
        "B 的 PromptSubmitted 已到（v1 判定在此场景失效）"
    );
    let committed_before = state.committed.len(); // A + B 两个气泡

    // stale TurnInterrupted（turn A 的取消事件晚到，服务器往返数百 ms）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: Some("A1".into()),
        },
    );

    // 验收：不删新气泡、不恢复旧文本；排队输入复位后立即 drain 提交
    assert_eq!(
        state.committed.len(),
        committed_before,
        "stale TurnInterrupted 不得删除新 turn 的用户气泡"
    );
    match &state.committed[1] {
        TuiRenderUnit::TuiUserBubble(d) => assert_eq!(d.text, "B"),
        other => panic!("committed[1] 应为 B 的气泡, got {other:?}"),
    }
    assert!(
        crate::kit::atoms::INPUT_RESTORE_TEXT
            .get()
            .and_then(|mu| mu.lock().clone())
            .is_none(),
        "stale TurnInterrupted 不得恢复旧输入文本"
    );
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "排队输入属于用户已提交的新请求：stale 复位后应立即 drain 提交（不得滞留悬挂）"
    );
    if let Some(mut rx) = drain_rx.take() {
        match rx.try_recv() {
            Ok(SubmitRequest::AgentText(t)) => assert_eq!(t, "queued"),
            Ok(other) => panic!("stale drain 应提交 AgentText, got {other:?}"),
            Err(e) => panic!("stale drain 应发出排队输入, got {e:?}"),
        }
    }
    assert_eq!(
        state.phase,
        SessionPhase::Idle,
        "phase 应复位（loading 解除）"
    );
    // 返工：stale 分支保留 last_submitted_text——它是最近一次提交（B）的回滚锚点，
    // 后续 B 被取消（连续取消）时零产出回滚仍需恢复 B 的输入文本。
    assert_eq!(
        state.last_submitted_text.as_deref(),
        Some("B"),
        "stale TurnInterrupted 应保留最近一次提交的文本锚点"
    );
}

/// Issue 2026-08-05 返工：排队分支（B 仅 LocalUserBubble、无 PromptSubmitted）
/// 下 stale 判定回退 v1 代际判定——id 配对（A1 == A1）判不出 stale，但
/// turn_generation(2) > last_prompt_generation(1) 识别为 stale。排队输入 B
/// 在复位后立即 drain 提交（遗留项修复）。
#[test]
#[serial]
fn test_stale_turn_interrupted_queued_branch_still_stale() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    INPUT_BUFFER.state().write().clear();
    if let Some(mu) = crate::kit::atoms::INPUT_RESTORE_TEXT.get() {
        mu.lock().take();
    }
    // 确保 SUBMIT_TX 已初始化（stale 分支 drain 依赖）；若本次成功安装
    // 可观察 channel，则顺带验证提交消息确实发出。
    let mut drain_rx = ensure_submit_tx_observable();

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
        &AcpEventData::LocalUserBubble { text: "A".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("A1".into()),
        },
    );
    // B 排队中：仅 LocalUserBubble，无 PromptSubmitted（is_loading gate 阻断 RPC）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "B".into() },
    );
    INPUT_BUFFER.state().write().push_back("B".into());
    assert_eq!(
        state.current_request_id.as_deref(),
        Some("A1"),
        "B 无 RPC → current_request_id 停留 A1"
    );
    assert_eq!(state.turn_generation, 2);
    assert_eq!(state.last_prompt_generation, 1);
    let committed_before = state.committed.len(); // A + B 两个气泡

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: Some("A1".into()),
        },
    );

    // 代际判定兜底 → stale：B 气泡保留、排队输入复位后立即 drain 提交
    assert_eq!(state.committed.len(), committed_before);
    match &state.committed[1] {
        TuiRenderUnit::TuiUserBubble(d) => assert_eq!(d.text, "B"),
        other => panic!("committed[1] 应为 B 的气泡, got {other:?}"),
    }
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "排队分支的 B 是用户已提交的请求：stale 复位后应立即 drain 提交（不得滞留悬挂）"
    );
    if let Some(mut rx) = drain_rx.take() {
        match rx.try_recv() {
            Ok(SubmitRequest::AgentText(t)) => assert_eq!(t, "B"),
            Ok(other) => panic!("stale drain 应提交 AgentText, got {other:?}"),
            Err(e) => panic!("stale drain 应发出排队输入, got {e:?}"),
        }
    }
    assert_eq!(state.phase, SessionPhase::Idle);
}

/// Issue 2026-08-05 遗留项：多次 stale TurnInterrupted 不重复 drain——
/// 第一次复位后 buffer 已清空，第二次（更旧 turn）到达时 drain no-op，
/// 不重复提交、不重复归档。
#[test]
#[serial]
fn test_stale_turn_interrupted_drain_is_idempotent() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    INPUT_BUFFER.state().write().clear();
    let drain_rx = ensure_submit_tx_observable();

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
    // turn A 运行中
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "A".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("A1".into()),
        },
    );
    // B 排队中（仅 LocalUserBubble，无 RPC）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "B".into() },
    );
    INPUT_BUFFER.state().write().push_back("B".into());

    // 第一个 stale 事件（A 的取消晚到）→ 复位 + drain
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: Some("A1".into()),
        },
    );
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "第一次 stale 后排队输入应立即 drain"
    );

    // 第二个 stale 事件（更旧 turn 的取消，id 仍不匹配）→ drain no-op
    let committed_before = state.committed.len();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: Some("X0".into()),
        },
    );
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "第二次 stale 不得重新产生排队输入"
    );
    assert_eq!(
        state.committed.len(),
        committed_before,
        "第二次 stale 不得重复归档/提交"
    );
    assert_eq!(state.phase, SessionPhase::Idle);
    if let Some(mut rx) = drain_rx {
        // 只应提交 1 条（第一次 stale 的 drain）
        match rx.try_recv() {
            Ok(SubmitRequest::AgentText(t)) => assert_eq!(t, "B"),
            Ok(other) => panic!("stale drain 应提交 AgentText, got {other:?}"),
            Err(e) => panic!("stale drain 应发出排队输入, got {e:?}"),
        }
        assert!(
            matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "第二次 stale 不得重复提交排队输入"
        );
    }
}

/// Issue 2026-08-05 返工：正常取消（无新提交）时 request_id 精确配对
/// （A1 == A1）→ 非 stale → 零产出回滚保持原行为；排队项（Slice 3 D4）
/// 不吞——取消后立即 drain 提交。
#[test]
#[serial]
fn test_turn_interrupted_current_request_id_rollback() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    INPUT_BUFFER.state().write().clear();
    if let Some(mu) = crate::kit::atoms::INPUT_RESTORE_TEXT.get() {
        mu.lock().take();
    }
    let mut drain_rx = ensure_submit_tx_observable();

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
        &AcpEventData::LocalUserBubble { text: "A".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("A1".into()),
        },
    );
    INPUT_BUFFER.state().write().push_back("queued".into());
    assert_eq!(state.committed.len(), 1);

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: Some("A1".into()),
        },
    );

    // 非 stale → 正常零产出回滚：删 A 气泡 + 恢复文本 + drain 排队项
    assert!(
        state.committed.is_empty(),
        "零产出回滚应删除本 turn 的用户气泡"
    );
    assert_eq!(
        crate::kit::atoms::INPUT_RESTORE_TEXT
            .get()
            .and_then(|mu| mu.lock().clone())
            .as_deref(),
        Some("A"),
        "零产出回滚应恢复输入文本"
    );
    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "排队项应被 drain（取消不吞排队输入——Slice 3 D4）"
    );
    if let Some(mut rx) = drain_rx.take() {
        match rx.try_recv() {
            Ok(SubmitRequest::AgentText(t)) => assert_eq!(t, "queued"),
            Ok(other) => panic!("回滚后 drain 应提交 AgentText, got {other:?}"),
            Err(e) => panic!("回滚后 drain 应发出排队输入, got {e:?}"),
        }
    }
    assert_eq!(state.phase, SessionPhase::Idle);
}

/// Issue 2026-08-05 返工：request_id 缺失（continuation / Immediate 命令 /
/// stdio 等路径回带 None）→ 跳过 id 判定，仅 v1 代际判定（非 stale → 回滚）。
#[test]
#[serial]
fn test_turn_interrupted_none_request_id_falls_back() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    INPUT_BUFFER.state().write().clear();
    if let Some(mu) = crate::kit::atoms::INPUT_RESTORE_TEXT.get() {
        mu.lock().take();
    }

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
        &AcpEventData::LocalUserBubble { text: "A".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("A1".into()),
        },
    );

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: None,
        },
    );

    // id 判定跳过；代际判定：turn_generation == last_prompt_generation → 非 stale → 回滚
    assert!(
        state.committed.is_empty(),
        "request_id=None 时回退 v1 判定，正常取消应回滚"
    );
    assert_eq!(
        crate::kit::atoms::INPUT_RESTORE_TEXT
            .get()
            .and_then(|mu| mu.lock().clone())
            .as_deref(),
        Some("A"),
        "request_id=None 时正常取消应恢复输入文本"
    );
    assert_eq!(state.phase, SessionPhase::Idle);
}

/// Issue 2026-08-05 返工：连续取消——A 取消事件（A1）先到（stale，B 保留），
/// B 的取消事件（B1）后到（id 精确配对 → 回滚 B）。request_id 配对提供
/// 精确的 turn 归属，时间快照启发式无法区分。
#[test]
#[serial]
fn test_double_cancel_request_id_pairs() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    INPUT_BUFFER.state().write().clear();
    if let Some(mu) = crate::kit::atoms::INPUT_RESTORE_TEXT.get() {
        mu.lock().take();
    }

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
    // A 提交并运行
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "A".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("A1".into()),
        },
    );
    // A 被取消后用户提交 B（完整 RPC 路径）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "B".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("B1".into()),
        },
    );

    // 第一个晚到事件：A 的取消（A1 ≠ 当前 B1）→ stale，B 保留
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: Some("A1".into()),
        },
    );
    assert_eq!(state.committed.len(), 2, "A 的取消不得删除 B 的气泡");
    assert_eq!(
        crate::kit::atoms::INPUT_RESTORE_TEXT
            .get()
            .and_then(|mu| mu.lock().clone()),
        None,
        "A 的取消不得恢复旧输入文本"
    );

    // 随后 B 的取消（B1 == B1）→ 非 stale → 正常回滚 B
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: Some("B1".into()),
        },
    );
    assert_eq!(
        state.committed.len(),
        1,
        "B 的取消应回滚 B 的气泡（A 气泡保留）"
    );
    match &state.committed[0] {
        TuiRenderUnit::TuiUserBubble(d) => assert_eq!(d.text, "A"),
        other => panic!("committed[0] 应为 A 的气泡, got {other:?}"),
    }
    assert_eq!(
        crate::kit::atoms::INPUT_RESTORE_TEXT
            .get()
            .and_then(|mu| mu.lock().clone())
            .as_deref(),
        Some("B"),
        "B 的取消应恢复 B 的输入文本"
    );
    assert_eq!(state.phase, SessionPhase::Idle);
}
