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
                    "peri/mcp/open" | "peri/mcp/app" | "peri/mcp/resource" | "peri/mcp/invoke" => {
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
            let relay = sessions
                .lock()
                .await
                .get(&prompt_session_id)
                .and_then(|state| state.environment.as_ref())
                .and_then(|env| env.cfg.mcp_apps_relay.clone())
                .or_else(|| cfg.mcp_apps_relay.clone());
            if let Some(relay) = relay {
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
        let sessions = self.sessions;
        let transport = Arc::clone(transport);
        let owner_id = if method == "peri/mcp/open" {
            params
                .get("ownerSessionId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else {
            let state = connection.lock().await;
            params
                .get("appSessionId")
                .and_then(Value::as_str)
                .and_then(|id| state.app_session(id))
                .map(|binding| binding.owner_session_id.clone())
        };
        let environment = match owner_id {
            Some(id) => self.sessions.lock().await.get(&id).and_then(|state| {
                (!state.closing)
                    .then(|| state.environment.clone())
                    .flatten()
            }),
            None => None,
        };
        let relay = environment
            .as_ref()
            .and_then(|env| env.cfg.mcp_apps_relay.clone())
            .or_else(|| cfg.mcp_apps_relay.clone());
        let connection = Arc::clone(connection);
        let sessions = Arc::clone(sessions);
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
                            "peri/mcp/invoke" => {
                                let owner_session_id = params
                                    .get("ownerSessionId")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                let gate = {
                                    let sessions = sessions.lock().await;
                                    match sessions.get(owner_session_id) {
                                        Some(state) => mcp_apps::InvokeSessionGate {
                                            known: true,
                                            owned: state.execution_owner.is_some()
                                                && !state.closing,
                                            prompt_in_flight: state.cancel_token.is_some(),
                                        },
                                        None => mcp_apps::InvokeSessionGate {
                                            known: false,
                                            owned: false,
                                            prompt_in_flight: false,
                                        },
                                    }
                                };
                                let request_connection = {
                                    let connection = connection.lock().await;
                                    connection.snapshot_for_request()
                                };
                                match mcp_apps::handle_invoke(
                                    &params,
                                    &request_connection,
                                    relay.as_ref(),
                                    gate,
                                )
                                .await
                                {
                                    Ok(result) => {
                                        for update in result.updates {
                                            let payload = serde_json::to_value(
                                                agent_client_protocol::schema::v1::SessionNotification::new(
                                                    agent_client_protocol::schema::v1::SessionId::new(
                                                        result.session_id.clone(),
                                                    ),
                                                    update,
                                                ),
                                            )
                                            .map_err(|_| {
                                                crate::transport::types::AcpError::new(
                                                    -32603,
                                                    "MCP Apps relay request failed",
                                                )
                                            })?;
                                            if let Err(error) = transport
                                                .send_notification("session/update", payload)
                                                .await
                                            {
                                                tracing::warn!(
                                                    %error,
                                                    "MCP Apps invoke session/update send failed"
                                                );
                                            }
                                        }
                                        Ok(result.value)
                                    }
                                    Err(error) => Err(error),
                                }
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
        let lifecycle_lock = if matches!(
            method.as_str(),
            "session/load"
                | "session/resume"
                | "session/fork"
                | "session/rewind"
                | "session/close"
                | "session/delete"
        ) {
            let lifecycle_session_id = extract_session_id(&params, "").to_owned();
            if matches!(method.as_str(), "session/close" | "session/delete") {
                if let Some(state) = sessions.lock().await.get_mut(&lifecycle_session_id) {
                    state.closing = true;
                    state.continuation_armed = false;
                    if let Some(token) = &state.cancel_token {
                        token.cancel();
                    }
                }
                cfg.session_manager.pre_close_session(&lifecycle_session_id);
            }
            let lock = self
                .prompt_locks
                .lock()
                .await
                .entry(lifecycle_session_id)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone();
            if matches!(method.as_str(), "session/close" | "session/delete") {
                match tokio::time::timeout(std::time::Duration::from_secs(5), lock.lock_owned())
                    .await
                {
                    Ok(guard) => Some(guard),
                    Err(_) => {
                        let _ = transport
                            .send_response(
                                id,
                                Err(crate::transport::types::AcpError::new(
                                    -32010,
                                    "Session close incomplete: active execution has not stopped",
                                )),
                            )
                            .await;
                        return;
                    }
                }
            } else {
                match lock.try_lock_owned() {
                    Ok(guard) => Some(guard),
                    Err(_) => {
                        let _ = transport
                            .send_response(
                                id,
                                Err(crate::transport::types::AcpError::new(
                                    -32010,
                                    "Session is executing; retry after the current turn",
                                )),
                            )
                            .await;
                        return;
                    }
                }
            }
        } else {
            None
        };
        let result = {
            let mut sessions = sessions.lock().await;
            handle_request(&method, &params, cfg, &mut sessions, transport).await
        };
        drop(lifecycle_lock);
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
                let environment = sessions
                    .lock()
                    .await
                    .get(&session_id)
                    .and_then(|state| state.environment.clone());
                let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
                requests::session_lifecycle::after_new_response(local, transport, &session_id)
                    .await;
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
                let relay = sessions
                    .lock()
                    .await
                    .get(session_id)
                    .and_then(|state| state.environment.as_ref())
                    .and_then(|env| env.cfg.mcp_apps_relay.clone())
                    .or_else(|| cfg.mcp_apps_relay.clone());
                if let Some(relay) = relay {
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
