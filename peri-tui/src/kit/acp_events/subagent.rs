//! Sub-agent event handlers — SubagentStarted, SubagentStopped.

use super::*;
use crate::kit::atoms::{BG_AGENT_IDS, BG_TASKS};
use crate::kit::bg_task_identity::{
    bind_linked_agent_on_subagent_started, bind_task_run, is_latest_child_run,
    task_id_for_agent_id, task_id_for_child_run_observed,
};
use crate::kit::bg_task_live::{handle_bg_subagent_stopped_on_task, init_agent_live_detail};
use peri_acp_types::event::ChildEventOrigin;

pub(super) fn handle_subagent_started(
    state: &mut BridgeState,
    agent_id: &str,
    agent_name: &str,
    is_background: bool,
    parent_tool_call_id: Option<String>,
    origin: Option<&ChildEventOrigin>,
) {
    tracing::info!(
        target: "tui.acp_events",
        agent_id = %agent_id,
        agent_name = %agent_name,
        is_background = %is_background,
        parent_tool_call_id = ?parent_tool_call_id,
        existing_subagent_count = state.current_turn.subagent_ids().len(),
        "SubagentStarted: creating SubAgentGroup container"
    );
    state.current_turn.start_subagent_with_background(
        agent_id.to_string(),
        agent_name.to_string(),
        is_background,
        parent_tool_call_id,
    );
    // 仅后台 subagent 注册到 BG_AGENT_IDS——同步 subagent 不进入后台显示区域
    if is_background {
        BG_AGENT_IDS.state().write().insert(agent_id.to_string());
        // Carry the occurrence into binding too: an old run can still be Running
        // in BG_LIVE_DETAIL when its Stop was lost and a newer execution starts.
        let instance_id = state
            .current_turn
            .subagent_instance_id(agent_id)
            .map(str::to_string);
        if let Some(task_id) = bind_linked_agent_on_subagent_started(
            agent_id,
            agent_name,
            instance_id.as_deref(),
            origin.map(|origin| origin.run_order),
        ) {
            init_agent_live_detail(&task_id, agent_id, agent_name, instance_id.as_deref());
            if let Some(origin) = origin {
                bind_task_run(&task_id, origin.run_order);
            }
        } else if let Some(origin) = origin {
            // The registry row for this occurrence has not arrived yet. Keep
            // the full occurrence, not just the display instance, so the row
            // can bind it and route later Stops exactly.
            state.child_runs.note_unbound_start(origin);
        }
    }
    state.variant = 1;
    if !is_background {
        state.phase = SessionPhase::PromptRunning;
    }
    state.publish_barrier();
    super::render::push_acp_state(state);
}

pub(super) fn handle_subagent_stopped(
    state: &mut BridgeState,
    agent_id: &str,
    result: &str,
    is_error: bool,
    origin: Option<&ChildEventOrigin>,
) {
    tracing::info!(
        target: "tui.acp_events",
        agent_id = %agent_id,
        is_error = %is_error,
        "SubagentStopped: marking SubAgentGroup as done"
    );
    state.current_turn.stop_subagent(agent_id, is_error, result);
    if let Some(origin) = origin {
        // Keep the occurrence terminal for a registry row that appears later;
        // it must not be rebound as a live run.
        state
            .child_runs
            .note_unbound_stop(agent_id, origin.run_order);
    }
    let pending_ceiling = state.child_runs.pending_run_ceiling(agent_id);
    let task_id = match origin {
        Some(origin) => task_id_for_child_run_observed(agent_id, origin.run_order, pending_ceiling),
        None => task_id_for_agent_id(agent_id),
    };
    // A snapshot can bind the child before/without a live Started event; the
    // task identity alone is enough to settle the exact run. When no row owns
    // the occurrence, settling the newest row would end a newer task.
    if let Some(task_id) = task_id.as_deref() {
        handle_bg_subagent_stopped_on_task(task_id, result, is_error);
        BG_TASKS
            .state()
            .write()
            .retain(|task| task.task_id != task_id);
    }
    // A stale Stop must not unregister the child while a newer run is live.
    let stale_stop = origin.is_some_and(|origin| !is_latest_child_run(agent_id, origin.run_order));
    if !stale_stop {
        BG_AGENT_IDS.state().write().remove(agent_id);
    }
    state.variant = 1;
    // phase 由 SubagentStarted + 流式事件维护，此处不再无条件覆盖
    // （避免 bg agent 的场景 TurnDone/TurnSuspended 后被重新激活）
    state.publish_barrier();
    super::render::push_acp_state(state);
}
