//! 窗口读取的短锁状态检查与稳定 entryId 解析。

use super::*;

/// 短锁内允许读取窗口的会话状态。
///
/// 只读窗口不依赖执行上下文驻留：`HandedOff`/`ReloadRequired` 不构成读取拒绝，
/// 仍从 Store + 合法活跃 owner 读取，不把空 `history_payloads` 当历史。
/// `closing`、rewind 等写入安全边界继续拒绝，只读允许不解除写入约束。
pub(super) fn readable_state<'a>(
    sessions: &'a std::collections::HashMap<String, SessionState>,
    session_id: &str,
) -> Result<&'a SessionState, AcpError> {
    let state = sessions
        .get(session_id)
        .ok_or_else(|| window_error(HistoryWindowError::SessionNotFound))?;
    if state.closing {
        return Err(window_error(HistoryWindowError::SessionClosing));
    }
    if state.history_window.rewind_in_progress {
        return Err(window_error(HistoryWindowError::MutationInProgress));
    }
    Ok(state)
}

/// `readable_state` 的可变版本；仅用于 Host 内存中的 ticket/oversize 元数据更新，
/// 不授权任何会话正文写入。
pub(super) fn readable_state_mut<'a>(
    sessions: &'a mut std::collections::HashMap<String, SessionState>,
    session_id: &str,
) -> Result<&'a mut SessionState, AcpError> {
    let state = sessions
        .get_mut(session_id)
        .ok_or_else(|| window_error(HistoryWindowError::SessionNotFound))?;
    if state.closing {
        return Err(window_error(HistoryWindowError::SessionClosing));
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
