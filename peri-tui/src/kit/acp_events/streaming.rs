//! Streaming event handlers — TextChunk, ReasoningChunk.

use super::*;
use crate::kit::stream_data::{TuiReasoningChunk, TuiTextChunk};

pub(super) fn handle_text_chunk(state: &mut BridgeState, tc: &TuiTextChunk) {
    // 先尝试 SubAgent 组路由；带 agent_id 但无匹配组 = 主 agent 文本
    // （v2 事件身份透传后主 agent chunk 亦携带 agent_id，`append_subagent_text`
    // 找不到组即回退主 agent 分支，不能静默丢弃——否则主 agent 回复不显示）。
    let routed_to_subagent = tc
        .agent_id
        .as_deref()
        .is_some_and(|agent_id| state.current_turn.append_subagent_text(agent_id, &tc.text));
    if routed_to_subagent {
        state.variant = 1;
        state.phase = SessionPhase::PromptRunning;
        // SubAgent 文本：Streaming/Block→always push, None→skip
        // 不做块边界检测——subagent 输出相对短且不是主要闪烁来源。
        if super::current_streaming_mode() != super::StreamingMode::None {
            super::render::push_view_models(state);
        }
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
            super::render::push_view_models(state);
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
        state.phase = SessionPhase::PromptRunning;
        // SubAgent 推理：Streaming/Block→always push, None→skip
        if super::current_streaming_mode() != super::StreamingMode::None {
            super::render::push_view_models(state);
        }
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
            super::render::push_view_models(state);
        }
    }
    super::render::push_acp_state(state);
}
