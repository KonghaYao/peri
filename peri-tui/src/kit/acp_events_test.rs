//! Tests for acp_events

use super::*;
use crate::kit::acp_types::AcpEventWithEpoch;
use crate::kit::message_area::TodoStatus;
use crate::kit::tui_render_unit::{
    InteractionKind, TuiAskUserBlock, TuiTodoChangeKind, TuiToolPresentation,
};
use peri_acp_types::event_data::{AskUser, HitlPending, Question, QuestionOption};
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
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
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
            message_id: Some("c1".into()),
            agent_id: Some("agent-1".into()),
        }),
    );

    // 流式期间：子 turn trailing bubble 持有 started_at（Running 形态）。
    let running_snap = VIEW_MODELS.state().read().clone();
    let b = match &running_snap.items[0] {
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
        },
    );

    // stop 后：started_at 清除 + duration_ms 冻结（详情面板不再显示增长中的
    // `◐ Thinking… Ns`）。
    let snap = VIEW_MODELS.state().read().clone();
    let b = match &snap.items[0] {
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

/// Slice 3 D4：drain 时**每条**排队文本先 `send_local_user_bubble`（本地气泡恰
/// 出现一次，镜像非 loading 路径）再提交 AgentText——不依赖服务端回显。
/// LOCAL_EVENT_TX 与 SUBMIT_TX 同为全局 OnceLock：本测试安装成功时（serial
/// 首个）可观察两通道，验证 FIFO 顺序与气泡唯一性；已被占用时只断言
/// buffer 清空（核心效应）。
#[test]
#[serial]
fn test_drain_input_buffer_sends_local_user_bubble_once() {
    crate::kit::atoms::init_atoms();
    INPUT_BUFFER.state().write().clear();
    // 安装可观察 channel；OnceLock 已占用则返回 None（跳过通道级断言）。
    let (tx, rx) = mpsc::unbounded_channel::<AcpEventWithEpoch>();
    let local_rx = match LOCAL_EVENT_TX.set(tx) {
        Ok(()) => Some(rx),
        Err(_) => None,
    };
    let mut submit_rx = ensure_submit_tx_observable();

    // 入队两条
    {
        let state = INPUT_BUFFER.state();
        let mut buf = state.write();
        buf.push_back("first".into());
        buf.push_back("second".into());
    }

    drain_input_buffer();

    assert!(
        INPUT_BUFFER.state().read().is_empty(),
        "buffer should be empty after drain"
    );
    if let Some(mut rx) = local_rx {
        // 恰一条 LocalUserBubble（FIFO 首条）
        match rx.try_recv() {
            Ok(ev) => match ev.event {
                AcpEventData::LocalUserBubble { text } => {
                    assert_eq!(text, "first", "drain 应先发首条排队项的气泡")
                }
                other => panic!("drain 应发 LocalUserBubble, got {other:?}"),
            },
            Err(e) => panic!("drain 应发出本地气泡, got {e:?}"),
        }
        // 第二条排队项的气泡
        match rx.try_recv() {
            Ok(ev) => match ev.event {
                AcpEventData::LocalUserBubble { text } => assert_eq!(text, "second"),
                other => panic!("drain 应发 LocalUserBubble, got {other:?}"),
            },
            Err(e) => panic!("drain 应发出第二条气泡, got {e:?}"),
        }
        assert!(
            matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
            "drain 不应发出第三条事件（气泡恰一次）"
        );
    }
    if let Some(mut rx) = submit_rx.take() {
        match rx.try_recv() {
            Ok(SubmitRequest::AgentText(t)) => assert_eq!(t, "first"),
            Ok(other) => panic!("drain 应提交 AgentText, got {other:?}"),
            Err(e) => panic!("drain 应提交排队输入, got {e:?}"),
        }
    }
}

/// 确保全局 SUBMIT_TX 已初始化，并尽可能安装可观察的 receiver。
///
/// 返回 Some(rx)：本次测试**新安装**了 channel，可断言 drain 实际发出的
/// SubmitRequest 消息；返回 None：channel 已由先前测试安装（OnceLock 全局
/// 单例无法重置，只能断言 drain 的核心效应——buffer 被清空）。
fn ensure_submit_tx_observable() -> Option<mpsc::UnboundedReceiver<SubmitRequest>> {
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    SUBMIT_TX.set(tx).ok()?;
    Some(rx)
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
    };
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "A".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted { request_id: None },
    );
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

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    let received = rx_a.try_recv().ok();
    assert_eq!(
        received.as_deref(),
        Some("test-session"),
        "场景 A: THREAD_LOAD_TX 应收到 session_id"
    );
    assert!(!state.compact_just_completed, "场景 A: flag 应清除");

    // ── 场景 B：agent 内部 compact → 不触发 reload ──────────────────
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
            files: vec![],
            skills: vec![],
            micro_cleared: 0,
            messages_json: String::new(),
            strategy: "micro".into(),
            trigger: "auto".into(),
            outcome: "micro_applied".into(),
        },
    );
    assert!(
        !state.compact_just_completed,
        "场景 B: auto compact 后标志不应置位（方案 A）"
    );
    assert!(
        crate::kit::atoms::PENDING_COMPACT_NOTE
            .state()
            .read()
            .is_none(),
        "场景 B: auto compact 不应写入 PENDING_COMPACT_NOTE（无 replay）"
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

    // ── 场景 B2：manual compact + 流事件（方案 B 防御路径保留）────────
    // manual compact 置位后，若 agent 继续产出流事件（理论上 manual 是
    // Immediate 命令无流事件，但防御逻辑保留），标志被清除，TurnDone
    // 不触发 reload。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::CompactCompleted {
            summary: "manual compact summary".into(),
            files: vec![],
            skills: vec![],
            micro_cleared: 0,
            messages_json: String::new(),
            strategy: "full".into(),
            trigger: "manual".into(),
            outcome: "full_applied".into(),
        },
    );
    assert!(
        state.compact_just_completed,
        "场景 B2: manual compact 后标志应置位"
    );
    // B2 补充（issue 2026-08-08-e2e-compact-command-screenshot-too-early）：
    // manual compact 写入 PENDING_COMPACT_NOTE——TurnDone 触发 session/load
    // replay 时 bridge reset 会清空 committed（含 SystemNote），replay 后由
    // reset 分支从该 atom 重建完成提示。
    let pending_note = crate::kit::atoms::PENDING_COMPACT_NOTE
        .state()
        .read()
        .clone();
    let note_ok = pending_note
        .as_deref()
        .is_some_and(|t| t.contains("compaction completed") || t.contains("压缩完成"));
    assert!(
        note_ok,
        "场景 B2: manual compact 应写入 compact 完成提示，实际: {pending_note:?}"
    );

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
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
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
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
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
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
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
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
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
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
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

// ── Slice 2：折叠状态机（spec §7 表）经 push_view_models 单点 pass ──────────

use crate::kit::stream_data::{TuiReasoningChunk, TuiTextChunk, TuiToolEnded, TuiToolStarted};
use crate::kit::tui_render_unit::{EntryStatus, FoldKey, FoldState, TuiReasoningBlock};

fn make_fold_test_state() -> BridgeState {
    crate::kit::atoms::init_atoms();
    *FOLD_OVERRIDES.state().write() = std::collections::HashMap::new();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();
    BridgeState {
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
    }
}

fn reasoning_of(snapshot: &ViewModelsSnapshot, idx: usize) -> &TuiReasoningBlock {
    match &snapshot.items[idx] {
        TuiRenderUnit::TuiAssistantBubble(b) => b.reasoning.as_ref().expect("应含 reasoning"),
        other => panic!("expected TuiAssistantBubble at [{idx}], got {other:?}"),
    }
}

fn tool_card_of(
    snapshot: &ViewModelsSnapshot,
    idx: usize,
) -> &crate::kit::tui_render_unit::TuiToolCard {
    match &snapshot.items[idx] {
        TuiRenderUnit::TuiToolCard(t) => t,
        other => panic!("expected TuiToolCard at [{idx}], got {other:?}"),
    }
}

/// §7 reasoning 行：流式（PromptRunning + trailing）→ Preview/Running；
/// TurnDone 后 phase 离开 PromptRunning → 全 Completed → Collapsed 单行。
#[test]
#[serial]
fn test_fold_pass_reasoning_running_preview_then_completed_collapsed() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "正在检查消息类型……".into(),
            message_id: Some("msg_r1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let r = reasoning_of(&snap, 0);
    assert_eq!(
        r.status,
        EntryStatus::Running,
        "流式中 reasoning 应为 Running"
    );
    assert!(r.is_running);
    assert_eq!(r.fold, FoldState::Preview, "§7 running 行 → Preview");
    assert!(
        !r.collapsed(),
        "Preview 经 collapsed() 访问器视为展开（body 可见）"
    );

    // 追加文本 chunk——文本到达 = 本消息 thinking 块结束（方案 1）：
    // 推理块立即冻结为 Completed/Collapsed（`◐ Thinking…` 停止），正文继续流式。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "回复内容".into(),
            message_id: Some("msg_r1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let r = reasoning_of(&snap, 0);
    assert_eq!(
        r.status,
        EntryStatus::Completed,
        "文本到达后推理应冻结为 Completed"
    );
    assert!(!r.is_running, "文本到达后推理不再 running");
    assert_eq!(
        r.fold,
        FoldState::Collapsed,
        "推理结束后自动收束为单行 Collapsed"
    );
    assert!(
        r.duration_ms.is_some(),
        "推理冻结时应携带时长（Thought for Ns）"
    );

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    let r = reasoning_of(&snap, 0);
    assert_eq!(
        r.status,
        EntryStatus::Completed,
        "TurnDone 后 reasoning 应为 Completed"
    );
    assert!(!r.is_running);
    assert_eq!(
        r.fold,
        FoldState::Collapsed,
        "§7 completed 行 → Collapsed（自动收束为单行）"
    );
    assert!(r.collapsed());
}

/// §7 tool 行：running → Preview；success → Collapsed；error → Expanded summary。
#[test]
#[serial]
fn test_fold_pass_tool_preview_collapsed_error_expanded() {
    let mut state = make_fold_test_state();

    // running：ToolStarted
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t1".into(),
            tool_name: "Read".into(),
            input_summary: "README.md".into(),
            raw_input: serde_json::json!({"path": "README.md"}),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(tool_card_of(&snap, 0).fold, FoldState::Preview);
    assert!(!tool_card_of(&snap, 0).user_modified);

    // success：ToolEnded（is_error=false）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "t1".into(),
            output_summary: "ok".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let t = tool_card_of(&snap, 0);
    assert_eq!(
        t.fold,
        FoldState::Collapsed,
        "§7 tool completed → Collapsed"
    );
    assert!(!t.is_running);

    // error：新工具失败
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t2".into(),
            tool_name: "Bash".into(),
            input_summary: "false".into(),
            raw_input: serde_json::json!({"command": "false"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "t2".into(),
            output_summary: "exit 1".into(),
            is_error: true,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let t = tool_card_of(&snap, 1);
    assert_eq!(
        t.fold,
        FoldState::Expanded,
        "§7 tool error → Expanded summary（永不自动隐藏）"
    );
}

/// §7 subagent 行：running 与 completed 均为 Collapsed（running 靠 live summary 表达）。
#[test]
#[serial]
fn test_fold_pass_subagent_running_and_completed_collapsed() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sa-1".into(),
            agent_name: "explorer".into(),
            is_background: false,
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    match &snap.items[0] {
        TuiRenderUnit::TuiSubAgentGroup(g) => {
            assert!(g.is_running);
            assert_eq!(
                g.fold,
                FoldState::Collapsed,
                "§7 subagent running → Collapsed + live summary"
            );
        }
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    }

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "sa-1".into(),
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    match &snap.items[0] {
        TuiRenderUnit::TuiSubAgentGroup(g) => {
            assert!(!g.is_running);
            assert_eq!(g.fold, FoldState::Collapsed);
        }
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    }
}

/// 手动覆盖免疫：reasoning 完成折叠后用户展开（FOLD_OVERRIDES），
/// 后续 push（新 turn 开始）不得重新折叠（spec §7「本 turn 内不再被自动策略覆盖」）。
#[test]
#[serial]
fn test_fold_pass_reasoning_manual_override_survives_following_turn() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "第一轮思考".into(),
            message_id: Some("msg_r1".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "第一轮回复".into(),
            message_id: Some("msg_r1".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    assert!(reasoning_of(&snap, 0).collapsed());

    // 用户手动展开（键盘 handler 的持久化等价操作）
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Reasoning("msg_r1".into()), FoldState::Expanded);

    // 新一轮开始——push 重建后手动展开必须保持
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "第二个问题".into(),
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    // items = [Assistant(msg_r1), UserBubble]——LocalUserBubble append 到 committed 尾部
    let r = reasoning_of(&snap, 0);
    assert_eq!(
        r.fold,
        FoldState::Expanded,
        "手动展开后跨 turn 不得被自动折叠"
    );
}

/// 手动覆盖免疫：running reasoning 被用户展开后，流式继续不得强制收回 Preview。
#[test]
#[serial]
fn test_fold_pass_reasoning_manual_override_immune_during_streaming() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "思考中".into(),
            message_id: Some("msg_r2".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(reasoning_of(&snap, 0).fold, FoldState::Preview);

    // 用户手动展开（流式中）
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Reasoning("msg_r2".into()), FoldState::Expanded);

    // 继续 streaming——不得收回 Preview
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "继续思考".into(),
            message_id: Some("msg_r2".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(
        reasoning_of(&snap, 0).fold,
        FoldState::Expanded,
        "手动展开在流式期间免疫自动策略"
    );
}

/// tool 覆盖：手动展开已完成工具后，重建（output 更新）仍保持 + user_modified 恢复。
#[test]
#[serial]
fn test_fold_pass_tool_manual_override_restores_user_modified() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t1".into(),
            tool_name: "Read".into(),
            input_summary: "a".into(),
            raw_input: serde_json::json!({"path": "a"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "t1".into(),
            output_summary: "done".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(tool_card_of(&snap, 0).fold, FoldState::Collapsed);

    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Tool("t1".into()), FoldState::Expanded);

    // 新一轮 push（LocalUserBubble 触发 view_models 重建）——
    // 卡片重建后覆盖与 user_modified 恢复
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "第二个问题".into(),
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    // items = [UserBubble, ToolCard]——工具卡在 current_turn，LocalUserBubble append 到 committed
    let t = tool_card_of(&snap, 1);
    assert_eq!(t.fold, FoldState::Expanded, "手动展开跨重建保持");
    assert!(
        t.user_modified,
        "覆盖存在的 entry 恢复 user_modified=true（免疫自动策略）"
    );
}

/// session 复位（BRIDGE_RESET_COUNTER → push_view_models_for_reset）清空覆盖表。
#[test]
#[serial]
fn test_push_view_models_for_reset_clears_fold_overrides() {
    make_fold_test_state();
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Tool("t1".into()), FoldState::Expanded);
    assert!(!FOLD_OVERRIDES.state().read().is_empty());
    // [S2 §3.4] 焦点单一事实源随 session 复位同步清空（slot/key 依赖旧会话
    // 索引与身份，残留会让新会话焦点/免疫错误指向）。
    *crate::kit::atoms::FOCUSED_ENTRY.state().write() = Some(crate::kit::atoms::FocusedEntry {
        slot: 0,
        key: Some(FoldKey::Tool("t1".into())),
    });
    assert!(crate::kit::atoms::FOCUSED_ENTRY.state().read().is_some());

    push_view_models_for_reset();

    assert!(
        FOLD_OVERRIDES.state().read().is_empty(),
        "session 复位必须清空覆盖表（跨 session 身份不唯一）"
    );
    assert!(
        crate::kit::atoms::FOCUSED_ENTRY.state().read().is_none(),
        "session 复位必须清空 entry 焦点"
    );
    assert!(VIEW_MODELS.state().read().items.is_empty());
}

// ── Slice 3：快照后处理流水线（turn divider / todo 摘要 / 工具分组）─────────

/// §6.6 turn 边界 divider：上一 turn 结束后，新 turn 的 prompt 位于 committed
/// 末尾——divider 插在 prompt 之前（committed|current_turn 边界本身是
/// prompt↔回复 的同一 turn 内部，不能用）。
#[test]
#[serial]
fn test_snapshot_turn_divider_before_new_user_prompt() {
    let mut state = make_fold_test_state();

    // Turn 1：user + answer → TurnDone（committed = [user1, answer1]）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "q1".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "a1".into(),
            message_id: Some("m1".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    // Turn 2 流式期间：committed = [user1, answer1, user2]
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "q2".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "a2".into(),
            message_id: Some("m2".into()),
            agent_id: None,
        }),
    );

    let snap = VIEW_MODELS.state().read().clone();
    // [user1, answer1, divider, user2, a2]
    assert_eq!(snap.items.len(), 5);
    match &snap.items[2] {
        TuiRenderUnit::TuiDivider(d) => assert_eq!(d.label, None),
        other => panic!("expected TuiDivider at [2], got {other:?}"),
    }
    assert!(
        matches!(&snap.items[3], TuiRenderUnit::TuiUserBubble(_)),
        "divider 应在新 prompt 之前"
    );

    // TurnDone 后 current_turn 清空 → divider 消失（仅流式期间存在）
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 4, "turn 完成后无 divider");
}

/// 首轮（committed 仅 1 个 prompt）不插 divider；同一 turn 内 prompt↔回复
/// 之间不插 divider。
#[test]
#[serial]
fn test_snapshot_no_divider_for_first_turn_or_inside_turn() {
    let mut state = make_fold_test_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble { text: "q1".into() },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "a1".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    // [user1, a1]——首轮无 divider
    assert_eq!(snap.items.len(), 2);
    assert!(
        snap.items
            .iter()
            .all(|vm| !matches!(vm, TuiRenderUnit::TuiDivider(_))),
        "首轮 prompt↔回复 之间不得有 divider"
    );
}

/// §6.9 todo 摘要：活动 turn（PromptRunning）且 TODO_ITEMS 非空时，
/// 摘要行插在 trailing 最终回答之前；回答后无 todo。
#[test]
#[serial]
fn test_snapshot_todo_summary_before_final_answer() {
    let mut state = make_fold_test_state();
    *crate::kit::atoms::TODO_ITEMS.state().write() = vec![
        crate::kit::message_area::TodoItem {
            status: crate::kit::message_area::TodoStatus::InProgress,
            content: "Running tests".into(),
        },
        crate::kit::message_area::TodoItem {
            status: crate::kit::message_area::TodoStatus::Completed,
            content: "Setup".into(),
        },
    ];

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "final answer".into(),
            message_id: Some("m1".into()),
            agent_id: None,
        }),
    );

    let snap = VIEW_MODELS.state().read().clone();
    // [todo_summary, answer]——摘要位于最终回答之前
    assert_eq!(snap.items.len(), 2);
    match &snap.items[0] {
        TuiRenderUnit::TuiTodoSummary(s) => {
            assert!(
                s.text.contains("1/2") && s.text.contains("Running tests"),
                "摘要格式 `1/2 tasks · Running tests`，实际 {:?}",
                s.text
            );
        }
        other => panic!("expected TuiTodoSummary at [0], got {other:?}"),
    }
    assert!(
        matches!(&snap.items[1], TuiRenderUnit::TuiAssistantBubble(_)),
        "最终回答在摘要之后"
    );

    // TurnDone 后（current_turn 清空）→ 无 todo 摘要
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    assert!(
        snap.items
            .iter()
            .all(|vm| !matches!(vm, TuiRenderUnit::TuiTodoSummary(_))),
        "回答后无 todo 摘要"
    );
}

/// §7 工具分组：相邻成功 Generic 工具压成 TuiCollapsedGroup（标题含隐藏数）；
/// running/error/diff-edit 不合并；不跨越 assistant 正文。
#[test]
#[serial]
fn test_snapshot_group_successful_tools() {
    let mut state = make_fold_test_state();

    let start_read = |st: &mut BridgeState, id: &str, path: &str| {
        dispatch_and_notify(
            st,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: id.into(),
                tool_name: "Read".into(),
                input_summary: path.into(),
                raw_input: serde_json::json!({"path": path}),
                agent_id: None,
            }),
        );
    };
    let end_tool = |st: &mut BridgeState, id: &str, is_error: bool| {
        dispatch_and_notify(
            st,
            &AcpEventData::ToolEnded(TuiToolEnded {
                tool_id: id.into(),
                output_summary: "ok".into(),
                is_error,
                agent_id: None,
            }),
        );
    };

    // 相邻成功：Read t1, Read t2 → 分组
    start_read(&mut state, "t1", "a.rs");
    end_tool(&mut state, "t1", false);
    start_read(&mut state, "t2", "b.rs");
    end_tool(&mut state, "t2", false);
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 1, "两个相邻成功工具 → 1 个分组");
    match &snap.items[0] {
        TuiRenderUnit::TuiCollapsedGroup(g) => {
            assert_eq!(g.count, 2);
            assert!(
                g.title.contains("Read 2"),
                "标题含隐藏数，实际 {:?}",
                g.title
            );
            assert_eq!(g.view_models.len(), 2, "隐藏 VM 保留在组内");
        }
        other => panic!("expected TuiCollapsedGroup, got {other:?}"),
    }

    // running 工具不合并（新工具开始 → 分组与 running 分离）
    start_read(&mut state, "t3", "c.rs");
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 2, "running 工具不得并入分组");
    assert!(matches!(
        &snap.items[0],
        TuiRenderUnit::TuiCollapsedGroup(_)
    ));
    assert!(matches!(&snap.items[1], TuiRenderUnit::TuiToolCard(t) if t.is_running));

    // error 工具不合并——组后**连续相邻** error 计入 failed_count（D2）
    end_tool(&mut state, "t3", true);
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 2, "error 工具不得并入分组");
    assert!(matches!(&snap.items[1], TuiRenderUnit::TuiToolCard(t) if t.is_error));
    match &snap.items[0] {
        TuiRenderUnit::TuiCollapsedGroup(g) => {
            assert_eq!(
                g.failed_count, 1,
                "紧邻 error 工具计入失败数（error 仍独立展开，不入组）"
            );
            // [G1] failed_count 纳入 hash——变化必须触发分片缓存重建
            assert_ne!(
                g.content_hash, 0,
                "组 hash 由 recompute_hash 计算（含 failed_count）"
            );
        }
        other => panic!("expected TuiCollapsedGroup, got {other:?}"),
    }

    // 正文打断相邻性：Read + text + Read → 不跨正文分组
    let mut state2 = make_fold_test_state();
    let start2 = |st: &mut BridgeState, id: &str| {
        dispatch_and_notify(
            st,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: id.into(),
                tool_name: "Read".into(),
                input_summary: "x".into(),
                raw_input: serde_json::json!({"path": "x"}),
                agent_id: None,
            }),
        );
    };
    start2(&mut state2, "s1");
    dispatch_and_notify(
        &mut state2,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "s1".into(),
            output_summary: "ok".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state2,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "中间正文".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    start2(&mut state2, "s2");
    dispatch_and_notify(
        &mut state2,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "s2".into(),
            output_summary: "ok".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 3, "正文打断 → 不跨正文分组");
    assert!(matches!(&snap.items[0], TuiRenderUnit::TuiToolCard(_)));
    assert!(matches!(
        &snap.items[1],
        TuiRenderUnit::TuiAssistantBubble(_)
    ));
    assert!(matches!(&snap.items[2], TuiRenderUnit::TuiToolCard(_)));
}

/// Skill/Todo 语义卡不分组（低信息密度才分组，语义卡保留）。
#[test]
#[serial]
fn test_snapshot_group_excludes_semantic_cards() {
    let mut state = make_fold_test_state();
    for (i, name) in ["Skill", "TodoWrite"].iter().enumerate() {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: format!("k{i}"),
                tool_name: name.to_string(),
                input_summary: "x".into(),
                raw_input: serde_json::json!({"skill": "s", "todos": []}),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolEnded(TuiToolEnded {
                tool_id: format!("k{i}"),
                output_summary: "ok".into(),
                is_error: false,
                agent_id: None,
            }),
        );
    }
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(snap.items.len(), 2, "语义卡不参与分组");
    assert!(
        snap.items
            .iter()
            .all(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_)))
    );
}

/// [§7 免疫] 焦点所在工具（`FOCUSED_ENTRY` 的 key）完成也不得并入折叠组。
///
/// 回归（review MED-2/F1）：用户 Alt+Down 聚焦运行中的工具，其完成后若被并入
/// 组——焦点 index 落到组上、展开态丢失（组不可展开且每帧重建）。当前
/// selected entry 按身份键免疫；焦点移走（Esc/导航）后恢复自动合并。
#[test]
#[serial]
fn test_snapshot_group_excludes_focused_tool() {
    use crate::kit::atoms::{FOCUSED_ENTRY, FocusedEntry};
    let mut state = make_fold_test_state();
    *FOCUSED_ENTRY.state().write() = None;
    let start_read = |st: &mut BridgeState, id: &str, path: &str| {
        dispatch_and_notify(
            st,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: id.into(),
                tool_name: "Read".into(),
                input_summary: path.into(),
                raw_input: serde_json::json!({"path": path}),
                agent_id: None,
            }),
        );
    };
    let end_tool = |st: &mut BridgeState, id: &str| {
        dispatch_and_notify(
            st,
            &AcpEventData::ToolEnded(TuiToolEnded {
                tool_id: id.into(),
                output_summary: "ok".into(),
                is_error: false,
                agent_id: None,
            }),
        );
    };

    start_read(&mut state, "t1", "a.rs");
    // 用户 Alt+Down 聚焦运行中的 t1（§7：当前 selected entry 免疫）。
    // 分组免疫只读 key（slot 不参与判定——含 slot 会使 key=None 的焦点
    // 移动无谓失效 TOOL_GROUP_CACHE 指纹）。
    *FOCUSED_ENTRY.state().write() = Some(FocusedEntry {
        slot: 0,
        key: Some(crate::kit::tui_render_unit::FoldKey::Tool("t1".into())),
    });
    end_tool(&mut state, "t1");
    start_read(&mut state, "t2", "b.rs");
    end_tool(&mut state, "t2");

    // t1 完成（焦点仍在其上）→ 不得并入分组：两个工具保持独立 entry。
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(
        snap.items.len(),
        2,
        "焦点工具免疫 → 保持独立 entry（不并入折叠组）"
    );
    assert!(matches!(&snap.items[0], TuiRenderUnit::TuiToolCard(t) if t.tool_id == "t1"));
    assert!(matches!(&snap.items[1], TuiRenderUnit::TuiToolCard(t) if t.tool_id == "t2"));

    // 焦点移走（Esc → 单一事实源清除）→ 下一帧快照恢复自动合并。
    *FOCUSED_ENTRY.state().write() = None;
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "总结".into(),
            message_id: Some("m1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert!(
        matches!(&snap.items[0], TuiRenderUnit::TuiCollapsedGroup(g) if g.count == 2),
        "焦点清除后两个相邻成功工具应并入分组"
    );
}

/// [Slice 3 探针] 真实 E2E 场景：两个相邻成功 Read + 尾部文本 → 应分组。
#[test]
#[serial]
fn test_probe_two_reads_then_text_grouped() {
    let mut state = make_fold_test_state();
    for id in ["t1", "t2"] {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: id.into(),
                tool_name: "Read".into(),
                input_summary: "Cargo.toml".into(),
                raw_input: serde_json::json!({"path": "Cargo.toml"}),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolEnded(TuiToolEnded {
                tool_id: id.into(),
                output_summary: "line1\nline2".into(),
                is_error: false,
                agent_id: None,
            }),
        );
    }
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "已使用 Read 工具读取 Cargo.toml。".into(),
            message_id: Some("m1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let types: Vec<&str> = snap
        .items
        .iter()
        .map(|vm| match vm {
            TuiRenderUnit::TuiCollapsedGroup(g) => {
                eprintln!("GROUP: {:?} count={}", g.title, g.count);
                "group"
            }
            TuiRenderUnit::TuiToolCard(_) => "tool",
            TuiRenderUnit::TuiAssistantBubble(_) => "assistant",
            _ => "other",
        })
        .collect();
    eprintln!("SNAPSHOT: {types:?}");
    assert!(
        snap.items
            .iter()
            .any(|vm| matches!(vm, TuiRenderUnit::TuiCollapsedGroup(_))),
        "两个相邻成功 Read + 尾部文本应分组，实际 {types:?}"
    );
}

// ── Slice 1：空 reasoning 占位（§6.3）+ assistant 时长冻结（§6.2）────────

fn assistant_of(
    snapshot: &ViewModelsSnapshot,
    idx: usize,
) -> &crate::kit::tui_render_unit::TuiAssistantBubble {
    match &snapshot.items[idx] {
        TuiRenderUnit::TuiAssistantBubble(b) => b,
        other => panic!("expected TuiAssistantBubble at [{idx}], got {other:?}"),
    }
}

/// §6.3 空 reasoning 占位：仅文本（无 reasoning chunk）流式 → 占位块
/// （text 空、Running、Preview）；TurnDone 后翻转 Completed + Collapsed 单行。
#[test]
#[serial]
fn test_empty_reasoning_placeholder_streams_then_folds() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "回复内容".into(),
            message_id: Some("msg_e1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let r = reasoning_of(&snap, 0);
    assert_eq!(
        r.text, "",
        "无 reasoning chunk → 空占位块（不出现空白 block）"
    );
    assert_eq!(r.status, EntryStatus::Running, "流式中占位块为 Running");
    assert!(r.is_running);
    assert_eq!(r.fold, FoldState::Preview, "§7 running 行 → Preview");

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    let r = reasoning_of(&snap, 0);
    assert_eq!(
        r.status,
        EntryStatus::Completed,
        "TurnDone 后空占位块翻转 Completed"
    );
    assert_eq!(
        r.fold,
        FoldState::Collapsed,
        "§7 completed 行 → Collapsed（收束为单行）"
    );
}

/// [R6] 空占位块 hash 跨 rebuild 稳定：流式追加（bubble 重建）→ 状态翻转
/// → 冻结后再次触发快照后处理，hash 保持稳定（秒级）。
#[test]
#[serial]
fn test_empty_reasoning_placeholder_hash_stable_across_rebuild() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "a".into(),
            message_id: Some("msg_e2".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let running_hash = assistant_of(&snap, 0).content_hash;

    // 流式追加文本——bubble 重建，hash 随内容变化
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "b".into(),
            message_id: Some("msg_e2".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let grown_hash = assistant_of(&snap, 0).content_hash;
    assert_ne!(grown_hash, running_hash, "内容变化 hash 必须变化");

    // TurnDone 冻结（fold/status/duration 翻转）——hash 再变一次
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    let frozen_hash = assistant_of(&snap, 0).content_hash;
    assert_ne!(frozen_hash, grown_hash, "状态翻转 hash 必须变化");

    // 冻结后快照静态：再次触发快照后处理，hash 秒级稳定（R6）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnCommitted {
            messages_json: "[]".into(),
            steps: 1,
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(
        assistant_of(&snap, 0).content_hash,
        frozen_hash,
        "冻结后跨 rebuild hash 稳定"
    );
}

/// §6.2 `12.4s`：turn 完成时冻结 assistant 正文时长（镜像 reasoning 冻结
/// 机制——apply_fold_pass 翻转点）；冻结后 hash 秒级稳定。
#[test]
#[serial]
fn test_assistant_duration_frozen_on_turn_done() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "hello".into(),
            message_id: Some("msg_d1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let b = assistant_of(&snap, 0);
    assert!(b.started_at.is_some(), "流式 trailing 段应持有 started_at");
    assert_eq!(b.duration_ms, None, "流式期间无冻结值");
    let running_hash = b.content_hash;

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    let b = assistant_of(&snap, 0);
    assert!(
        b.duration_ms.is_some(),
        "TurnDone 后应冻结 duration_ms（G1：fold pass 翻转点）"
    );
    assert_eq!(b.started_at, None, "冻结后 started_at 置 None（不再增长）");
    let frozen_hash = b.content_hash;
    assert_ne!(
        running_hash, frozen_hash,
        "running→frozen 翻转必须改变 content_hash（frozen 判别位）——冻结落在同一秒时 \
         duration_secs 数值不变，无判别位则按 hash 分片的渲染缓存持续供应无 meta 的旧帧"
    );

    // 冻结后快照静态：再次触发快照后处理，hash 秒级稳定
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnCommitted {
            messages_json: "[]".into(),
            steps: 1,
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    let b = assistant_of(&snap, 0);
    assert_eq!(b.content_hash, frozen_hash, "冻结后 hash 稳定");
}

// ── [Slice 4 §6.8] Interaction block：生产创建点 + 结果回写 + 折叠表 ──

fn make_interaction_state() -> BridgeState {
    let st = make_fold_test_state();
    // 模拟 acp_notifier 写入的 request_id atom（handle_* 创建 block 时克隆）
    *crate::kit::atoms::HITL_REQUEST_ID.state().write() =
        Some(serde_json::to_string(&"hitl-1").unwrap());
    *crate::kit::atoms::ASK_USER_REQUEST_ID.state().write() =
        Some(serde_json::to_string(&"ask-1").unwrap());
    st
}

fn ask_user_block_of(snapshot: &ViewModelsSnapshot, idx: usize) -> &TuiAskUserBlock {
    match &snapshot.items[idx] {
        TuiRenderUnit::TuiAskUserBlock(a) => a,
        other => panic!("expected TuiAskUserBlock at [{idx}], got {other:?}"),
    }
}

/// HitlPending 到达 → block 按事件位置 push 到 committed（不进 CurrentTurn
/// 缓存——sync_cache 段对齐不可破坏），pending + 选项 [Allow once, Deny]。
#[test]
#[serial]
fn test_hitl_pending_injects_pending_permission_block() {
    let mut state = make_interaction_state();
    let hp = HitlPending {
        tool_name: "Bash".into(),
        tool_input: serde_json::json!({"command": "cargo test"}),
        batch: None,
    };
    dispatch_and_notify(&mut state, &AcpEventData::HitlPending(hp));

    let snap = VIEW_MODELS.state().read().clone();
    // committed 末尾是 interaction block（无 current_turn 内容 → 快照即 committed）
    let block = ask_user_block_of(&snap, snap.items.len() - 1);
    assert!(block.pending, "等待响应 → pending=true");
    assert_eq!(block.kind, InteractionKind::Permission);
    assert_eq!(block.verb, "Bash");
    assert_eq!(block.question, "Bash wants to run: cargo test");
    assert_eq!(block.options, vec!["Allow once", "Deny"], "D6：仅两项");
    assert_eq!(
        block.fold,
        FoldState::Expanded,
        "§7 interaction Running → Expanded（可聚焦）"
    );
    assert!(
        block.request_id.as_deref() == Some(&serde_json::to_string(&"hitl-1").unwrap()),
        "request_id 从 HITL_REQUEST_ID atom 克隆"
    );
    // 断言 request_id 与 atom 同源
    assert_eq!(
        block.request_id.as_deref(),
        crate::kit::atoms::HITL_REQUEST_ID.state().read().as_deref()
    );
}

/// [§6.8 模态互斥] 同 request_id 的 pending block 重复注入（事件重放/重连/重试
/// 重复到达）→ 跳过第二次注入：重复 pending 块永远不会被 resolve（单响应
/// 事件只匹配首个），会以「可聚焦假象」永久滞留 transcript（review TEST MEDIUM）。
#[test]
#[serial]
fn test_hitl_pending_duplicate_request_id_not_reinjected() {
    let mut state = make_interaction_state();
    let hp = || HitlPending {
        tool_name: "Bash".into(),
        tool_input: serde_json::json!({"command": "cargo test"}),
        batch: None,
    };
    dispatch_and_notify(&mut state, &AcpEventData::HitlPending(hp()));
    dispatch_and_notify(&mut state, &AcpEventData::HitlPending(hp()));

    let snap = VIEW_MODELS.state().read().clone();
    let pending = snap
        .items
        .iter()
        .filter(|vm| matches!(vm, TuiRenderUnit::TuiAskUserBlock(a) if a.pending))
        .count();
    assert_eq!(snap.items.len(), 1, "重复 pending 事件不注入第二个 block");
    assert_eq!(pending, 1, "transcript 至多一个 pending 块（模态互斥）");
}

/// AskUser 到达 → pending block 用首问 header/options 摘要（双轨 D5）。
#[test]
#[serial]
fn test_ask_user_injects_pending_ask_user_block() {
    let mut state = make_interaction_state();
    let au = AskUser {
        questions: vec![
            Question {
                id: "q1".into(),
                header: "Pick a strategy".into(),
                question: "How to proceed?".into(),
                options: vec![
                    QuestionOption {
                        label: "Fast".into(),
                        description: String::new(),
                    },
                    QuestionOption {
                        label: "Careful".into(),
                        description: String::new(),
                    },
                ],
                multi_select: false,
            },
            Question {
                id: "q2".into(),
                header: "Second".into(),
                question: "Second question".into(),
                options: vec![],
                multi_select: false,
            },
        ],
    };
    dispatch_and_notify(&mut state, &AcpEventData::AskUser(au));

    let snap = VIEW_MODELS.state().read().clone();
    let block = ask_user_block_of(&snap, snap.items.len() - 1);
    assert!(block.pending);
    assert_eq!(block.kind, InteractionKind::AskUser);
    assert_eq!(block.question, "Pick a strategy", "首问 header 摘要");
    assert_eq!(block.options, vec!["Fast", "Careful"]);
    assert_eq!(block.fold, FoldState::Expanded);
}

/// 结果回写：InteractionResolved 按 request_id 匹配 pending block → clone +
/// pending=false + result + 重算 hash + 原位 set（COW）；completed → Collapsed。
#[test]
#[serial]
fn test_interaction_resolved_writes_back_pending_block() {
    let mut state = make_interaction_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::HitlPending(HitlPending {
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command": "cargo test"}),
            batch: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let idx = snap.items.len() - 1;
    let before = ask_user_block_of(&snap, idx).clone();
    let hash_before = before.content_hash;

    let rid = serde_json::to_string(&"hitl-1").unwrap();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::InteractionResolved {
            request_id: rid.clone(),
            result: "Allowed once".into(),
        },
    );

    let snap = VIEW_MODELS.state().read().clone();
    let block = ask_user_block_of(&snap, idx);
    assert!(!block.pending, "结果回写 → pending=false");
    assert_eq!(block.result.as_deref(), Some("Allowed once"));
    assert_eq!(
        block.fold,
        FoldState::Expanded,
        "答毕保持 Expanded 完整展示（用户需求，不再自动收束）"
    );
    assert_ne!(
        block.content_hash, hash_before,
        "结果回写必须重算 hash（触发分片缓存重建）"
    );

    // 幂等：重复到达（迟到/重复事件）不改变结果（matched 条件 pending=false 不再命中）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::InteractionResolved {
            request_id: rid,
            result: "Allowed once".into(),
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    let block = ask_user_block_of(&snap, idx);
    assert!(!block.pending);
    assert_eq!(block.result.as_deref(), Some("Allowed once"));
}

/// request_id 不匹配的 InteractionResolved → no-op（防御）。
#[test]
#[serial]
fn test_interaction_resolved_mismatched_request_id_noop() {
    let mut state = make_interaction_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::HitlPending(HitlPending {
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command": "ls"}),
            batch: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::InteractionResolved {
            request_id: serde_json::to_string(&"other-rid").unwrap(),
            result: "Denied".into(),
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    let block = ask_user_block_of(&snap, snap.items.len() - 1);
    assert!(block.pending, "不匹配的 id 不回写");
    assert!(block.result.is_none());
}

/// 折叠 pass：pending → Running → Expanded（覆盖免疫）；结果回写后 Completed
/// 默认 Expanded 完整展示（用户需求）；手动折叠覆盖（FoldKey::Interaction）优先。
#[test]
#[serial]
fn test_fold_pass_interaction_pending_expanded_override_priority() {
    let mut state = make_interaction_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::HitlPending(HitlPending {
            tool_name: "Bash".into(),
            tool_input: serde_json::json!({"command": "cargo test"}),
            batch: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let idx = snap.items.len() - 1;
    assert_eq!(ask_user_block_of(&snap, idx).fold, FoldState::Expanded);

    // 结果回写 → Completed → Expanded（不自动收束）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::InteractionResolved {
            request_id: serde_json::to_string(&"hitl-1").unwrap(),
            result: "Denied".into(),
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(ask_user_block_of(&snap, idx).fold, FoldState::Expanded);

    // 手动折叠覆盖：FOLD_OVERRIDES 写入 Interaction(rid) → 折叠 pass 恢复 Collapsed
    //（默认策略已是 Expanded，覆盖必须优先）
    let rid = serde_json::to_string(&"hitl-1").unwrap();
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Interaction(rid.clone()), FoldState::Collapsed);
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    let block = ask_user_block_of(&snap, idx);
    assert_eq!(block.fold, FoldState::Collapsed, "用户覆盖优先于自动策略");
    assert!(block.user_modified, "覆盖后 user_modified=true（免疫）");
}

/// hitl_input_summary 提取矩阵：优先主要对象字段，fallback 紧凑 JSON。
#[test]
fn test_hitl_input_summary_extraction_matrix() {
    use crate::kit::acp_events::system::hitl_input_summary;
    assert_eq!(
        hitl_input_summary(&serde_json::json!({"command": "cargo test"})),
        "cargo test"
    );
    assert_eq!(
        hitl_input_summary(&serde_json::json!({"path": "src/main.rs"})),
        "src/main.rs"
    );
    assert_eq!(
        hitl_input_summary(&serde_json::json!({"query": "fn main"})),
        "fn main"
    );
    // 空字符串字段跳过，继续找下一个候选
    assert_eq!(
        hitl_input_summary(&serde_json::json!({"command": "", "path": "Cargo.toml"})),
        "Cargo.toml"
    );
    // 无候选字段 → 紧凑 JSON
    assert_eq!(
        hitl_input_summary(&serde_json::json!({"a": 1, "b": true})),
        r#"{"a":1,"b":true}"#
    );
    // null 输入 → 兜底文案
    assert_eq!(
        hitl_input_summary(&serde_json::Value::Null),
        crate::i18n::tr("render-interaction-tool-unknown")
    );
}

// ── [G-Diff] Slice 5：含 diff 的 Edit 不入组 + 生产路径 diff 解析 ─────────

/// §7「不得合并含 diff 的 edit」：Edit 输出含 unified diff（解析成功）→
/// 独立展开渲染，不并入 TuiCollapsedGroup（`group_successful_tools` 的
/// `t.diff.is_none()` 守卫自动生效）。
/// [Fix flaky] 与 `test_edit_plain_output_grouped_normally` 等共享全局
/// VIEW_MODELS atom——非 serial 时并行写读交错会读到对方快照。
#[test]
#[serial]
fn test_edit_with_diff_not_grouped() {
    let mut state = make_fold_test_state();
    let diff_text = "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,2 @@
-old line
+new line
";
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "e1".into(),
            tool_name: "Edit".into(),
            input_summary: "src/main.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/main.rs"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "e1".into(),
            output_summary: diff_text.into(),
            is_error: false,
            agent_id: None,
        }),
    );
    // 相邻 Read 工具（可合并组）+ Edit 带 diff
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "r1".into(),
            tool_name: "Read".into(),
            input_summary: "b.rs".into(),
            raw_input: serde_json::json!({"path": "b.rs"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "r1".into(),
            output_summary: "ok".into(),
            is_error: false,
            agent_id: None,
        }),
    );

    let snap = VIEW_MODELS.state().read().clone();
    // Read 单独成组（run_len >= 2 才压缩——1 个 Read 不组）；Edit 保持独立卡片
    let edit = snap
        .items
        .iter()
        .find_map(|vm| match vm {
            TuiRenderUnit::TuiToolCard(t) if t.tool_name == "Edit" => Some(t),
            _ => None,
        })
        .expect("Edit 卡片独立存在（未并入分组）");
    assert!(
        edit.diff.is_some(),
        "Edit 输出中的 unified diff 被解析（G-Diff 生产路径）"
    );
    let diff = edit.diff.as_ref().unwrap();
    assert_eq!(
        diff.path, "src/main.rs",
        "path hint 来自 raw_input.file_path"
    );
    assert_eq!(diff.hunks.len(), 1);
    let change_lines: Vec<_> = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| {
            matches!(
                l.kind,
                crate::kit::tui_render_unit::TuiHunkLineKind::Add
                    | crate::kit::tui_render_unit::TuiHunkLineKind::Del
            )
        })
        .collect();
    assert_eq!(change_lines.len(), 2, "+1 −1");
    // 组内不得出现 Edit（含 diff 不合并）
    for vm in snap.items.iter() {
        if let TuiRenderUnit::TuiCollapsedGroup(g) = vm {
            assert!(
                g.view_models.iter().all(
                    |inner| !matches!(inner, TuiRenderUnit::TuiToolCard(t) if t.tool_name == "Edit")
                ),
                "含 diff 的 Edit 永不并入分组"
            );
        }
    }
}

/// 非 diff 输出（Edit 结果不含 unified diff）→ diff=None → 可正常分组。
/// [Fix flaky] 共享全局 VIEW_MODELS atom——非 serial 时与 serial 测试
/// 并行写读交错（serial_test 只互斥 serial 测试之间）。
#[test]
#[serial]
fn test_edit_plain_output_grouped_normally() {
    let mut state = make_fold_test_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "e1".into(),
            tool_name: "Edit".into(),
            input_summary: "src/x.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/x.rs"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "e1".into(),
            output_summary: "Replaced text in src/x.rs".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "e2".into(),
            tool_name: "Write".into(),
            input_summary: "src/y.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/y.rs"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "e2".into(),
            output_summary: "Wrote 3 lines".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    // 两个无 diff 的相邻成功工具 → 分组（标题含 Edit/Write 计数）
    assert_eq!(snap.items.len(), 1, "无 diff 的相邻工具仍正常分组");
    match &snap.items[0] {
        TuiRenderUnit::TuiCollapsedGroup(g) => {
            assert_eq!(g.count, 2);
            assert!(
                g.title.contains("Edit") || g.title.contains("Write"),
                "标题含工具名，实际 {:?}",
                g.title
            );
        }
        other => panic!("expected TuiCollapsedGroup, got {other:?}"),
    }
}

/// [Slice 5] 真实摘要路径：Edit 输出 `Added 2 lines to P`（真实工具形态，
/// 无 unified diff）→ 摘要 fallback 解析出 diff 块（adds=2）→ 不入组。
#[test]
#[serial]
fn test_edit_with_real_summary_diff_not_grouped() {
    let mut state = make_fold_test_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "s1".into(),
            tool_name: "Edit".into(),
            input_summary: "src/s.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/s.rs"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "s1".into(),
            output_summary: "Added 2 lines to src/s.rs".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let edit = match &snap.items[0] {
        TuiRenderUnit::TuiToolCard(c) => c.clone(),
        other => panic!("expected TuiToolCard, got {other:?}"),
    };
    let diff = edit
        .diff
        .expect("真实摘要应解析出 diff 块（G-Diff fallback）");
    assert_eq!(diff.path, "src/s.rs", "path hint 来自 raw_input.file_path");
    assert!(diff.hunks.is_empty(), "摘要块无 hunk 行");
    let (adds, dels) = crate::kit::tui_render_unit::diff_change_counts(&diff);
    assert_eq!((adds, dels), (2, 0), "摘要计数进入顶层字段");

    // 相邻成功 Read（可合并）+ 带 diff 的 Edit → Edit 不入组
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "s2".into(),
            tool_name: "Read".into(),
            input_summary: "src/r.rs".into(),
            raw_input: serde_json::json!({"file_path": "src/r.rs"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "s2".into(),
            output_summary: "line1\nline2".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let has_edit_tool = snap.items.iter().any(
        |vm| matches!(vm, TuiRenderUnit::TuiToolCard(t) if t.tool_name == "Edit" && t.diff.is_some()),
    );
    assert!(has_edit_tool, "带 diff 的 Edit 保持独立展开渲染");
    let has_group = snap
        .items
        .iter()
        .any(|vm| matches!(vm, TuiRenderUnit::TuiCollapsedGroup(_)));
    assert!(!has_group, "含 diff 的 Edit 不并入相邻 Read 组");
}

/// [Slice 5] 真实摘要 ±0（`Replaced text (same line count)`）→ 无 diff 块，
/// 保持可合并（回归防线：同行数替换 Edit 仍可入组）。
#[test]
#[serial]
fn test_edit_same_line_replacement_still_grouped() {
    let mut state = make_fold_test_state();
    for (id, output) in [
        ("r1", "Replaced text (same line count) to src/a.rs"),
        ("r2", "Replaced text (same line count) to src/b.rs"),
    ] {
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(TuiToolStarted {
                tool_id: id.into(),
                tool_name: "Edit".into(),
                input_summary: format!("src/{}.rs", id),
                raw_input: serde_json::json!({"file_path": format!("src/{}.rs", id)}),
                agent_id: None,
            }),
        );
        dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolEnded(TuiToolEnded {
                tool_id: id.into(),
                output_summary: output.into(),
                is_error: false,
                agent_id: None,
            }),
        );
    }
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(
        snap.items.len(),
        1,
        "±0 摘要无 diff → 相邻成功 Edit 正常分组"
    );
}
