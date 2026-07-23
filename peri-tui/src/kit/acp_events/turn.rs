//! Turn lifecycle event handlers — TurnDone, TurnInterrupted, TurnSuspended,
//! TurnCommitted, PromptStarted/Submitted, SessionReplayStarted/Done,
//! LocalUserBubble, BgCallbackBubble, CommittedAssistantText.

use super::*;
use crate::kit::acp_types::CurrentTurn;
use crate::kit::atoms::{INPUT_BUFFER, RENDER_HEARTBEAT, THREAD_LOAD_TX};
use crate::kit::tui_render_unit::{
    TuiAssistantBubble, TuiReasoningBlock, TuiRenderUnit, TuiUserBubble, tui_hash_str,
};

pub(super) fn handle_turn_done(state: &mut BridgeState) {
    // H3: TurnDone 仅做两件事：
    // (a) current_turn.view_models() → 逐条 push_back 到 committed
    // (b) current_turn.reset() + push_view_models
    // buffered_text 已由 LocalUserBubble 事件提前入队 committed，
    // TurnDone 不再代为搬运。
    state.flush_current_turn();
    state.last_pushed_text_len = 0;
    state.last_pushed_reasoning_len = 0;
    state.variant = 0;

    state.phase = SessionPhase::Idle;

    tracing::info!(
        is_loading = state.phase == SessionPhase::PromptRunning,
        committed_len = state.committed.len(),
        current_turn_empty = state.current_turn.is_empty(),
        "TurnDone: writing ACP_STATE"
    );

    super::render::push_view_models(state);
    super::render::push_acp_state(state);

    // (g) C1: agent 完成本轮——drain INPUT_BUFFER，按顺序重新提交。
    super::render::drain_input_buffer();

    // C2: compact 命令完成后触发 session/load 重放。
    // 区分 agent 内部 compact：命令 compact（Immediate）后无后续流事件，
    // current_turn 为空；agent 内部 compact 后 current_turn 有内容。
    if state.compact_just_completed && state.current_turn.is_empty() {
        state.compact_just_completed = false;
        if let Some(tx) = THREAD_LOAD_TX.get() {
            let session_id = state.active_session_id.clone();
            tracing::info!(
                session_id = %session_id,
                "TurnDone: compact completed, triggering session/load replay"
            );
            let _ = tx.send(session_id);
        }
    }
}

pub(super) fn handle_turn_interrupted(state: &mut BridgeState, _reason: &str) {
    // 零产出回滚：Agent 尚未产出任何 AI 内容时（current_turn 为空），
    // 撤销本次用户气泡 + 恢复文本到输入框。
    // 仅当有 last_submitted_text 时才执行（正常情况下 LocalUserBubble 已到达）。
    if state.current_turn.is_empty() && state.last_submitted_text.is_some() {
        let restore_text = state.last_submitted_text.take().unwrap();
        // 移除 committed 中最后一条用户气泡
        if let Some(last) = state.committed.last()
            && matches!(last, TuiRenderUnit::TuiUserBubble(_))
        {
            let last_idx = state.committed.len().saturating_sub(1);
            state.committed.remove(last_idx);
        }
        // 将文本放入恢复存储，递增 RENDER_HEARTBEAT 触发 input_area 重渲染
        let mu =
            crate::kit::atoms::INPUT_RESTORE_TEXT.get_or_init(|| parking_lot::Mutex::new(None));
        *mu.lock() = Some(restore_text);
        RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
        // 清除排队输入缓冲——取消后不应继续处理排队的输入
        INPUT_BUFFER.state().write().clear();
        state.current_turn = CurrentTurn::new();
        state.last_pushed_text_len = 0;
        state.last_pushed_reasoning_len = 0;
        state.variant = 0;
        state.phase = SessionPhase::Idle;
        super::render::push_view_models(state);
        super::render::push_acp_state(state);
        return;
    }

    // 守卫：仅当 current_turn 有未归档内容时才归档
    if !state.current_turn.committed && !state.current_turn.is_empty() {
        state.current_turn.deactivate();
        for vm in state.current_turn.view_models() {
            state.committed.push_back(vm.clone());
        }
    }
    state.current_turn = CurrentTurn::new();
    state.last_pushed_text_len = 0;
    state.last_pushed_reasoning_len = 0;
    state.variant = 0;
    state.phase = SessionPhase::Idle;
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}

pub(super) fn handle_turn_suspended(state: &mut BridgeState) {
    // Turn 挂起（idle/await_wake）——与 TurnDone 类似但 Agent 保持存活。
    // 归档 current_turn → committed，停止 loading，但不 drain_input_buffer
    // （Agent 还在 await_wake，新 turn 的流事件会自动恢复 loading）
    if !state.current_turn.committed && !state.current_turn.is_empty() {
        state.current_turn.deactivate();
        for vm in state.current_turn.view_models() {
            state.committed.push_back(vm.clone());
        }
    }
    state.current_turn.reset();
    state.last_pushed_text_len = 0;
    state.last_pushed_reasoning_len = 0;
    state.variant = 0;
    state.phase = SessionPhase::Idle;
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
    // 注意：不调用 drain_input_buffer()——Agent 保持存活，
    // 输入缓冲在 Agent 真正完成（TurnDone）时再处理。
}

pub(super) fn handle_turn_committed(state: &mut BridgeState, steps: usize) {
    tracing::info!(steps, "bridge: TurnCommitted ({steps} steps)");
    // 在 goal 自驱场景下 TurnDone 只在最终循环退出时触发，
    // TurnCommitted 作为每次 ReAct 迭代边界的刷新检查点，防止 TUI atom 漂移。
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}

pub(super) fn handle_prompt_started(state: &mut BridgeState) {
    tracing::trace!("dead path: PromptStarted not emitted by notifier");
    state.phase = SessionPhase::PromptRunning;
    state.variant = 1;
    super::render::push_acp_state(state);
}

pub(super) fn handle_prompt_submitted(state: &mut BridgeState) {
    // submit_consumer 在 prompt RPC 之前发出此事件，让 bridge 统一管理 loading 状态。
    state.phase = SessionPhase::PromptRunning;
    state.variant = 1;
    super::render::push_acp_state(state);
}

pub(super) fn handle_session_replay_started(state: &mut BridgeState) {
    tracing::trace!("dead path: SessionReplayStarted not emitted by notifier");
    state.phase = SessionPhase::ReplayingHistory;
    state.variant = 0;
    state.current_turn.reset();
    state.last_pushed_text_len = 0;
    state.last_pushed_reasoning_len = 0;
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}

pub(super) fn handle_session_replay_done(state: &mut BridgeState) {
    tracing::trace!("dead path: SessionReplayDone not emitted by notifier");
    if state.phase == SessionPhase::ReplayingHistory {
        state.phase = SessionPhase::Idle;
    }
    state.variant = 0;
    state.current_turn.reset();
    state.last_pushed_text_len = 0;
    state.last_pushed_reasoning_len = 0;
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}

pub(super) fn handle_local_user_bubble(state: &mut BridgeState, text: &str) {
    state.last_submitted_text = Some(text.to_string());
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            text.to_string(),
        )));
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}

pub(super) fn handle_bg_callback_bubble(state: &mut BridgeState) {
    // bg callback flush-only：把 current_turn 归档到 committed，
    // 但不 push bg 回调气泡本身。气泡由标准 session/update 通道的
    // LocalUserBubble 负责推送。这样保证：
    // ① current_turn 内容在前（flush）
    // ② bg 回调气泡在中间（LocalUserBubble 随后到达）
    // ③ 后续 AI 内容在后（TurnDone 归档）
    state.flush_current_turn();
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}

pub(super) fn handle_committed_assistant_text(
    state: &mut BridgeState,
    text: &str,
    reasoning: &Option<String>,
) {
    let reason_block = reasoning.as_ref().map(|r| TuiReasoningBlock {
        text: r.clone(),
        collapsed: false,
    });
    let content_hash = tui_hash_str(&format!("{}|{}", text, reasoning.as_deref().unwrap_or("")));
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: text.to_string(),
        reasoning: reason_block,
        content_hash,
    });
    state.committed.push_back(vm);
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}
