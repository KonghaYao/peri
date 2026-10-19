//! Agent event handlers — AgentExecutionFailed, BackgroundTaskCompleted.

use super::*;
use crate::i18n;
use crate::kit::atoms;
use crate::kit::tui_render_unit::TuiNoteLevel;
use fluent_bundle::FluentValue;

pub(super) fn handle_agent_execution_failed(state: &mut BridgeState, message: &str) {
    tracing::error!("bridge: AgentExecutionFailed");
    state.pending_cache_usage = None;
    state.phase = SessionPhase::Idle;
    state.current_turn.deactivate();
    let report = crate::kit::steer_state::STEERS
        .state()
        .write()
        .fail_session_submissions(&state.active_session_id);
    // 失败结算必须说明输入去向：未发出的已恢复，结果未知的保留原文待对账。
    if let Some(notice) = report.notice() {
        atoms::NOTIFICATION.set(Some(atoms::Notification {
            message: notice,
            until: peri_time::monotonic_now() + crate::kit::steer_consumer::FAILURE_NOTICE_DURATION,
        }));
    }
    let text = i18n::tr_args(
        "app-note-agent-failed",
        &[("message".into(), FluentValue::from(message))],
    );
    let content_hash = crate::kit::tui_render_unit::tui_hash_str(&text);
    state
        .current_turn
        .push_system_note(text, TuiNoteLevel::Error, content_hash);
    state.publish_barrier();
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
