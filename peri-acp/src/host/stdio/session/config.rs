//! 会话配置：set_mode / set_config_option / update_config。
//!
//! 批 2 起全部转发到 requests 侧唯一实现（`host::requests::config_options`），
//! 本文件只剩协议层 typed 转换。
//!
//! `await_holding_lock` 快注：adapter 跨 await 持有 `ctx.sessions` 写锁是转发
//! 契约（同 `host/stdio/session/create.rs`，parking_lot 已开 `send_guard`）。

#![allow(clippy::await_holding_lock)]

use agent_client_protocol::{
    schema::v1::{
        SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, SetSessionModeRequest,
        SetSessionModeResponse,
    },
    Client, ConnectionTo, Error, Handled, Responder, UntypedMessage,
};

use super::super::context::StdioContext;
use super::super::transport::{map_acp_error, notify_transport};

/// 处理 session/set_mode（thin adapter → `config_options::handle_set_mode`）。
pub(crate) async fn handle_set_mode(
    ctx: &StdioContext,
    req: SetSessionModeRequest,
    responder: Responder<SetSessionModeResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), Error> {
    let params =
        serde_json::to_value(&req).map_err(|e| Error::internal_error().data(e.to_string()))?;
    let transport = notify_transport(&cx);
    let result =
        crate::host::requests::config_options::handle_set_mode(&params, &ctx.cfg, &transport)
            .await
            .map_err(map_acp_error)?;
    let resp: SetSessionModeResponse =
        serde_json::from_value(result).map_err(|e| Error::internal_error().data(e.to_string()))?;
    responder.respond(resp)
}

/// 处理 session/set_config_option（thin adapter →
/// `config_options::handle_set_config_option`）。
///
/// ACP schema `value` 为 flatten 的 `{value: "..."}`（ValueId 变体 untagged），
/// `serde_json::to_value` 后 `params.value` 恰为字符串，与 requests 侧
/// `params.get("value").as_str()` 消费形态一致；Boolean 变体 → `value: true`
/// → requests 侧 as_str None → no-op（与旧 typed 分支行为一致）。
pub(crate) async fn handle_set_config_option(
    ctx: &StdioContext,
    req: SetSessionConfigOptionRequest,
    responder: Responder<SetSessionConfigOptionResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), Error> {
    let params =
        serde_json::to_value(&req).map_err(|e| Error::internal_error().data(e.to_string()))?;
    let transport = notify_transport(&cx);
    let mut sessions = ctx.sessions.write();
    let result = crate::host::requests::config_options::handle_set_config_option(
        &params,
        &ctx.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .map_err(map_acp_error)?;
    let resp: SetSessionConfigOptionResponse =
        serde_json::from_value(result).map_err(|e| Error::internal_error().data(e.to_string()))?;
    responder.respond(resp)
}

/// 处理 session/update_config (custom extension，thin adapter →
/// `config_options::handle_update_config`)。
pub(crate) async fn handle_update_config(
    ctx: &StdioContext,
    req: UntypedMessage,
    responder: Responder<serde_json::Value>,
    cx: ConnectionTo<Client>,
) -> Result<Handled<(UntypedMessage, Responder<serde_json::Value>)>, Error> {
    // Only handle session/update_config; pass through all others
    if req.method() != "session/update_config" {
        return Ok(Handled::No {
            message: (req, responder),
            retry: false,
        });
    }

    let transport = notify_transport(&cx);
    let mut sessions = ctx.sessions.write();
    let result = crate::host::requests::config_options::handle_update_config(
        req.params(),
        &ctx.cfg,
        &mut sessions,
        &transport,
    )
    .await
    .map_err(map_acp_error)?;
    let _ = responder.respond(result);
    Ok(Handled::Yes)
}
