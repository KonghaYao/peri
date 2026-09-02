//! Turn lifecycle event handlers — TurnDone, TurnInterrupted, TurnSuspended,
//! TurnCommitted, PromptStarted/Submitted, SessionReplayStarted/Done,
//! LocalUserBubble, BgCallbackBubble, CommittedAssistantText.

use super::*;
use crate::kit::acp_types::CurrentTurn;
use crate::kit::atoms::{PERI_CONFIG_HANDLE, RENDER_HEARTBEAT, THREAD_LOAD_TX};
use crate::kit::tui_render_unit::{
    TuiAssistantBubble, TuiReasoningBlock, TuiRenderUnit, TuiUserBubble,
};

pub(super) fn handle_turn_done(state: &mut BridgeState) {
    // H3: TurnDone 仅做两件事：
    // (a) current_turn.view_models() → 逐条 push_back 到 committed
    // (b) current_turn.reset() + push_view_models
    // buffered_text 已由 LocalUserBubble 事件提前入队 committed，
    // TurnDone 不再代为搬运。
    let _ = state.pending_cache_usage.take();
    state.flush_current_turn();
    state.last_pushed_text_len = 0;
    state.last_pushed_reasoning_len = 0;
    state.variant = 0;
    // Issue 2026-08-05 次要项 (b)：last_submitted_text 是"最近一次用户提交"的
    // 回滚锚点，只对运行中的 turn 有效。TurnDone 后本 turn 已结束，不清除会让
    // 旧文本跨 turn 残留——后续到达的 stale TurnInterrupted 会误删不相关气泡。
    state.last_submitted_text = None;

    state.phase = SessionPhase::Idle;

    tracing::info!(
        is_loading = state.phase == SessionPhase::PromptRunning,
        committed_len = state.committed.len(),
        current_turn_empty = state.current_turn.is_empty(),
        "TurnDone: writing ACP_STATE"
    );

    super::render::push_view_models(state);
    super::render::push_acp_state(state);

    // C2: compact 命令完成后触发 session/load 重放。
    // S4.1 方案 B：命令 compact（Immediate）后无流事件，标志保持到 TurnDone；
    // agent 内部 auto-compact 后标志已被后续流事件清除（见 dispatch_and_notify
    // 入口的流事件判定）。原 `current_turn.is_empty()` 判定失效——flush 之后
    // current_turn 恒空、失去区分能力（Issue 2026-08-05），已删除。
    if state.compact_just_completed {
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

    // (g) C1: agent 完成本轮——drain INPUT_BUFFER，按顺序重新提交。
    // compact replay 必须先同步取得 load reservation，否则 drain 出来的首条
    // prompt 仍可能抢在异步 session/load consumer 前绑定旧 Stable session。
    super::render::drain_input_buffer();
}

pub(super) fn handle_cache_usage_updated(
    state: &mut BridgeState,
    sample: &Option<crate::kit::acp_types::CacheUsageSample>,
) {
    state.pending_cache_usage = sample.clone();
    if let Some(sample) = sample {
        let show_warning = PERI_CONFIG_HANDLE
            .get()
            .map(|handle| handle.read().config.show_cache_warning.unwrap_or(false))
            .unwrap_or(false);
        inject_cache_coverage_warning_if_needed(state, sample, show_warning);
    }
}

/// Turn-end helper for tests; production warnings emit on each root `usage_update`.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn finalize_cache_coverage(state: &mut BridgeState, show_warning: bool) {
    if let Some(sample) = state.pending_cache_usage.take() {
        inject_cache_coverage_warning_if_needed(state, &sample, show_warning);
    }
}

fn inject_cache_coverage_warning_if_needed(
    state: &mut BridgeState,
    sample: &crate::kit::acp_types::CacheUsageSample,
    show_warning: bool,
) {
    if sample.input_tokens == 0
        || sample.cached_tokens == 0
        || sample.cached_tokens > sample.input_tokens
    {
        return;
    }
    let coverage = sample.cached_tokens as f64 / sample.input_tokens as f64;
    if show_warning && coverage < 0.8 {
        use fluent_bundle::FluentValue;
        let pct = (coverage * 100.0) as u64;
        let uncached = sample.input_tokens.saturating_sub(sample.cached_tokens);
        let request_id = sample.request_id.as_deref().unwrap_or("-");
        let text = crate::i18n::tr_args(
            "app-note-cache-coverage-low",
            &[
                ("pct".into(), FluentValue::from(pct)),
                ("cached".into(), FluentValue::from(sample.cached_tokens)),
                ("input".into(), FluentValue::from(sample.input_tokens)),
                ("uncached".into(), FluentValue::from(uncached)),
                ("req_id".into(), FluentValue::from(request_id)),
            ],
        );
        state.inject_system_note(text, crate::kit::tui_render_unit::TuiNoteLevel::Warning);
    }
}

pub(super) fn handle_turn_interrupted(
    state: &mut BridgeState,
    _reason: &str,
    request_id: &Option<String>,
) {
    // Issue 2026-08-05（返工）：stale 判定 = request_id 配对（主导排序）
    // OR 代际判定（排队分支兜底），两条互补：
    //
    // - request_id 配对：事件携带 request_id 且 ≠ 当前 turn 的 request_id →
    //   stale。覆盖"新提交已发 RPC（PromptSubmitted 先到）而旧 turn 的
    //   TurnInterrupted 晚到"的主导排序场景——v1 仅靠代际判定在此场景失效
    //   （last_prompt_generation 已被新提交刷新，N+1 > N+1 为 false）。
    // - 代际判定：turn_generation > last_prompt_generation → stale。覆盖
    //   排队分支——B 仅 LocalUserBubble（无 RPC、无 request_id），
    //   current_request_id 停留在 A，id 配对判不出 stale，代际判定兜底。
    //
    // 正常取消（无新提交）两条都不命中 → 非 stale → 走零产出回滚。
    let id_mismatch = request_id
        .as_ref()
        .is_some_and(|rid| state.current_request_id.as_ref() != Some(rid));
    let is_stale = id_mismatch || state.turn_generation > state.last_prompt_generation;
    if is_stale {
        tracing::info!(
            turn_generation = state.turn_generation,
            last_prompt_generation = state.last_prompt_generation,
            id_mismatch,
            request_id = ?request_id,
            current_request_id = ?state.current_request_id,
            "TurnInterrupted: stale (belongs to an older turn), skipping zero-output rollback"
        );
        // stale 分支：只归档旧 turn 已产出内容 + 复位状态，不执行回滚副作用
        // （不删新气泡、不恢复文本）。INPUT_BUFFER 排队输入是用户已提交的新
        // 请求（loading 期间排队，LocalUserBubble 已显示），不得静默丢弃——
        // 复位后立即 drain 按序提交（见下方 drain_input_buffer 调用）。
        // last_submitted_text **保留**——它指向最近一次提交（新 turn 的锚点），
        // 后续该 turn 被取消（连续取消场景）时零产出回滚仍需它恢复输入文本；
        // 旧 turn 的锚点残留风险由 TurnDone 清空（handle_turn_done）兜底。
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
        // Issue 2026-08-05 遗留项（中）：stale 分支复位后主动 drain 排队输入。
        // 旧 turn 已取消、其 TurnDone 永不到达——排队输入（用户 loading 期间
        // 提交的请求）若无触发器会永久悬挂；若用户随后提交新内容 C，排队输入
        // 要等 C 的 TurnDone 才被 drain，执行顺序从 B→C 反转为 C→B。复位后
        // （is_loading=false）立即按序提交，语义与 TurnDone 路径的
        // drain_input_buffer 完全一致：每条 SubmitRequest::AgentText →
        // submit_consumer 生成新 request_id → PromptSubmitted → prompt RPC
        // （request_id 由 submit_consumer 生成，与 handle_prompt_submitted 的
        // current_request_id 记录自动对齐，此处不自行生成）。不递增
        // turn_generation（不重复发 LocalUserBubble——气泡已显示）。
        // 边界：buffer 为空时 no-op；多次 stale 事件第二次起 buffer 已空，
        // 不会重复提交；drain 后用户手动提交经 SUBMIT_TX FIFO 顺序追加。
        super::render::drain_input_buffer();
        return;
    }

    state.pending_cache_usage = None;

    // 零产出回滚：Agent 尚未产出任何 AI 内容时（current_turn 为空），
    // 撤销本次用户气泡 + 恢复文本到输入框。
    // 仅当有 last_submitted_text 时才执行（正常情况下 LocalUserBubble 已到达；
    // [Slice 3] loading 提交只入队不发气泡，不设置 last_submitted_text——
    // 回滚不会误恢复排队中的输入）。
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
        // [Slice 3 D4] 排队项是用户已提交的请求（composer 上方队列可见），
        // 取消当前 turn 不吞排队项——复位后立即 drain 提交（与 stale 分支
        // 行为对齐；stale 判定靠代际/request_id，不依赖本分支）。
        super::render::drain_input_buffer();
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
    // [Slice 3 D4] 归档分支同样 drain 排队项（不丢弃）：排队输入是用户已提交
    // 的请求，取消本 turn 后继续按序发送，与 stale 分支行为一致。
    super::render::drain_input_buffer();
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

pub(super) fn handle_prompt_submitted(state: &mut BridgeState, request_id: &Option<String>) {
    // submit_consumer 在 prompt RPC 之前发出此事件，让 bridge 统一管理 loading 状态。
    state.phase = SessionPhase::PromptRunning;
    state.variant = 1;
    // Issue 2026-08-05: 记录"已真正发出 prompt RPC"时的代际快照——
    // TurnInterrupted 的 stale 判定以 turn_generation > last_prompt_generation
    // 为依据（存在已显示气泡但请求未发出/晚到的更新提交）。
    state.last_prompt_generation = state.turn_generation;
    // Issue 2026-08-05 返工：记录当前 turn 的 request_id——stale TurnInterrupted
    // 的 request_id 配对判定基准（主导排序场景，见 handle_turn_interrupted 注释）。
    state.current_request_id = request_id.clone();
    state.pending_cache_usage = None;
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
    // Issue 2026-08-05: 每次用户可见提交递增 turn 代际——这是 stale TurnInterrupted
    // 判定的基准（注意 session replay 的 user_message_chunk 也走本变体，但 replay
    // 期间无 turn 运行、无 TurnInterrupted 到达，递增不会造成误判）。
    state.turn_generation = state.turn_generation.wrapping_add(1);
    state
        .committed
        .push_back(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(
            text.to_string(),
        )));
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}

/// S4.2: 本地 loading 复位请求（cancel / /clear / prompt 失败兜底，由
/// submit_consumer 注入 LOCAL_EVENT_TX）。幂等：phase 非 PromptRunning 时
/// no-op（不 push）——命令 compact、replay、正常提交等场景不受影响；phase
/// 为 PromptRunning 时复位为 Idle 并重推 ACP_STATE，防止后续事件触发
/// push_acp_state 时用 phase 重算 is_loading=true 造成取消后 loading 闪回
/// （Issue 2026-08-05 S4.2）。
pub(super) fn handle_loading_reset(state: &mut BridgeState) {
    if state.phase == SessionPhase::PromptRunning {
        state.phase = SessionPhase::Idle;
        super::render::push_acp_state(state);
    }
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
    // 完整消息（非流式）→ reasoning 视为已完成：按 §7 表折叠为单行。
    // [G1] hash 统一走 TuiAssistantBubble::compute_hash（含 fold/status）。
    let reason_block = reasoning.as_ref().map(|r| TuiReasoningBlock {
        text: r.clone(),
        fold: crate::kit::tui_render_unit::fold_for_status(
            crate::kit::tui_render_unit::FoldTarget::Reasoning,
            crate::kit::tui_render_unit::EntryStatus::Completed,
        ),
        status: crate::kit::tui_render_unit::EntryStatus::Completed,
        is_running: false,
        // 非流式通道无推理起始时间——完成时长不可得（G-Tokens 降级：
        // 仅显示行数，省略 `Thought for Ns` 时长）。
        started_at: None,
        duration_ms: None,
    });
    let mut bubble = TuiAssistantBubble {
        text: text.to_string(),
        reasoning: reason_block,
        // 非流式通道无 message_id 来源（重放/commit 路径）——不可作为
        // FoldKey::Reasoning 覆盖目标，Slice 2 接受此限制。
        message_id: None,
        // 非流式通道无文本起始时刻——正文时长不可得（G-Tokens 降级：
        // 不显示 `12.4s`）。
        started_at: None,
        duration_ms: None,
        content_hash: 0,
    };
    bubble.recompute_hash();
    let vm = TuiRenderUnit::TuiAssistantBubble(bubble);
    state.committed.push_back(vm);
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}
