//! Session 创建：new / load / resume / fork。
//!
//! 批 2 起全部转发到 requests 侧唯一实现（`host::requests::session_lifecycle`，
//! 会话级 lsp_pool 创建（H1）与 MCP skill 发现预热（决策 B 扩展）已并入 requests
//! 侧；通知经 `transport::notify_transport` 适配器回到 typed 发送路径），本文件
//! 只剩协议层 typed 转换。
//!
//! `await_holding_lock` 快注：adapter 跨 await 持有 `ctx.sessions` 写锁是**转发
//! 契约**——requests 侧 `handle_*` 在函数全程持有 `&mut HashMap`（与
//! `run_acp_server` 持 tokio MutexGuard 跨 `handle_request().await` 同一语义）。
//! parking_lot 已开 `send_guard`（guard Send），锁竞争语义等于 tokio 异步锁。

#![allow(clippy::await_holding_lock)]

use agent_client_protocol::{
    schema::v1::{
        ForkSessionRequest, ForkSessionResponse, LoadSessionRequest, LoadSessionResponse,
        NewSessionRequest, NewSessionResponse, ResumeSessionRequest, ResumeSessionResponse,
    },
    Client, ConnectionTo, Error, Responder,
};

use super::super::context::StdioContext;
use super::super::transport::{map_acp_error, notify_transport};

/// session/new 处理器（thin adapter → `session_lifecycle::handle_new`）。
pub(crate) async fn handle_new(
    ctx: &StdioContext,
    req: NewSessionRequest,
    responder: Responder<NewSessionResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let params =
        serde_json::to_value(&req).map_err(|e| Error::internal_error().data(e.to_string()))?;
    let transport = notify_transport(&cx);
    let mut sessions = ctx.sessions.write();
    // handle_new 内部已含：SessionState 构造（含 lsp_pool/lease）、
    // AvailableCommandsUpdate 通知、prewarm_session_mcp_discovery。
    let result = crate::host::requests::session_lifecycle::handle_new(
        &params,
        &ctx.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .map_err(map_acp_error)?;
    let resp: NewSessionResponse =
        serde_json::from_value(result).map_err(|e| Error::internal_error().data(e.to_string()))?;
    responder.respond(resp)
}

/// session/load 处理器（thin adapter → `session_lifecycle::handle_load`；
/// history replay、config options 更新、AvailableCommandsUpdate 通知、
/// prewarm 均在 requests 侧发出）。
pub(crate) async fn handle_load(
    ctx: &StdioContext,
    req: LoadSessionRequest,
    responder: Responder<LoadSessionResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let params =
        serde_json::to_value(&req).map_err(|e| Error::internal_error().data(e.to_string()))?;
    let transport = notify_transport(&cx);
    let mut sessions = ctx.sessions.write();
    let result = crate::host::requests::session_lifecycle::handle_load(
        &params,
        &ctx.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .map_err(map_acp_error)?;
    let resp: LoadSessionResponse =
        serde_json::from_value(result).map_err(|e| Error::internal_error().data(e.to_string()))?;
    responder.respond(resp)
}

/// session/resume 处理器（thin adapter → `session_lifecycle::handle_resume`；
/// AvailableCommandsUpdate 通知 + prewarm 已并入 requests 侧）。
pub(crate) async fn handle_resume(
    ctx: &StdioContext,
    req: ResumeSessionRequest,
    responder: Responder<ResumeSessionResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let params =
        serde_json::to_value(&req).map_err(|e| Error::internal_error().data(e.to_string()))?;
    let transport = notify_transport(&cx);
    let mut sessions = ctx.sessions.write();
    let result = crate::host::requests::session_lifecycle::handle_resume(
        &params,
        &ctx.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .map_err(map_acp_error)?;
    let resp: ResumeSessionResponse =
        serde_json::from_value(result).map_err(|e| Error::internal_error().data(e.to_string()))?;
    responder.respond(resp)
}

/// session/fork 处理器（thin adapter → `session_lifecycle::handle_fork`；
/// AvailableCommandsUpdate 通知 + prewarm 已并入 requests 侧）。
pub(crate) async fn handle_fork(
    ctx: &StdioContext,
    req: ForkSessionRequest,
    responder: Responder<ForkSessionResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let params =
        serde_json::to_value(&req).map_err(|e| Error::internal_error().data(e.to_string()))?;
    let transport = notify_transport(&cx);
    let mut sessions = ctx.sessions.write();
    let result = crate::host::requests::session_lifecycle::handle_fork(
        &params,
        &ctx.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .map_err(map_acp_error)?;
    let resp: ForkSessionResponse =
        serde_json::from_value(result).map_err(|e| Error::internal_error().data(e.to_string()))?;
    responder.respond(resp)
}

#[cfg(test)]
#[path = "create_test.rs"]
mod tests;
