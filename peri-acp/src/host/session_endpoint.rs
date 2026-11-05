//! 会话级交互 endpoint 注册：连接归属是会话，不是某一轮。
//!
//! 单独成模块是为了让 server loop 的恢复接线与轮次分发都能复用同一份幂等注册，
//! 而不把连接/传输细节带进恢复协调器。

use std::sync::Arc;

use super::connection::ConnectionContext;
use super::SharedSessions;

/// 把当前连接注册为本会话的交互 endpoint（幂等；表满/离线时记 warn 并继续——
/// 该连接的审批随后按无在线 endpoint 处理，是 fail-closed 而不是静默降级）。
pub(crate) async fn register_session_endpoint(
    sessions: &SharedSessions,
    session_id: &str,
    connection: &Arc<tokio::sync::Mutex<ConnectionContext>>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) {
    if session_id.is_empty() {
        return;
    }
    let connection_id = connection.lock().await.id().to_string();
    let environment = sessions
        .lock()
        .await
        .get(session_id)
        .and_then(|state| state.environment.clone());
    let Some(environment) = environment else {
        return;
    };
    if let Err(error) = environment
        .interaction_router()
        .register_connection(&connection_id, Arc::clone(transport))
    {
        tracing::warn!(
            session_id,
            connection_id = %connection_id,
            %error,
            "session interaction endpoint registration rejected"
        );
    }
}
