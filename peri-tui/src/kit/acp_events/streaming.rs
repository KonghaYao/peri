//! Streaming event handlers — TextChunk, ReasoningChunk.

use super::*;
use crate::kit::atoms::BG_AGENT_IDS;
use crate::kit::bg_task_live::{append_bg_reasoning_chunk, append_bg_text_chunk};
use crate::kit::stream_data::{TuiReasoningChunk, TuiTextChunk};

/// 路由失败的 chunk 是否属于仍在运行的 bg subagent（BG_AGENT_IDS 已注册）。
///
/// bg subagent 的生命周期跨越主 turn 边界：TurnSuspended/TurnInterrupted 会
/// 无条件 reset current_turn（清空 SubAgentAccumulator，`flush_current_turn`
/// 的 running-subagent 守卫覆盖不到这两条路径），此后 bg 的流式 chunk 找不到
/// 组。与 tool.rs 的 bg 兜底同口径：bg 内容不进主消息区——命中则跳过（不
/// 回退主 agent 分支），否则外溢到主回复气泡。
fn is_bg_agent_without_group(agent_id: &str) -> bool {
    let is_bg = BG_AGENT_IDS.state().read().contains(agent_id);
    if is_bg {
        tracing::debug!(
            target: "tui.acp_events",
            agent_id = %agent_id,
            "chunk: bg subagent 组已被 turn 边界清除，跳过（不进入主回复）"
        );
    }
    is_bg
}

pub(super) fn handle_text_chunk(state: &mut BridgeState, tc: &TuiTextChunk) -> PublicationIntent {
    if tc.text.is_empty() {
        return PublicationIntent::None;
    }
    let first = first_chunk(
        state,
        tc.agent_id.as_deref(),
        tc.message_id.as_deref(),
        false,
    );
    // 先尝试 SubAgent 组路由；带 agent_id 但无匹配组 = 主 agent 文本
    // （v2 事件身份透传后主 agent chunk 亦携带 agent_id，`append_subagent_text`
    // 找不到组即回退主 agent 分支，不能静默丢弃——否则主 agent 回复不显示）。
    // bg subagent 例外：组被 turn 边界清除时不得回退主分支（见
    // `is_bg_agent_without_group`）。
    let routed_to_subagent = tc
        .agent_id
        .as_deref()
        .is_some_and(|agent_id| state.current_turn.append_subagent_text(agent_id, &tc.text));
    if routed_to_subagent {
        state.variant = 1;
        // bg subagent 不触碰 phase（Issue 2026-08-12）：bg 生命周期跨越主 turn
        // 边界，TurnSuspended 后 phase=Idle 不得被 bg 流式事件拉回 PromptRunning
        // （loading 残留）。主 agent 推理期间 phase 由主 agent 自身事件维持，
        // bg 事件无需参与。sync subagent 不在 BG_AGENT_IDS，维持原行为。
        let is_bg = tc
            .agent_id
            .as_deref()
            .is_some_and(|id| BG_AGENT_IDS.state().read().contains(id));
        if is_bg && let Some(agent_id) = tc.agent_id.as_deref() {
            append_bg_text_chunk(agent_id, tc);
        }
        if !is_bg {
            state.phase = SessionPhase::PromptRunning;
        }
    } else if tc
        .agent_id
        .as_deref()
        .is_some_and(is_bg_agent_without_group)
    {
        if let Some(agent_id) = tc.agent_id.as_deref() {
            append_bg_text_chunk(agent_id, tc);
        }
        state.variant = 1;
        super::render::push_acp_state(state);
        return PublicationIntent::None;
    } else {
        state
            .current_turn
            .append_text(&tc.text, tc.message_id.as_deref());
        state.variant = 1;
        state.phase = SessionPhase::PromptRunning;
    }
    super::render::push_acp_state(state);
    stream_intent(state, first, routed_to_subagent, false)
}

pub(super) fn handle_reasoning_chunk(
    state: &mut BridgeState,
    rc: &TuiReasoningChunk,
) -> PublicationIntent {
    if rc.text.is_empty() {
        return PublicationIntent::None;
    }
    let first = first_chunk(
        state,
        rc.agent_id.as_deref(),
        rc.message_id.as_deref(),
        true,
    );
    // 同 handle_text_chunk：subagent 路由失败时回退主 agent 推理分支
    // （主 agent thinking chunk 亦携带 agent_id，不能静默丢弃）。
    let routed_to_subagent = rc.agent_id.as_deref().is_some_and(|agent_id| {
        state
            .current_turn
            .append_subagent_reasoning(agent_id, &rc.text)
    });
    if routed_to_subagent {
        state.variant = 1;
        // bg subagent 不触碰 phase（Issue 2026-08-12，同 handle_text_chunk）。
        let is_bg = rc
            .agent_id
            .as_deref()
            .is_some_and(|id| BG_AGENT_IDS.state().read().contains(id));
        if is_bg && let Some(agent_id) = rc.agent_id.as_deref() {
            append_bg_reasoning_chunk(agent_id, rc);
        }
        if !is_bg {
            state.phase = SessionPhase::PromptRunning;
        }
    } else if rc
        .agent_id
        .as_deref()
        .is_some_and(is_bg_agent_without_group)
    {
        if let Some(agent_id) = rc.agent_id.as_deref() {
            append_bg_reasoning_chunk(agent_id, rc);
        }
        state.variant = 1;
        super::render::push_acp_state(state);
        return PublicationIntent::None;
    } else {
        state
            .current_turn
            .append_reasoning(&rc.text, rc.message_id.as_deref());
        // [Diagnostic] 每 token 调用一次，仅排查流式推理累积问题时需要——
        // trace 级别避免默认 info filter 下同步写滚动文件。
        tracing::trace!(
            len = state.current_turn.reasoning.len(),
            "bridge: reasoning appended"
        );
        state.variant = 1;
        state.phase = SessionPhase::PromptRunning;
    }
    super::render::push_acp_state(state);
    stream_intent(state, first, routed_to_subagent, true)
}

/// 子流沿现有 occurrence/segment 路由；不把主 Agent 的空字符串当子流首块。
fn first_chunk(
    state: &BridgeState,
    agent_id: Option<&str>,
    message_id: Option<&str>,
    reasoning: bool,
) -> bool {
    if let Some(child) = agent_id.and_then(|id| {
        state
            .current_turn
            .subagents
            .iter()
            .rev()
            .find(|s| s.agent_id == id)
    }) {
        // 既有子流 API 没有透传 message_id，使用 occurrence 内的 segment 边界。
        child.child_turn.starts_stream_block(None, reasoning)
    } else {
        state
            .current_turn
            .starts_stream_block(message_id, reasoning)
    }
}

fn stream_intent(
    state: &mut BridgeState,
    first: bool,
    subagent: bool,
    reasoning: bool,
) -> PublicationIntent {
    match current_streaming_mode() {
        StreamingMode::None => PublicationIntent::Hidden,
        StreamingMode::Streaming => {
            if first {
                PublicationIntent::Immediate
            } else {
                PublicationIntent::Streaming
            }
        }
        StreamingMode::Block if subagent => {
            if first {
                PublicationIntent::Immediate
            } else {
                PublicationIntent::Streaming
            }
        }
        StreamingMode::Block => {
            let (text, pushed) = if reasoning {
                (
                    &state.current_turn.reasoning,
                    &mut state.last_pushed_reasoning_len,
                )
            } else {
                (&state.current_turn.text, &mut state.last_pushed_text_len)
            };
            if has_md_block_boundary_since(text, *pushed) {
                *pushed = text.chars().count();
                PublicationIntent::Immediate
            } else {
                PublicationIntent::Hidden
            }
        }
    }
}
