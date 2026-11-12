//! Notification dispatch kept separate from request lifecycle admission.

use serde_json::Value;

use super::ServerLoop;
use crate::host::{extract_session_id, handle_notification, requests};

impl ServerLoop<'_> {
    pub(super) async fn dispatch_notification(&self, method: String, params: Value) {
        let cfg = self.cfg;
        let sessions = self.sessions;
        let cont_tx = self.cont_tx;
        if method == "mcp/message" {
            // client 宿主的 MCP server 反向下发的通知：按 serverId 路由到承载
            // 会话的服务。就地 await（不 spawn）以保住与后续请求的相对次序，但只
            // 在短锁内定位承载者——投递等待由对端决定，不得占着会话锁。
            let routed = {
                let registry = self.connection.lock().await.acp_mcp.clone();
                let sessions = sessions.lock().await;
                requests::acp_mcp::route_inbound(&registry, &sessions, &params)
            };
            match routed {
                Ok((port, inbound)) => {
                    if let Err(error) = port.notify(inbound).await {
                        tracing::warn!(
                            method,
                            code = error.code,
                            error = %error.message,
                            "MCP over ACP 通知未能投递"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        method,
                        code = error.code,
                        error = %error.message,
                        "MCP over ACP 通知未能路由"
                    );
                }
            }
            return;
        }
        // `session/cancel` 是轮次级取消，不触碰 Apps 域；会话级拆除只走 request close。
        // 锁外发送 continuation，避免 notification 路径持会话锁触碰 scheduler。
        let outcome = {
            let mut sessions = sessions.lock().await;
            handle_notification(&method, &params, &mut sessions, cfg).await
        };
        if let Some(req) = outcome.continuation {
            if cont_tx.send(req).is_err() {
                tracing::warn!(
                    method,
                    session_id = extract_session_id(&params, ""),
                    "ACP continuation scheduling failed: channel closed"
                );
            }
        }
        // 用户 Stop 与模型配置变化的锁外持久化：失败只记录，不改变通知本身的结算。
        super::super::notify::persist_notification_effects(
            &method,
            cfg,
            outcome.stop_targets,
            outcome.context_usage_invalidations,
        )
        .await;
    }
}
