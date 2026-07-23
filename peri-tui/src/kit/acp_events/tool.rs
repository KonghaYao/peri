//! Tool event handlers — ToolStarted, ToolEnded, ToolCount, Progress,
//! ReplayToolStarted, ReplayToolEnded + update_committed_tool_card helper.

use super::*;
use crate::kit::acp_types::ToolCardAccumulator;
use crate::kit::atoms::BG_AGENT_IDS;
use crate::kit::atoms::BG_DISPLAY;
use crate::kit::stream_data::{TuiToolEnded, TuiToolStarted};
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiToolCard, tui_hash_str};

pub(super) fn handle_tool_started(state: &mut BridgeState, ts: &TuiToolStarted) {
    if let Some(agent_id) = ts.agent_id.as_deref() {
        // bg sub-agent: TurnSuspended 后 SubAgentAccumulator 已被清除，
        // 后续 bg 工具事件仅更新 BG_DISPLAY，不走 start_subagent_tool
        if BG_AGENT_IDS.state().read().contains(agent_id) {
            if let Some(entry) = BG_DISPLAY
                .state()
                .write()
                .iter_mut()
                .find(|e| e.id == agent_id)
            {
                entry.current_tool = Some(ts.tool_name.clone());
            }
            state.variant = 1;
            state.phase = SessionPhase::PromptRunning;
            super::render::push_view_models(state);
            // block 模式：ToolStarted 时已推送缓冲文本到视图，
            // 同步追踪变量，确保工具执行完毕后新 TextChunk 的块边界检测从正确位置开始。
            state.last_pushed_text_len = state.current_turn.text.chars().count();
            state.last_pushed_reasoning_len = state.current_turn.reasoning.chars().count();
        } else {
            // 同步 sub-agent: 路由到 SubAgentAccumulator
            let routed = state.current_turn.start_subagent_tool(
                agent_id,
                ToolCardAccumulator::new(
                    ts.tool_id.clone(),
                    ts.tool_name.clone(),
                    ts.input_summary.clone(),
                ),
            );
            if !routed {
                // [诊断] 收集当前所有 SubAgentAccumulator 的 agent_id
                let registered_ids: Vec<&str> = state.current_turn.subagent_ids();
                tracing::warn!(
                    target: "tui.acp_events",
                    agent_id = ?agent_id,
                    tool_id = %ts.tool_id,
                    tool_name = %ts.tool_name,
                    routed = false,
                    registered_count = registered_ids.len(),
                    registered_agent_ids = ?registered_ids,
                    "subagent tool start NOT ROUTED to SubAgentGroup"
                );
                // [修复] 兜底：路由失败时将工具卡作为普通 ToolCard 展示，
                // 确保第二个 SubAgent 的工具调用不会完全丢失
                state.current_turn.start_tool(ToolCardAccumulator::new(
                    ts.tool_id.clone(),
                    ts.tool_name.clone(),
                    ts.input_summary.clone(),
                ));
            }
            state.variant = 1;
            state.phase = SessionPhase::PromptRunning;
            super::render::push_view_models(state);
            state.last_pushed_text_len = state.current_turn.text.chars().count();
            state.last_pushed_reasoning_len = state.current_turn.reasoning.chars().count();
        }
    } else {
        state.current_turn.start_tool(ToolCardAccumulator::new(
            ts.tool_id.clone(),
            ts.tool_name.clone(),
            ts.input_summary.clone(),
        ));
        state.variant = 1;
        state.phase = SessionPhase::PromptRunning;
        super::render::push_view_models(state);
        state.last_pushed_text_len = state.current_turn.text.chars().count();
        state.last_pushed_reasoning_len = state.current_turn.reasoning.chars().count();
    }
    super::render::push_acp_state(state);
}

pub(super) fn handle_tool_ended(state: &mut BridgeState, te: &TuiToolEnded) {
    if let Some(agent_id) = te.agent_id.as_deref() {
        // bg sub-agent: TurnSuspended 后 SubAgentAccumulator 已被清除，
        // 后续 bg 工具事件仅更新 BG_DISPLAY，不走 end_subagent_tool
        if BG_AGENT_IDS.state().read().contains(agent_id) {
            if let Some(entry) = BG_DISPLAY
                .state()
                .write()
                .iter_mut()
                .find(|e| e.id == agent_id)
            {
                entry.current_tool = None;
                entry.tool_count += 1;
            }
            state.variant = 1;
            state.phase = SessionPhase::PromptRunning;
            super::render::push_view_models(state);
            state.last_pushed_text_len = state.current_turn.text.chars().count();
            state.last_pushed_reasoning_len = state.current_turn.reasoning.chars().count();
        } else {
            let routed = state.current_turn.end_subagent_tool(
                agent_id,
                &te.tool_id,
                te.output_summary.clone(),
                te.is_error,
            );
            if !routed {
                state
                    .current_turn
                    .end_tool(&te.tool_id, te.output_summary.clone(), te.is_error);
            }
            state.variant = 1;
            state.phase = SessionPhase::PromptRunning;
            super::render::push_view_models(state);
            state.last_pushed_text_len = state.current_turn.text.chars().count();
            state.last_pushed_reasoning_len = state.current_turn.reasoning.chars().count();
        }
    } else {
        state
            .current_turn
            .end_tool(&te.tool_id, te.output_summary.clone(), te.is_error);
        state.variant = 1;
        state.phase = SessionPhase::PromptRunning;
        super::render::push_view_models(state);
        state.last_pushed_text_len = state.current_turn.text.chars().count();
        state.last_pushed_reasoning_len = state.current_turn.reasoning.chars().count();
    }
    super::render::push_acp_state(state);
}

pub(super) fn handle_tool_count(state: &mut BridgeState) {
    super::render::push_acp_state(state);
}

pub(super) fn handle_progress(state: &mut BridgeState) {
    super::render::push_acp_state(state);
}

pub(super) fn handle_replay_tool_started(
    state: &mut BridgeState,
    tool_id: &str,
    tool_name: &str,
    input_summary: &str,
) {
    let card = TuiToolCard {
        tool_id: tool_id.to_string(),
        tool_name: tool_name.to_string(),
        input_summary: input_summary.to_string(),
        output_summary: String::new(),
        is_error: false,
        is_running: true,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: tui_hash_str(&format!(
            "{}|{}|{}||false|true",
            tool_id, tool_name, input_summary
        )),
    };
    state.committed.push_back(TuiRenderUnit::TuiToolCard(card));
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}

pub(super) fn handle_replay_tool_ended(
    state: &mut BridgeState,
    tool_id: &str,
    output_summary: &str,
    is_error: bool,
) {
    update_committed_tool_card(state, tool_id, output_summary, is_error);
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}

/// 在 `state.committed` 中按 `tool_id` 查找并更新 TuiToolCard。
///
/// 用于 replay 场景：`ReplayToolStarted` 先 push 一张 is_running=true 的卡片，
/// 后续 `ReplayToolEnded` 到达时更新 output + is_running=false。
/// 如果找不到对应 tool_id，静默忽略。
fn update_committed_tool_card(
    state: &mut BridgeState,
    tool_id: &str,
    output_summary: &str,
    is_error: bool,
) {
    for i in 0..state.committed.len() {
        if let TuiRenderUnit::TuiToolCard(card) = &state.committed[i]
            && card.tool_id == tool_id
            && card.is_running
        {
            let updated = TuiToolCard {
                tool_id: card.tool_id.clone(),
                tool_name: card.tool_name.clone(),
                input_summary: card.input_summary.clone(),
                output_summary: output_summary.to_string(),
                is_error,
                is_running: false,
                running_duration_ms: None,
                diff: card.diff.clone(),
                tool_calls_count: card.tool_calls_count,
                content_hash: tui_hash_str(&format!(
                    "{}|{}|{}|{}|{is_error}|false",
                    card.tool_id, card.tool_name, card.input_summary, output_summary,
                )),
            };
            state.committed = state
                .committed
                .update(i, TuiRenderUnit::TuiToolCard(updated));
            return;
        }
    }
}
