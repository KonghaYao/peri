//! Sub-agent event handlers — SubagentStarted, SubagentStopped.

use super::*;
use crate::kit::atoms::BG_AGENT_IDS;
use crate::kit::bg_task_identity::bind_linked_agent_on_subagent_started;
use crate::kit::bg_task_live::{handle_bg_subagent_stopped, init_agent_live_detail};

pub(super) fn handle_subagent_started(
    state: &mut BridgeState,
    agent_id: &str,
    agent_name: &str,
    is_background: bool,
) {
    tracing::info!(
        target: "tui.acp_events",
        agent_id = %agent_id,
        agent_name = %agent_name,
        is_background = %is_background,
        existing_subagent_count = state.current_turn.subagent_ids().len(),
        "SubagentStarted: creating SubAgentGroup container"
    );
    state
        .current_turn
        .start_subagent(agent_id.to_string(), agent_name.to_string());
    // 仅后台 subagent 注册到 BG_AGENT_IDS——同步 subagent 不进入后台显示区域
    if is_background {
        BG_AGENT_IDS.state().write().insert(agent_id.to_string());
        if let Some(task_id) = bind_linked_agent_on_subagent_started(agent_id, agent_name) {
            init_agent_live_detail(&task_id, agent_id, agent_name);
        }
    }
    state.variant = 1;
    state.phase = SessionPhase::PromptRunning;
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}

pub(super) fn handle_subagent_stopped(
    state: &mut BridgeState,
    agent_id: &str,
    result: &str,
    is_error: bool,
) {
    tracing::info!(
        target: "tui.acp_events",
        agent_id = %agent_id,
        is_error = %is_error,
        "SubagentStopped: marking SubAgentGroup as done"
    );
    state.current_turn.stop_subagent(agent_id, is_error, result);
    if BG_AGENT_IDS.state().read().contains(agent_id) {
        handle_bg_subagent_stopped(agent_id, result, is_error);
    }
    // 清理后台 agent_id 注册
    BG_AGENT_IDS.state().write().remove(agent_id);
    state.variant = 1;
    // phase 由 SubagentStarted + 流式事件维护，此处不再无条件覆盖
    // （避免 bg agent 的场景 TurnDone/TurnSuspended 后被重新激活）
    super::render::push_view_models(state);
    super::render::push_acp_state(state);
}
