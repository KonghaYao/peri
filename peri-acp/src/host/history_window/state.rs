//! 窗口读取的短锁状态检查与稳定 entryId 解析。

use super::*;

/// 短锁内允许读取窗口的会话状态；`HandedOff`/`ReloadRequired` 都不得解释为空历史。
pub(super) fn writable_state<'a>(
    sessions: &'a std::collections::HashMap<String, SessionState>,
    session_id: &str,
) -> Result<&'a SessionState, AcpError> {
    let state = sessions
        .get(session_id)
        .ok_or_else(|| window_error(HistoryWindowError::SessionNotFound))?;
    if state.closing {
        return Err(window_error(HistoryWindowError::SessionClosing));
    }
    match state.context_ownership {
        ContextOwnership::Resident => {}
        ContextOwnership::HandedOff => {
            return Err(AcpError::new(
                -32010,
                "Session context is owned by an in-flight prompt",
            ))
        }
        ContextOwnership::ReloadRequired => {
            return Err(invalidated(
                "session hot state requires reload; reopen the history window",
            ))
        }
    }
    if state.history_window.rewind_in_progress {
        return Err(window_error(HistoryWindowError::MutationInProgress));
    }
    Ok(state)
}

pub(super) fn writable_state_mut<'a>(
    sessions: &'a mut std::collections::HashMap<String, SessionState>,
    session_id: &str,
) -> Result<&'a mut SessionState, AcpError> {
    let state = sessions
        .get_mut(session_id)
        .ok_or_else(|| window_error(HistoryWindowError::SessionNotFound))?;
    if state.closing {
        return Err(window_error(HistoryWindowError::SessionClosing));
    }
    match state.context_ownership {
        ContextOwnership::Resident => {}
        ContextOwnership::HandedOff => {
            return Err(AcpError::new(
                -32010,
                "Session context is owned by an in-flight prompt",
            ))
        }
        ContextOwnership::ReloadRequired => {
            return Err(invalidated(
                "session hot state requires reload; reopen the history window",
            ))
        }
    }
    if state.history_window.rewind_in_progress {
        return Err(window_error(HistoryWindowError::MutationInProgress));
    }
    Ok(state)
}

/// 稳定 entryId 只校验子会话归属；source 的 snapshot/ancestor 由调用方在 bounds 后复核。
pub(super) fn parse_anchor(
    entry_id: &str,
    session_id: &str,
) -> Result<peri_acp_types::messages::MessageId, AcpError> {
    HistoryEntryId::parse_for_session(entry_id, &session_id.to_owned()).map_err(window_error)
}
