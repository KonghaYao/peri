//! Tool event handlers — ToolStarted, ToolEnded, ToolCount, Progress,
//! ReplayToolStarted, ReplayToolEnded + update_committed_tool_card helper.

use super::*;
use crate::kit::acp_types::ToolCardAccumulator;
use crate::kit::atoms::BG_AGENT_IDS;
use crate::kit::atoms::BG_DISPLAY;
use crate::kit::stream_data::{TuiToolEnded, TuiToolStarted};
use crate::kit::tui_render_unit::{
    EntryStatus, FoldTarget, TuiRenderUnit, TuiToolCard, fold_for_status,
};

pub(super) fn handle_tool_started(state: &mut BridgeState, ts: &TuiToolStarted) {
    if ts.tool_name == "TodoWrite" {
        state.record_todo_started(ts.tool_id.clone(), ts.raw_input.clone());
    }
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
                ToolCardAccumulator::with_input(
                    ts.tool_id.clone(),
                    ts.tool_name.clone(),
                    ts.input_summary.clone(),
                    ts.raw_input.clone(),
                    state.last_successful_todos.as_ref(),
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
                state
                    .current_turn
                    .start_tool(ToolCardAccumulator::with_input(
                        ts.tool_id.clone(),
                        ts.tool_name.clone(),
                        ts.input_summary.clone(),
                        ts.raw_input.clone(),
                        state.last_successful_todos.as_ref(),
                    ));
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
            .start_tool(ToolCardAccumulator::with_input(
                ts.tool_id.clone(),
                ts.tool_name.clone(),
                ts.input_summary.clone(),
                ts.raw_input.clone(),
                state.last_successful_todos.as_ref(),
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
    let _todo_advanced = if let Some(agent_id) = te.agent_id.as_deref() {
        // bg sub-agent 不生成消息卡片，但仍需在成功结束时推进 Todo 基线。
        if BG_AGENT_IDS.state().read().contains(agent_id) {
            if let Some(entry) = BG_DISPLAY
                .state()
                .write()
                .iter_mut()
                .find(|entry| entry.id == agent_id)
            {
                entry.current_tool = None;
                entry.tool_count += 1;
            }
            state.variant = 1;
            state.phase = SessionPhase::PromptRunning;
            super::render::push_view_models(state);
            state.last_pushed_text_len = state.current_turn.text.chars().count();
            state.last_pushed_reasoning_len = state.current_turn.reasoning.chars().count();
            state.complete_todo_if_current(&te.tool_id, te.is_error)
        } else {
            let ended = state.current_turn.end_subagent_tool(
                agent_id,
                &te.tool_id,
                te.output_summary.clone(),
                te.is_error,
            ) || state.current_turn.end_tool(
                &te.tool_id,
                te.output_summary.clone(),
                te.is_error,
            );
            state.variant = 1;
            state.phase = SessionPhase::PromptRunning;
            super::render::push_view_models(state);
            state.last_pushed_text_len = state.current_turn.text.chars().count();
            state.last_pushed_reasoning_len = state.current_turn.reasoning.chars().count();
            ended && state.complete_todo_if_current(&te.tool_id, te.is_error)
        }
    } else {
        let ended =
            state
                .current_turn
                .end_tool(&te.tool_id, te.output_summary.clone(), te.is_error);
        state.variant = 1;
        state.phase = SessionPhase::PromptRunning;
        super::render::push_view_models(state);
        state.last_pushed_text_len = state.current_turn.text.chars().count();
        state.last_pushed_reasoning_len = state.current_turn.reasoning.chars().count();
        ended && state.complete_todo_if_current(&te.tool_id, te.is_error)
    };

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
    raw_input: &serde_json::Value,
) {
    if tool_name == "TodoWrite" {
        state.record_todo_started(tool_id.to_string(), raw_input.clone());
    }
    let presentation = crate::kit::tool_semantics::presentation_for(
        tool_name,
        raw_input,
        state.last_successful_todos.as_ref(),
    );
    let card = TuiToolCard {
        tool_id: tool_id.to_string(),
        tool_name: tool_name.to_string(),
        input_summary: input_summary.to_string(),
        output_summary: String::new(),
        is_error: false,
        is_running: true,
        running_duration_ms: None,
        completed_duration_ms: None,
        // [G-Diff] replay 构造时无输出（is_running=true）——解析器按 skip 返回
        // None；`ReplayToolEnded`（update_committed_tool_card）到达时再解析。
        diff: super::super::acp_types::parse_tool_diff(tool_name, "", true, None),
        presentation: presentation.clone(),
        // replay 构造的卡片按当前状态取表值（running → Preview）；
        // [G1] hash 由 recompute_hash 单点计算。
        fold: fold_for_status(FoldTarget::Tool, EntryStatus::Running),
        user_modified: false,
        tool_calls_count: 0,
        content_hash: 0,
    };
    let mut card = card;
    card.recompute_hash();
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
    if update_committed_tool_card(state, tool_id, output_summary, is_error) {
        state.complete_todo_if_current(tool_id, is_error);
    }
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
) -> bool {
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
                completed_duration_ms: card.completed_duration_ms,
                // [G-Diff] ReplayToolEnded 是 replay 路径唯一有输出的点——
                // 在此解析 diff（is_error 时解析器 skip；path hint 复用
                // input_summary——Edit/Write 摘要即 file_path 口径）。
                diff: super::super::acp_types::parse_tool_diff(
                    &card.tool_name,
                    output_summary,
                    is_error,
                    Some(card.input_summary.clone()),
                ),
                presentation: card.presentation.clone(),
                // 保留既有折叠状态——折叠统一由 push_view_models 的 pass 按
                // 新状态（completed/error）重算；用户覆盖由 FOLD_OVERRIDES 表接管。
                fold: card.fold,
                user_modified: card.user_modified,
                tool_calls_count: card.tool_calls_count,
                content_hash: 0,
            };
            let mut updated = updated;
            updated.recompute_hash();
            state.committed = state
                .committed
                .update(i, TuiRenderUnit::TuiToolCard(updated));
            return true;
        }
    }
    false
}
