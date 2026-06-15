//! 传输层事件：initialize 响应 + type:cancel 中断钩子。

use std::sync::Arc;

use agent_client_protocol::{
    schema::InitializeRequest, Client, ConnectionTo, LineDirection, Responder,
};
use peri_acp::dispatch;

use super::context::StdioContext;

/// initialize 请求处理器。
pub(super) async fn handle_initialize(
    _req: InitializeRequest,
    responder: Responder<agent_client_protocol::schema::InitializeResponse>,
    _cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    tracing::info!("ACP initialize");
    responder.respond(dispatch::build_initialize_response())
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
