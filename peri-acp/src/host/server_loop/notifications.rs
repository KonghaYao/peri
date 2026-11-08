//! 通知分派：锁内只处理取消/配置事实，锁外持久化交给 host task。

use std::sync::Arc;

use serde_json::Value;

use super::ServerLoop;
use crate::host::{extract_session_id, handle_notification, notify, requests, task_scope};

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
        // `session/cancel` 是**轮次级**取消：只中断本轮的模型执行，**不触碰
        // Apps 域**（设计 §2 / §4：App record 与已 dispatch 的调用不受 turn
        // 取消影响）。会话级拆除走 `session/close`（见 `dispatch_request` 的
        // `closed_session_id` 分支，那里才调用 `close_session`）。
        // session/cancel 可能需要在锁外补发 continuation 请求
        // （race 兜底：bg 结果已 route 为 Defer，但通知可能在 cancel
        // 置位前被 scheduler 跳过）。unbounded send 虽不阻塞，仍统一
        // 在释放 sessions 锁后发送，避免 notify 路径持锁触碰 scheduler。
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
        // 用户 Stop 与模型配置变化的锁外持久化：停止意图先落库；占用快照失效写
        // NULL。写入受存储争用支配，交给 host task 执行，收包循环不再等待同连接
        // 后续帧。失败只记 error/warn，不阻断取消/配置更新本身（设计 §5.4 / §2.2）。
        if !outcome.stop_targets.is_empty() || !outcome.context_usage_invalidations.is_empty() {
            let persist_method = method.clone();
            let cfg = Arc::clone(self.cfg);
            let sessions = Arc::clone(self.sessions);
            let prompt_locks = Arc::clone(self.prompt_locks);
            let spawned = self.cfg.host_task_spawner.spawn(
                task_scope::HostTaskOwnerKind::Session,
                task_scope::HostTaskKind::NotificationEffect,
                async move {
                    notify::persist_notification_effects(
                        &persist_method,
                        &cfg,
                        &sessions,
                        &prompt_locks,
                        outcome.stop_targets,
                        outcome.context_usage_invalidations,
                    )
                    .await;
                },
            );
            if spawned.is_err() {
                tracing::warn!(
                    method,
                    "notification effect persistence skipped: host scope closed"
                );
            }
        }
    }
}
