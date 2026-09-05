//! One synchronous projection boundary for interactive session transitions.

use crate::app::panel_types::PanelKind;
use crate::kit::{acp_events, atoms, input_history, panel_registry};

/// Project a session transition before the client route can accept reverse work.
///
/// This is deliberately synchronous: the lifecycle operation gate is held by
/// the caller across this complete projection.
pub fn project_session_boundary(target_session_id: Option<&str>) {
    atoms::ACTIVE_SESSION_ID.set(target_session_id.unwrap_or_default().to_string());
    atoms::BRIDGE_RESET_COUNTER.set(atoms::BRIDGE_RESET_COUNTER.get().wrapping_add(1));
    acp_events::push_view_models_for_reset();
    {
        let state = atoms::ACP_STATE.state();
        let mut state = state.write();
        state.variant = 0;
        state.is_loading = false;
    }
    atoms::INPUT_BUFFER.state().write().clear();
    *atoms::HITL_PENDING.state().write() = None;
    *atoms::ASK_USER_PENDING.state().write() = None;
    panel_registry::close_panel(PanelKind::AskUser);

    let popup = *atoms::POPUP_KIND.state().read();
    let reject_confirm = atoms::CONFIRM_PAYLOAD
        .state()
        .read()
        .as_ref()
        .is_some_and(|payload| {
            matches!(
                payload.pending_action,
                atoms::ConfirmAction::RejectAskUser { .. }
            )
        });
    if matches!(
        popup,
        Some(atoms::PopupKind::Hitl | atoms::PopupKind::AskUser)
    ) || (popup == Some(atoms::PopupKind::Confirm) && reject_confirm)
    {
        *atoms::POPUP_KIND.state().write() = None;
    }
    if reject_confirm {
        *atoms::CONFIRM_PAYLOAD.state().write() = None;
    }

    *atoms::REWIND_PREVIEW.state().write() = None;
    *atoms::REWIND_TARGET_TEXT.state().write() = None;
    *atoms::REWIND_PREVIEW_FINGERPRINT.state().write() = None;
    *atoms::REWIND_BUDGET_STATE.state().write() = atoms::RewindBudgetState::Idle;
    *atoms::REWIND_QUERY_ERROR.state().write() = None;
    *atoms::TODO_ITEMS.state().write() = Vec::new();
    *atoms::GOAL_SNAPSHOT.state().write() = None;
    panel_registry::close_panel(PanelKind::Goal);
    input_history::reset_history_cursor();
}

#[cfg(test)]
#[path = "session_boundary_test.rs"]
mod tests;
