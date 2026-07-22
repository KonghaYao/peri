//! Agent event handlers — AgentExecutionFailed, BackgroundTaskCompleted.

use super::*;
use crate::i18n;
use crate::kit::tui_render_unit::TuiNoteLevel;
use fluent_bundle::FluentValue;

pub(super) fn handle_agent_execution_failed(state: &mut BridgeState, message: &str) {
    tracing::error!(message, "bridge: AgentExecutionFailed");
    let text = i18n::tr_args(
        "app-note-agent-failed",
        &[("message".into(), FluentValue::from(message))],
    );
    state.inject_system_note(text, TuiNoteLevel::Error);
    state.phase = SessionPhase::Idle;
    super::render::push_acp_state(state);
}

pub(super) fn handle_background_task_completed(
    agent_name: &str,
    task_id: &str,
    success: bool,
    duration_ms: u64,
) {
    let msg = if success {
        format!(
            "后台 {} {} 完成 ({:.0}s)",
            agent_name,
            task_id,
            duration_ms as f64 / 1000.0
        )
    } else {
        format!(
            "后台 {} {} 失败 ({:.0}s)",
            agent_name,
            task_id,
            duration_ms as f64 / 1000.0
        )
    };
    tracing::info!(msg, "bridge: BackgroundTaskCompleted");
}
