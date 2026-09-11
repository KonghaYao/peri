//! Receive loop and request-class dispatch; long-running work stays host-owned.

use std::sync::Arc;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{
    connection::ConnectionContext, dispatch_prompt_turn, extract_session_id, handle_notification,
    handle_request, mcp_apps, requests, send_session_info_update, task_scope, AcpServerConfig,
    PromptLocks, SharedSessions,
};
use crate::transport::types::{IncomingMessage, RequestId};

pub(super) struct ServerLoop<'a> {
    pub(super) transport: &'a Arc<dyn crate::transport::AcpTransport>,
    pub(super) cfg: &'a Arc<AcpServerConfig>,
    pub(super) sessions: &'a SharedSessions,
    pub(super) prompt_locks: &'a PromptLocks,
    pub(super) cont_tx:
        &'a Arc<tokio::sync::mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>>,
    pub(super) connection: &'a Arc<tokio::sync::Mutex<ConnectionContext>>,
    pub(super) connection_cancellation: &'a CancellationToken,
}

impl ServerLoop<'_> {
    pub(super) async fn run(&self) {
        while let Some(msg) = self.transport.recv().await {
            match msg {
                IncomingMessage::Request { id, method, params } => match method.as_str() {
                    "session/prompt" => self.spawn_prompt(id, params).await,
                    "peri/mcp/open" | "peri/mcp/app" | "peri/mcp/resource" => {
                        self.spawn_mcp_apps_request(id, method, params).await;
                    }
                    _ => self.dispatch_request(id, method, params).await,
                },
                IncomingMessage::Notification { method, params } => {
                    self.dispatch_notification(method, params).await;
                }
                IncomingMessage::Response { .. } => {
                    // Responses are routed internally by the transport's pending map.
                }
            }
        }
    }

    async fn spawn_prompt(&self, id: RequestId, params: Value) {
        let sessions = self.sessions;
        let transport = self.transport;
        let prompt_locks = self.prompt_locks;
        let cfg = self.cfg;
        let cont_tx = self.cont_tx;
        // Spawn long-running prompt execution so the server loop
        // continues processing session/cancel notifications.
        let prompt_session_id = extract_session_id(&params, "").to_string();
        if !prompt_session_id.is_empty() {
            if let Some(relay) = cfg.mcp_apps_relay.as_ref() {
                relay.begin_session_turn(&prompt_session_id);
            }
        }
        let sessions = sessions.clone();
        let transport = Arc::clone(transport);
        let prompt_locks = prompt_locks.clone();
        let cfg = Arc::clone(cfg);
        let cont_tx = cont_tx.clone();
        let prompt_spawner = cfg.host_task_spawner.clone();
        let rejected_transport = Arc::clone(&transport);
        let rejected_id = id.clone();
        let spawn_result = prompt_spawner.spawn(
            task_scope::HostTaskOwnerKind::Session,
            task_scope::HostTaskKind::Prompt,
            async move {
                let result = dispatch_prompt_turn(
                    params,
                    false,
                    None,
                    &sessions,
                    &prompt_locks,
                    &transport,
                    &cfg,
                    &cont_tx,
                )
                .await;
                super::user_input::schedule_mailbox(
                    &prompt_session_id,
                    &sessions,
                    &prompt_locks,
                    &cfg,
                    &transport,
                    &cont_tx,
                );
                if let Err(error) = transport.send_response(id, result).await {
                    tracing::warn!(%error, "prompt terminal response send failed");
                    return;
                }
                if !prompt_session_id.is_empty() {
                    send_session_info_update(transport.as_ref(), &prompt_session_id).await;
                }
            },
        );
        if spawn_result.is_err() {
            if let Err(error) = rejected_transport
                .send_response(
                    rejected_id,
                    Err(crate::transport::types::AcpError::new(
                        -32800,
                        "request cancelled",
                    )),
                )
                .await
            {
                tracing::warn!(%error, "rejected prompt response send failed");
            }
        }
    }

    async fn spawn_mcp_apps_request(&self, id: RequestId, method: String, params: Value) {
        let transport = self.transport;
        let cfg = self.cfg;
        let connection = self.connection;
        let connection_cancellation = self.connection_cancellation;
        let transport = Arc::clone(transport);
        let relay = cfg.mcp_apps_relay.clone();
        let connection = Arc::clone(connection);
        let app_spawner = cfg.host_task_spawner.clone();
        let connection_cancellation = connection_cancellation.clone();
        let rejected_transport = Arc::clone(&transport);
        let rejected_id = id.clone();
        let spawn_result = app_spawner.spawn(
            task_scope::HostTaskOwnerKind::Connection,
            task_scope::HostTaskKind::McpAppsRelay,
            async move {
                let result = tokio::select! {
                    _ = connection_cancellation.cancelled() => {
                        Err(crate::transport::types::AcpError::new(-32800, "request cancelled"))
                    }
                    result = async {
                        match method.as_str() {
                            "peri/mcp/open" => {
                                let mut connection = connection.lock().await;
                                mcp_apps::handle_request(
                                    &method,
                                    &params,
                                    &mut connection,
                                    relay.as_ref(),
                                )
                                .await
                            }
                            _ => {
                                let mut request_connection = {
                                    let connection = connection.lock().await;
                                    connection.snapshot_for_request()
                                };
                                mcp_apps::handle_request(
                                    &method,
                                    &params,
                                    &mut request_connection,
                                    relay.as_ref(),
                                )
                                .await
                            }
                        }
                    } => result,
                };
                if let Err(error) = transport.send_response(id, result).await {
                    tracing::warn!(%error, "MCP Apps terminal response send failed");
                }
            },
        );
        if spawn_result.is_err() {
            let _ = rejected_transport
                .send_response(
                    rejected_id,
                    Err(crate::transport::types::AcpError::new(
                        -32800,
                        "request cancelled",
                    )),
                )
                .await;
        }
    }

    async fn dispatch_request(&self, id: RequestId, method: String, params: Value) {
        let transport = self.transport;
        let cfg = self.cfg;
        let sessions = self.sessions;
        let connection = self.connection;
        let closed_session_id = matches!(method.as_str(), "session/close" | "session/delete")
            .then(|| extract_session_id(&params, "").to_string())
            .filter(|session_id| !session_id.is_empty());
        let result = {
            let mut sessions = sessions.lock().await;
            handle_request(&method, &params, cfg, &mut sessions, transport).await
        };
        if result.is_ok() && super::user_input::starts_execution(&method) {
            if let Some(session_id) = params.get("sessionId").and_then(Value::as_str) {
                super::user_input::schedule_mailbox(
                    session_id,
                    self.sessions,
                    self.prompt_locks,
                    self.cfg,
                    self.transport,
                    self.cont_tx,
                );
            }
        }
        if method == "initialize" && result.is_ok() {
            connection.lock().await.commit_initialize();
        }
        if result.is_ok() {
            if let (Some(session_id), Some(relay)) =
                (closed_session_id.as_deref(), cfg.mcp_apps_relay.as_ref())
            {
                relay.close_session(session_id);
            }
        }
        let new_session_id = (method == "session/new")
            .then(|| {
                result
                    .as_ref()
                    .ok()?
                    .get("sessionId")?
                    .as_str()
                    .map(str::to_owned)
            })
            .flatten();
        let response_sent = transport.send_response(id, result).await.is_ok();
        if response_sent {
            if let Some(session_id) = new_session_id {
                requests::session_lifecycle::after_new_response(cfg, transport, &session_id).await;
            }
        }
    }

    async fn dispatch_notification(&self, method: String, params: Value) {
        let cfg = self.cfg;
        let sessions = self.sessions;
        let cont_tx = self.cont_tx;
        if method == "session/cancel" {
            let session_id = extract_session_id(&params, "");
            if !session_id.is_empty() {
                if let Some(relay) = cfg.mcp_apps_relay.as_ref() {
                    relay.close_session(session_id);
                }
            }
        }
        // session/cancel 可能需要在锁外补发 continuation 请求
        // （race 兜底：bg 结果已 route 为 Defer，但通知可能在 cancel
        // 置位前被 scheduler 跳过）。unbounded send 虽不阻塞，仍统一
        // 在释放 sessions 锁后发送，避免 notify 路径持锁触碰 scheduler。
        let cont_req = {
            let mut sessions = sessions.lock().await;
            handle_notification(&method, &params, &mut sessions, cfg)
        };
        if let Some(req) = cont_req {
            let _ = cont_tx.send(req);
        }
    }
}
