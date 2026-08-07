//! 传输层事件：initialize 响应 + type:cancel 中断钩子。

use std::sync::Arc;

use crate::dispatch;
use agent_client_protocol::{
    schema::v1::InitializeRequest, Client, ConnectionTo, LineDirection, Responder,
};
use peri_acp_types::PeriCaps;

use super::context::StdioContext;

/// initialize 请求处理器。
pub(super) async fn handle_initialize(
    ctx: &StdioContext,
    req: InitializeRequest,
    responder: Responder<agent_client_protocol::schema::v1::InitializeResponse>,
    _cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    tracing::info!("ACP initialize");

    // 解析 clientCapabilities._meta 中的 peri 自定义 flag
    let peri_caps = req
        .client_capabilities
        .meta
        .as_ref()
        .map(PeriCaps::from_client_meta)
        .unwrap_or_default();

    // 暂存到 SessionManager，session/new 时 consume
    ctx.session_manager.set_pending_caps(peri_caps.clone());

    let resp = dispatch::build_initialize_response(&peri_caps);
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
