//! 会话控制：list / cancel / close。

use agent_client_protocol::schema::{ListSessionsRequest, ListSessionsResponse};

use super::super::context::StdioContext;

/// session/list 核心逻辑
pub(crate) async fn handle_list(
    ctx: &StdioContext,
    req: ListSessionsRequest,
) -> ListSessionsResponse {
    let cwd_filter = req.cwd.as_ref().map(|p| p.to_string_lossy().to_string());
    let entries =
        peri_acp::dispatch::list_sessions_as_info(ctx.thread_store.as_ref(), cwd_filter.as_deref())
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "session/list: failed to list threads");
                Vec::new()
            });
    ListSessionsResponse::new(entries)
}

/// session/cancel 核心逻辑
pub(crate) fn handle_cancel(ctx: &StdioContext, session_id: &str) {
    let sessions = ctx.sessions.read();
    if let Some(s) = sessions.get(session_id) {
        if let Some(ref token) = s.cancel_token {
            token.cancel();
            tracing::info!(session_id = %session_id, "Cancel requested");
        }
    }
}

/// session/close 核心逻辑
pub(crate) async fn handle_close(ctx: &StdioContext, session_id: &str) {
    {
        let mut sessions = ctx.sessions.write();
        if let Some(s) = sessions.remove(session_id) {
            if let Some(ref token) = s.cancel_token {
                token.cancel();
            }
            tracing::info!(session_id = %session_id, "Session closed");
        }
    }
    // 同步从 SessionManager 移除 AcpSession 记录（取消所有 cascade 子 agent）
    let _ = ctx.session_manager.close_session(session_id).await;
}
