//! 传输层事件：initialize 响应 + type:cancel 中断钩子。
//!
//! 批 2：stdio 控制面迁移为 thin adapter。`handle_initialize` 转发到
//! `host::requests::session_lifecycle::handle_initialize`（唯一实现，含
//! `set_pending_caps`）；`StdioNotifyTransport` 是过渡期通知适配器——把
//! requests 侧经 `AcpTransport::send_notification` 发出的 `session/update`
//! payload（`{sessionId, update}`，与 `SessionNotification` 序列化逐字段
//! 相同，见 `host/unify_wire_baseline_test.rs`）还原为 typed
//! `SessionNotification` 走 `ConnectionTo<Client>` 发出，保持 wire 形态不变。
//! 批 3 删除 typed handler 后由 `transport/stdio.rs` + `run_acp_server` 取代。

use std::sync::Arc;

use agent_client_protocol::{
    schema::v1::{InitializeRequest, SessionNotification},
    Client, ConnectionTo, Error, LineDirection, Responder,
};
use async_trait::async_trait;
use serde_json::Value;

use super::context::StdioContext;
use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use crate::transport::AcpTransport;

/// 批 2 过渡期通知适配器：`AcpTransport` 全接口但仅支持
/// `send_notification("session/update", payload)`（requests 侧控制面方法所需
/// 的唯一出站形态）。其余方法不落（调用即内部错误）——stdio 无反请求/响应对
/// 需求；`recv` 永不产生入站消息（typed 框架自己驱动）。
///
/// 语义：payload 与 `SessionNotification` 序列化逐字段同构（批 0 wire 基线
/// 已证），`serde_json::from_value` 还原后经 `ConnectionTo<Client>::send_notification`
/// 走原有 typed 发送路径，wire 输出不变。
#[derive(Clone)]
pub(super) struct StdioNotifyTransport {
    cx: ConnectionTo<Client>,
}

#[async_trait]
impl AcpTransport for StdioNotifyTransport {
    async fn send_request(&self, method: &str, _params: Value) -> Result<Value, AcpError> {
        Err(AcpError::new(
            -32603,
            format!("StdioNotifyTransport: send_request({method}) not supported in 批 2 adapter"),
        ))
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        if method != "session/update" {
            return Err(AcpError::new(
                -32603,
                format!("StdioNotifyTransport: unexpected notification method {method}"),
            ));
        }
        let notif: SessionNotification = serde_json::from_value(params).map_err(|e| {
            AcpError::new(
                -32603,
                format!("notify adapter: invalid session/update payload: {e}"),
            )
        })?;
        self.cx
            .send_notification(notif)
            .map_err(|e| AcpError::new(-32603, format!("notify adapter: send failed: {e}")))
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        None
    }

    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Err(AcpError::new(
            -32603,
            "StdioNotifyTransport: send_response not supported in 批 2 adapter",
        ))
    }
}

/// 构造 requests 侧函数所需的 `&Arc<dyn AcpTransport>`。
pub(super) fn notify_transport(cx: &ConnectionTo<Client>) -> Arc<dyn AcpTransport> {
    Arc::new(StdioNotifyTransport { cx: cx.clone() })
}

/// 把 requests 侧 `AcpError` 收口到 agent-client-protocol 的 JSON-RPC `Error`。
pub(super) fn map_acp_error(e: AcpError) -> agent_client_protocol::Error {
    Error::new(e.code as i32, e.message)
}

/// initialize 请求处理器（thin adapter）：typed request → Value params →
/// `session_lifecycle::handle_initialize`（唯一实现，含 `set_pending_caps`）→
/// Value result → typed response。
pub(super) async fn handle_initialize(
    ctx: &StdioContext,
    req: InitializeRequest,
    responder: Responder<agent_client_protocol::schema::v1::InitializeResponse>,
    _cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    tracing::info!("ACP initialize");
    let params = serde_json::to_value(&req)
        .map_err(|e| Error::internal_error().data(format!("Serialize request failed: {e}")))?;
    let result = crate::host::requests::session_lifecycle::handle_initialize(&params, &ctx.cfg)
        .map_err(map_acp_error)?;
    let resp: agent_client_protocol::schema::v1::InitializeResponse =
        serde_json::from_value(result).map_err(|e| {
            Error::internal_error().data(format!("Deserialize response failed: {e}"))
        })?;
    responder.respond(resp)
}

/// 构建 type:cancel 中断钩子（供 Stdio::new().with_debug() 使用）。
pub(super) fn cancel_debug_hook(ctx: Arc<StdioContext>) -> impl Fn(&str, LineDirection) {
    move |line: &str, _direction| {
        if line.trim() == r#"{"type":"cancel"}"# {
            let guard = ctx.sessions.read();
            for (sid, s) in guard.iter() {
                if let Some(ref token) = s.cancel_token {
                    token.cancel();
                    tracing::info!(session_id = %sid, "Cancelled via type:cancel");
                }
            }
        }
    }
}
