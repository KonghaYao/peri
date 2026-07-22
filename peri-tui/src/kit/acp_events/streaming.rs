//! Streaming event handlers — TextChunk, ReasoningChunk.

use super::*;
use crate::kit::stream_data::{TuiReasoningChunk, TuiTextChunk};

pub(super) fn handle_text_chunk(state: &mut BridgeState, tc: &TuiTextChunk) {
    if let Some(agent_id) = tc.agent_id.as_deref() {
        if !state.current_turn.append_subagent_text(agent_id, &tc.text) {
            tracing::trace!(
                agent_id,
                "kit bridge: subagent text chunk has no active group"
            );
        }
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
            super::StreamingMode::Block => super::has_md_block_boundary_since(
                &state.current_turn.text,
                state.last_pushed_text_len,
            ),
            super::StreamingMode::None => false,
        };
        if should_push {
            state.last_pushed_text_len = state.current_turn.text.chars().count();
            super::render::push_view_models(state);
        }
    }
    super::render::push_acp_state(state);
}

pub(super) fn handle_reasoning_chunk(state: &mut BridgeState, rc: &TuiReasoningChunk) {
    if let Some(agent_id) = rc.agent_id.as_deref() {
        if !state
            .current_turn
            .append_subagent_reasoning(agent_id, &rc.text)
        {
            tracing::trace!(
                agent_id,
                "kit bridge: subagent reasoning chunk has no active group"
            );
        }
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
        tracing::info!(
            len = state.current_turn.reasoning.len(),
            "bridge: reasoning appended"
        );
        state.variant = 1;
        state.phase = SessionPhase::PromptRunning;
        let should_push = match super::current_streaming_mode() {
            super::StreamingMode::Streaming => true,
            super::StreamingMode::Block => super::has_md_block_boundary_since(
                &state.current_turn.reasoning,
                state.last_pushed_reasoning_len,
            ),
            super::StreamingMode::None => false,
        };
        if should_push {
            state.last_pushed_reasoning_len = state.current_turn.reasoning.chars().count();
            super::render::push_view_models(state);
        }
    }
    super::render::push_acp_state(state);
}
