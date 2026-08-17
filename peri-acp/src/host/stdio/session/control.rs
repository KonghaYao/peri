//! 会话控制：list / cancel / close / delete。
//!
//! 批 2 起全部转发到 requests 侧唯一实现（`host::requests::session_lifecycle`
//! 与 `host::notify::handle_notification`），本文件只剩协议层 typed 转换。
//!
//! `await_holding_lock` 快注：adapter 跨 await 持有 `ctx.sessions` 写锁是转发
//! 契约（同 `host/stdio/session/create.rs`，parking_lot 已开 `send_guard`）。

#![allow(clippy::await_holding_lock)]

use agent_client_protocol::schema::v1::{
    CloseSessionResponse, DeleteSessionResponse, ListSessionsRequest, ListSessionsResponse,
};
use serde_json::json;

use super::super::context::StdioContext;
use super::super::transport::map_acp_error;

/// session/list 核心逻辑（thin adapter：无副作用，直接转发）。
pub(crate) async fn handle_list(
    ctx: &StdioContext,
    req: ListSessionsRequest,
) -> ListSessionsResponse {
    let params = serde_json::to_value(&req).unwrap_or_else(|_| json!({}));
    match crate::host::requests::session_lifecycle::handle_list(&params, &ctx.cfg).await {
        Ok(value) => match serde_json::from_value(value) {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!(error = %e, "session/list: failed to decode response");
                ListSessionsResponse::new(Vec::new())
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "session/list: failed to list threads");
            ListSessionsResponse::new(Vec::new())
        }
    }
}

/// session/cancel（通知）——批 2 起以 notify 语义为准
/// （`host::notify::handle_notification`）：writer lease 校验 + cancel token +
/// `continuation_armed` 置位。
///
/// notify 侧 `handle_notification` 还会返回 `Option<ContinuationRequest>`
/// （cancel 竞态兜底，TUI 经 continuation scheduler 消费）；stdio 适配期无
/// scheduler（批 3 才并入），此返回值按设计丢弃，仅记录调试日志——续跑由
/// 批 3 的 `run_acp_server` + scheduler 接管。
pub(crate) fn handle_cancel(ctx: &StdioContext, session_id: &str) {
    let params = json!({ "sessionId": session_id });
    let cont_req = {
        let mut sessions = ctx.sessions.write();
        crate::host::notify::handle_notification("session/cancel", &params, &mut sessions, &ctx.cfg)
    };
    if cont_req.is_some() {
        // 批 2 适配期 stdio 无 continuation scheduler，竞态兜底请求不调度。
        tracing::debug!(
            session_id = %session_id,
            "session/cancel: continuation request produced but dropped (no scheduler in stdio, 批 2)"
        );
    }
}

/// session/close 核心逻辑（thin adapter → `session_lifecycle::handle_close`；
/// 统一语义：移除内存态 + cancel token + SessionManager 记录移除，返回
/// `CloseSessionResponse`）。
pub(crate) async fn handle_close(
    ctx: &StdioContext,
    session_id: &str,
    responder: agent_client_protocol::Responder<CloseSessionResponse>,
) -> Result<(), agent_client_protocol::Error> {
    let params = json!({ "sessionId": session_id });
    let mut sessions = ctx.sessions.write();
    let result =
        crate::host::requests::session_lifecycle::handle_close(&params, &ctx.cfg, &mut sessions)
            .await
            .map_err(map_acp_error)?;
    let resp: CloseSessionResponse = serde_json::from_value(result)
        .map_err(|e| agent_client_protocol::Error::internal_error().data(e.to_string()))?;
    responder.respond(resp)
}

/// session/delete 核心逻辑（thin adapter → `session_lifecycle::handle_delete`；
/// 统一语义：内存态清理 + LSP pool shutdown + SessionManager 移除 +
/// ThreadStore 持久化删除）。
pub(crate) async fn handle_delete(
    ctx: &StdioContext,
    session_id: &str,
    responder: agent_client_protocol::Responder<DeleteSessionResponse>,
) -> Result<(), agent_client_protocol::Error> {
    let params = json!({ "sessionId": session_id });
    let mut sessions = ctx.sessions.write();
    let result =
        crate::host::requests::session_lifecycle::handle_delete(&params, &ctx.cfg, &mut sessions)
            .await
            .map_err(map_acp_error)?;
    let resp: DeleteSessionResponse = serde_json::from_value(result)
        .map_err(|e| agent_client_protocol::Error::internal_error().data(e.to_string()))?;
    responder.respond(resp)
}
