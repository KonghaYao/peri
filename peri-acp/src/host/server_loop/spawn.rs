//! 收包循环的任务派生：每个请求只做短准入，长执行交给 host task scope。

use std::sync::Arc;

use serde_json::Value;

use super::ServerLoop;
use crate::host::diagnostics::ResponseDiagnostics;
use crate::host::session_endpoint::register_session_endpoint;
use crate::host::{
    dispatch_prompt_turn, extract_session_id, mcp_apps, requests, send_session_info_update,
    task_scope,
};
use crate::transport::types::RequestId;

impl ServerLoop<'_> {
    pub(super) async fn spawn_mcp_control(&self, id: RequestId, params: Value) {
        let diagnostics = ResponseDiagnostics::new(id, "peri/mcp/control", &params);
        let connection = self.connection.lock().await.facts();
        let prepared = {
            let sessions = self.sessions.lock().await;
            requests::mcp_control::prepare(&params, &connection, &sessions)
        };
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = diagnostics.send(self.transport.as_ref(), Err(error)).await;
                return;
            }
        };
        let transport = Arc::clone(self.transport);
        let rejected_transport = Arc::clone(&transport);
        let rejected_diagnostics = diagnostics.clone();
        // The request observer may disappear; cleanup remains owned by the MCP pool.
        // Do not race this future against transport cancellation and detach its work.
        let spawned = self.cfg.host_task_spawner.spawn(
            task_scope::HostTaskOwnerKind::Session,
            task_scope::HostTaskKind::McpControl,
            async move {
                let result = requests::mcp_control::execute(prepared).await;
                let _ = diagnostics.send(transport.as_ref(), result).await;
            },
        );
        if spawned.is_err() {
            let _ = rejected_diagnostics
                .send(
                    rejected_transport.as_ref(),
                    Err(requests::mcp_control::error(
                        -32000,
                        "pool_closed",
                        "ACP host is closing",
                    )),
                )
                .await;
        }
    }

    pub(super) async fn spawn_session_io(&self, id: RequestId, method: String, params: Value) {
        let diagnostics = ResponseDiagnostics::new(id, &method, &params);
        let rejected_diagnostics = diagnostics.clone();
        let cfg = Arc::clone(self.cfg);
        let transport = Arc::clone(self.transport);
        let rejected_transport = Arc::clone(&transport);
        let sessions = Arc::clone(self.sessions);
        let cancellation = self.connection_cancellation.clone();
        let spawned = self.cfg.host_task_spawner.spawn(
            task_scope::HostTaskOwnerKind::Session,
            task_scope::HostTaskKind::SessionRequest,
            async move {
                let request = async {
                    let prepared = {
                        let sessions = sessions.lock().await;
                        requests::session_io::prepare(&method, &params, &sessions)?
                    };
                    let local = prepared.as_ref().map(|environment| &environment.cfg).unwrap_or(&cfg);
                    requests::session_io::handle(&method, &params, local, &transport).await
                };
                let result = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => Err(crate::transport::types::AcpError::new(-32800, "request cancelled")),
                    result = request => result,
                };
                let _ = diagnostics.send(transport.as_ref(), result).await;
            },
        );
        if spawned.is_err() {
            let _ = rejected_diagnostics
                .send(
                    rejected_transport.as_ref(),
                    Err(crate::transport::types::AcpError::new(
                        -32800,
                        "session request host is closing",
                    )),
                )
                .await;
        }
    }

    pub(super) async fn spawn_cancel_bg_task(&self, id: RequestId, params: Value) {
        let diagnostics = ResponseDiagnostics::new(id, "session/cancel-bg-task", &params);
        let environment = {
            let sessions = self.sessions.lock().await;
            match params.get("sessionId").and_then(Value::as_str) {
                None => Err(crate::transport::types::AcpError::new(
                    -32602,
                    "missing sessionId",
                )),
                Some(session_id) => match sessions.get(session_id) {
                    Some(state) if state.closing => Err(crate::transport::types::AcpError::new(
                        -32010,
                        "Session is closing",
                    )),
                    Some(state) => Ok(state.environment.clone()),
                    None => Err(crate::transport::types::AcpError::new(
                        -32602,
                        "session not found",
                    )),
                },
            }
        };
        let environment = match environment {
            Ok(environment) => environment,
            Err(error) => {
                let _ = diagnostics.send(self.transport.as_ref(), Err(error)).await;
                return;
            }
        };
        let cfg = Arc::clone(self.cfg);
        let transport = Arc::clone(self.transport);
        let rejected_transport = Arc::clone(&transport);
        let rejected_diagnostics = diagnostics.clone();
        let cancellation = self.connection_cancellation.clone();
        let spawned = self.cfg.host_task_spawner.spawn(
            task_scope::HostTaskOwnerKind::Session,
            task_scope::HostTaskKind::SessionRequest,
            async move {
                let local_cfg = environment
                    .as_ref()
                    .map(|environment| &environment.cfg)
                    .unwrap_or(cfg.as_ref());
                let result = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => Err(crate::transport::types::AcpError::new(-32800, "request cancelled")),
                    result = requests::session_lifecycle::handle_cancel_bg_task(&params, local_cfg) => result,
                };
                let _ = diagnostics.send(transport.as_ref(), result).await;
            },
        );
        if spawned.is_err() {
            let _ = rejected_diagnostics
                .send(
                    rejected_transport.as_ref(),
                    Err(crate::transport::types::AcpError::new(
                        -32800,
                        "session request host is closing",
                    )),
                )
                .await;
        }
    }

    pub(super) async fn spawn_prompt(&self, id: RequestId, params: Value) {
        let diagnostics = ResponseDiagnostics::new(id, "session/prompt", &params);
        let sessions = self.sessions;
        let transport = self.transport;
        let prompt_locks = self.prompt_locks;
        let cfg = self.cfg;
        let cont_tx = self.cont_tx;
        // Spawn long-running prompt execution so the server loop
        // continues processing session/cancel notifications.
        // 新 prompt **不再撤销**已有 App record：App 生命周期不绑 turn generation
        //（设计 §4「新 turn 不撤销 App」；撤销入口是 `apps/close` 与连接关闭）。
        let prompt_session_id = extract_session_id(&params, "").to_string();
        // 本连接注册为本会话的交互 endpoint：prompt 装配点据此按 transport 发布
        // 当前轮 endpoint（跨轮不缓存 broker；连接关闭即 retire）。
        register_session_endpoint(
            self.sessions,
            &prompt_session_id,
            self.connection,
            self.transport,
        )
        .await;
        let sessions = sessions.clone();
        let transport = Arc::clone(transport);
        let prompt_locks = prompt_locks.clone();
        let cfg = Arc::clone(cfg);
        let cont_tx = cont_tx.clone();
        let prompt_spawner = cfg.host_task_spawner.clone();
        let rejected_transport = Arc::clone(&transport);
        let rejected_diagnostics = diagnostics.clone();
        let spawn_result = prompt_spawner.spawn(
            task_scope::HostTaskOwnerKind::Session,
            task_scope::HostTaskKind::Prompt,
            async move {
                let result = dispatch_prompt_turn(
                    params,
                    crate::host::PromptOrigin::User,
                    None,
                    &sessions,
                    &prompt_locks,
                    &transport,
                    &cfg,
                    &cont_tx,
                )
                .await;
                crate::host::user_input::schedule_mailbox(
                    &prompt_session_id,
                    &sessions,
                    &prompt_locks,
                    &cfg,
                    &transport,
                    &cont_tx,
                );
                if diagnostics.send(transport.as_ref(), result).await.is_err() {
                    return;
                }
                if !prompt_session_id.is_empty() {
                    send_session_info_update(transport.as_ref(), &prompt_session_id).await;
                }
            },
        );
        if spawn_result.is_err() {
            let _ = rejected_diagnostics
                .send(
                    rejected_transport.as_ref(),
                    Err(crate::transport::types::AcpError::new(
                        -32800,
                        "request cancelled",
                    )),
                )
                .await;
        }
    }

    /// 旧别名（`app` / `resource`）的会话定位：本连接必须是该会话的在线 endpoint，
    /// 且该会话的附件表认识信封里的 `appSessionId`（附件身份）。
    pub(super) async fn locate_alias_session(&self, params: &Value) -> Option<String> {
        let attachment_id = params.get("appSessionId").and_then(Value::as_str)?;
        let attachment_id =
            peri_acp_types::interaction::AttachmentId::new(attachment_id.to_string());
        let connection_id = self.connection.lock().await.id().to_string();
        let sessions = self.sessions.lock().await;
        for (session_id, state) in sessions.iter() {
            if state.closing {
                continue;
            }
            let Some(environment) = state.environment.as_ref() else {
                continue;
            };
            let router = environment.interaction_router();
            if router.is_online_endpoint(&connection_id) && router.lookup(&attachment_id).is_some()
            {
                return Some(session_id.clone());
            }
        }
        None
    }

    pub(super) async fn spawn_mcp_apps_request(
        &self,
        id: RequestId,
        method: String,
        params: Value,
    ) {
        let diagnostics = ResponseDiagnostics::new(id, &method, &params);
        let transport = self.transport;
        let cfg = self.cfg;
        let connection = self.connection;
        let connection_cancellation = self.connection_cancellation;
        let sessions = self.sessions;
        let transport = Arc::clone(transport);
        // 会话定位：七个新方法带显式 `sessionId`（只定位）；旧 4 名的信封没有该
        // 字段，按其自有的归属字段反查（`ownerSessionId` / `appSessionId` → 连接
        // 表的 lease owner）——该反查随旧名删除一并消失（S8）。
        let owner_id = if method.starts_with("peri/mcp/apps/") {
            params
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else if method == "peri/mcp/open" || method == "peri/mcp/invoke" {
            params
                .get("ownerSessionId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else {
            // 旧 `peri/mcp/app` / `peri/mcp/resource` 的信封只有 `appSessionId`
            //（= 附件身份，§A-2）：在「本连接为在线 endpoint」的会话中按附件反查
            //（attachment_id 全局唯一，查询不是状态；不使用任何连接级 lease 表）。
            self.locate_alias_session(&params).await
        };
        let environment = match owner_id.as_deref() {
            Some(id) => self.sessions.lock().await.get(id).and_then(|state| {
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
        // 会话级 facade（`apps/*` 的唯一入口）：与 relay 同一会话环境、同一 router、
        // 同一 runtime epoch；未装配（apps 能力关闭 / 非会话级装配）即显式拒绝。
        let facade = environment
            .as_ref()
            .and_then(|env| env.apps_facade().cloned());
        // session 交互路由：App 回调 / Host invoke 的审批 endpoint 选择与
        // attachment 路由绑定都经它（连接先注册为本会话的在线 endpoint）；
        // 新方法的附件绑定由 facade 内部完成，本处不再补丁（S6 起）。
        if let Some(session_id) = owner_id.as_deref() {
            register_session_endpoint(self.sessions, session_id, self.connection, self.transport)
                .await;
        }
        let connection = Arc::clone(connection);
        let sessions = Arc::clone(sessions);
        let app_spawner = cfg.host_task_spawner.clone();
        let connection_cancellation = connection_cancellation.clone();
        let rejected_transport = Arc::clone(&transport);
        let rejected_diagnostics = diagnostics.clone();
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
                            "peri/mcp/apps/rerun" => {
                                // Host 归属回本连接：session 交互路由按**请求作用域**
                                // 选择审批 endpoint，与 ledger 校验后的
                                // `{namespace, seq}` 构成同一对权威值。
                                let mut request_connection = {
                                    let connection = connection.lock().await;
                                    connection.snapshot_for_request()
                                };
                                let connection_id = request_connection.id().to_string();
                                crate::host::interaction_router::scope_host_request_connection(
                                    connection_id,
                                    mcp_apps::handle_request(
                                        &method,
                                        &params,
                                        &mut request_connection,
                                        facade.as_ref(),
                                    ),
                                )
                                .await
                            }
                            "peri/mcp/invoke" => {
                                let owner_session_id = params
                                    .get("ownerSessionId")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                // 会话准入快照 = 存在性 + 未关闭（R8：不再含「prompt 在飞」
                                // 判据；执行的顺序与资格由 canonical service 的共同准入决定）。
                                let gate = {
                                    let sessions = sessions.lock().await;
                                    match sessions.get(owner_session_id) {
                                        Some(state) => mcp_apps::InvokeSessionGate {
                                            known: true,
                                            open: !state.closing,
                                        },
                                        None => mcp_apps::InvokeSessionGate {
                                            known: false,
                                            open: false,
                                        },
                                    }
                                };
                                let request_connection = {
                                    let connection = connection.lock().await;
                                    connection.snapshot_for_request()
                                };
                                // 请求作用域注入：本连接的 Host 审批归属回本连接
                                //（session 交互路由按它选 endpoint）。
                                let invocation = crate::host::interaction_router::scope_host_request_connection(
                                    request_connection.id().to_string(),
                                    mcp_apps::handle_invoke(
                                        &params,
                                        &request_connection,
                                        relay.as_ref(),
                                        facade.as_ref(),
                                        gate,
                                    ),
                                );
                                match invocation.await
                                {
                                    // R3：Host 回调**不得**冒充模型工具调用——结果只经
                                    // 原请求返回，不发任何合成模型卡片（`session/update`）。
                                    Ok(result) => Ok(result.value),
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
                                    facade.as_ref(),
                                )
                                .await
                            }
                        }
                    } => result,
                };
                let _ = diagnostics.send(transport.as_ref(), result).await;
            },
        );
        if spawn_result.is_err() {
            let _ = rejected_diagnostics
                .send(
                    rejected_transport.as_ref(),
                    Err(crate::transport::types::AcpError::new(
                        -32800,
                        "request cancelled",
                    )),
                )
                .await;
        }
    }
}
