//! 会话控制：list / cancel / close / delete。

use agent_client_protocol::schema::v1::{ListSessionsRequest, ListSessionsResponse};

use super::super::context::StdioContext;

/// session/list 核心逻辑
pub(crate) async fn handle_list(
    ctx: &StdioContext,
    req: ListSessionsRequest,
) -> ListSessionsResponse {
    let cwd_filter = req.cwd.as_ref().map(|p| p.to_string_lossy().to_string());
    let entries =
        crate::dispatch::list_sessions_as_info(ctx.controller.as_ref(), cwd_filter.as_deref())
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
    let lsp_pool = {
        let mut sessions = ctx.sessions.write();
        if let Some(s) = sessions.remove(session_id) {
            if let Some(ref token) = s.cancel_token {
                token.cancel();
            }
            tracing::info!(session_id = %session_id, "Session closed");
            s.lsp_pool
        } else {
            None
        }
    };
    // 关闭会话级 LSP pool：LspServerPool 无 Drop cleanup，不显式 shutdown 则
    // LSP 服务器子进程与 read task 在 session/close 后残留（stdio 长驻宿主
    // 下服务器进程无限累积）。shutdown 在写锁外执行（kill 子进程耗时）。
    if let Some(pool) = lsp_pool {
        pool.shutdown().await;
    }
    // 同步从 SessionManager 移除 AcpSession 记录（取消所有 cascade 子 agent）
    let _ = ctx.session_manager.close_session(session_id).await;
}

/// session/delete 核心逻辑（标准 ACP：从 session history 中移除会话）。
///
/// 语义（agentclientprotocol.com/protocol/v1/session-delete）：删除后会话不再
/// 出现在 `session/list` 中，`session/load` 亦无法再加载。实现分两步：
/// 1. 若会话当前活跃，先执行与 `session/close` 相同的清理（cancel token、
///    LSP pool shutdown、SessionManager 记录移除），避免子进程残留；
/// 2. 从 ThreadStore 删除线程（消息级联删除）。
///
/// 注意：`handle_close` 只移除内存态、保留 history；`handle_delete` 是持久化
/// 删除——调用方须确认用户意图，无恢复路径。
pub(crate) async fn handle_delete(ctx: &StdioContext, session_id: &str) {
    let lsp_pool = {
        let mut sessions = ctx.sessions.write();
        if let Some(s) = sessions.remove(session_id) {
            if let Some(ref token) = s.cancel_token {
                token.cancel();
            }
            tracing::info!(session_id = %session_id, "Session removed on delete");
            s.lsp_pool
        } else {
            None
        }
    };
    if let Some(pool) = lsp_pool {
        pool.shutdown().await;
    }
    // 同步从 SessionManager 移除 AcpSession 记录（取消所有 cascade 子 agent）
    let _ = ctx.session_manager.close_session(session_id).await;
    // 持久化删除线程（消息级联删除）；线程不存在时（幂等）不视为错误
    if let Err(e) = ctx
        .thread_store
        .delete_thread(&session_id.to_string())
        .await
    {
        tracing::error!(session_id = %session_id, error = %e, "session/delete: thread deletion failed");
    } else {
        tracing::info!(session_id = %session_id, "Session history deleted");
    }
}
