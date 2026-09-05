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

pub(super) fn handle_text_chunk(state: &mut BridgeState, tc: &TuiTextChunk) {
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
        // SubAgent 文本：Streaming/Block→always push, None→skip
        // 不做块边界检测——subagent 输出相对短且不是主要闪烁来源。
        if super::current_streaming_mode() != super::StreamingMode::None {
            super::render::push_view_models(state);
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
        return;
    } else {
        state
            .current_turn
            .append_text(&tc.text, tc.message_id.as_deref());
        state.variant = 1;
        state.phase = SessionPhase::PromptRunning;
        let should_push = match super::current_streaming_mode() {
            super::StreamingMode::Streaming => true,
            super::StreamingMode::Block => {
                if super::has_md_block_boundary_since(
                    &state.current_turn.text,
                    state.last_pushed_text_len,
                ) {
                    state.last_pushed_text_len = state.current_turn.text.chars().count();
                    true
                } else {
                    false
                }
            }
            super::StreamingMode::None => false,
        };
        if should_push {
            // bridge-local scheduler consumes the publication intent after canonical ingest.
        }
    }
    super::render::push_acp_state(state);
}

pub(super) fn handle_reasoning_chunk(state: &mut BridgeState, rc: &TuiReasoningChunk) {
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
        // SubAgent 推理：Streaming/Block→always push, None→skip
        if super::current_streaming_mode() != super::StreamingMode::None {
            super::render::push_view_models(state);
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
        return;
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
        let should_push = match super::current_streaming_mode() {
            super::StreamingMode::Streaming => true,
            super::StreamingMode::Block => {
                if super::has_md_block_boundary_since(
                    &state.current_turn.reasoning,
                    state.last_pushed_reasoning_len,
                ) {
                    state.last_pushed_reasoning_len = state.current_turn.reasoning.chars().count();
                    true
                } else {
                    false
                }
            }
            super::StreamingMode::None => false,
        };
        if should_push {
            // bridge-local scheduler consumes the publication intent after canonical ingest.
        }
    }
    super::render::push_acp_state(state);
}
