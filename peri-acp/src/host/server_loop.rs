//! Receive loop and request-class dispatch; long-running work stays host-owned.

mod history;

use std::sync::Arc;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::diagnostics::ResponseDiagnostics;
use super::session_endpoint::register_session_endpoint;
use super::{
    connection::ConnectionContext, dispatch_prompt_turn, extract_session_id, handle_notification,
    handle_request_for_connection, history_page, history_v2_load, history_window, mcp_apps,
    requests, send_session_info_update, task_scope, AcpServerConfig, PromptLocks, SharedSessions,
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
    /// 进入会话时的恢复协调（设计 §3/§4）：只由 setup 请求触发，启动延后到响应之后。
    pub(super) recovery: &'a super::recovery::HostSessionRecovery,
}

impl ServerLoop<'_> {
    pub(super) async fn run(&self) {
        while let Some(msg) = self.transport.recv().await {
            match msg {
                IncomingMessage::Request { id, method, params } => match method.as_str() {
                    "session/prompt" => self.spawn_prompt(id, params).await,
                    "peri/mcp/control" => self.spawn_mcp_control(id, params).await,
                    "session/input/snapshot" => {
                        self.spawn_session_io(id, method, params).await;
                    }
                    // 取消可能等待外部 owner 的 RPC。只做短锁存在性校验，执行放到
                    // 请求 task，避免接收循环被取消等待占住。
                    "session/cancel-bg-task" => {
                        self.spawn_cancel_bg_task(id, params).await;
                    }
                    // MCP Apps：七个新方法 + 旧 4 名薄别名（同一执行链，见
                    // `host/mcp_apps.rs`）。新方法一律按 `sessionId` 定位会话环境。
                    "peri/mcp/apps/list"
                    | "peri/mcp/apps/attach"
                    | "peri/mcp/apps/request"
                    | "peri/mcp/apps/cancel"
                    | "peri/mcp/apps/detach"
                    | "peri/mcp/apps/close"
                    | "peri/mcp/apps/rerun"
                    | "peri/mcp/open"
                    | "peri/mcp/app"
                    | "peri/mcp/resource"
                    | "peri/mcp/invoke" => {
                        self.spawn_mcp_apps_request(id, method, params).await;
                    }
                    // History V2 只读 RPC：在 initialize 之后、会话生命周期路由之前分流。
                    m if m == peri_acp_types::history_v2::HISTORY_METHOD => {
                        self.handle_history_request(id, params).await;
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

    /// `_peri/history` 分流：连接级协商与 own 适配器注入都在进入服务前完成。
    ///
    /// V1 分页/窗口路由仍在本文档的 `dispatch_request` 内保持；V2 请求的执行与
    /// 任务所有权见 [`history`]。
    async fn spawn_mcp_control(&self, id: RequestId, params: Value) {
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

    async fn spawn_session_io(&self, id: RequestId, method: String, params: Value) {
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

    async fn spawn_cancel_bg_task(&self, id: RequestId, params: Value) {
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

    async fn spawn_prompt(&self, id: RequestId, params: Value) {
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
                    super::PromptOrigin::User,
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
    async fn locate_alias_session(&self, params: &Value) -> Option<String> {
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

    async fn spawn_mcp_apps_request(&self, id: RequestId, method: String, params: Value) {
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
                                super::interaction_router::scope_host_request_connection(
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
                                let invocation = super::interaction_router::scope_host_request_connection(
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

    async fn dispatch_request(&self, id: RequestId, method: String, params: Value) {
        let diagnostics = ResponseDiagnostics::new(id, &method, &params);
        let transport = self.transport;
        let cfg = self.cfg;
        let sessions = self.sessions;
        let connection = self.connection;
        // 本连接的协商事实：一次请求取一份（短锁，不跨派发持锁），路由里按连接分流的判定
        // 只读它——`caps()` 是展示判据、注册表是进程/会话级的，都不能回答「当前连接声明了
        // 什么」（REQ §5.3）。History V2 服务与连接同生命周期，initial 的重放账本也在这里。
        let (connection_facts, connection_history_v2) = {
            let connection = connection.lock().await;
            (connection.facts(), connection.history_v2.clone())
        };
        // Child browsing is read-only even when the child has no ACP SessionState.
        // Route it before all session lifecycle and execution setup paths.
        if matches!(
            method.as_str(),
            "session/history/child" | "session/history/child/body"
        ) {
            let result = if !connection_facts.history_window_v1 {
                Err(history_window::window_error(
                    peri_acp_types::history_window::HistoryWindowError::NotNegotiated,
                ))
            } else {
                let window_connection = history_window::WindowConnection::new(
                    connection_facts.connection_id.clone(),
                    Arc::new(history_window::TransportWindowSink::new(Arc::clone(
                        self.transport,
                    ))),
                );
                tokio::select! {
                    biased;
                    _ = self.connection_cancellation.cancelled() => Err(
                        crate::transport::types::AcpError::new(-32800, "request cancelled"),
                    ),
                    result = async {
                        if method == "session/history/child/body" {
                            history_window::child::handle_body(cfg, sessions, &params,
                                &connection_facts.connection_id).await
                        } else {
                            history_window::child::handle(cfg, sessions, &connection_facts.caps,
                                &params, &window_connection).await
                        }
                    } => result,
                }
            };
            let _ = diagnostics.send(transport.as_ref(), result).await;
            return;
        }
        // History V2 initial：在生命周期装配之前解析。V2 连接缺 meta、混入旧顶层私有字段、
        // 未协商却带 initial 都在这里显式拒绝，不进入会话恢复路径。
        let history_load = match method.as_str() {
            "session/load" => match history_v2_load::parse(&params, &connection_facts) {
                Ok(Some(request)) => Some(request),
                Ok(None) => None,
                Err(error) => {
                    let _ = diagnostics.send(transport.as_ref(), Err(error)).await;
                    return;
                }
            },
            _ => None,
        };
        let v2_initial = history_load.is_some();
        // `session/history/body`：V1 正文分块，不进入生命周期路由。
        if method == "session/history/body" {
            let result = if !connection_facts.history_window_v1 {
                Err(history_window::window_error(
                    peri_acp_types::history_window::HistoryWindowError::NotNegotiated,
                ))
            } else {
                tokio::select! {
                    biased;
                    _ = self.connection_cancellation.cancelled() => Err(
                        crate::transport::types::AcpError::new(-32800, "request cancelled"),
                    ),
                    result = history_window::handle_body_chunk(
                        cfg,
                        sessions,
                        &params,
                        &connection_facts.connection_id,
                    ) => result,
                }
            };
            let _ = diagnostics.send(transport.as_ref(), result).await;
            return;
        }
        let closed_session_id = matches!(method.as_str(), "session/close" | "session/delete")
            .then(|| extract_session_id(&params, "").to_string())
            .filter(|session_id| !session_id.is_empty());
        // 历史分页：模式在进入生命周期路由**之前**解析（REQ §5.1）。带 cursor 的 load 是
        // 只读浏览——不取执行生命周期锁、不装配、不广播；非法参数与未协商能力都在这里
        // 明确拒绝，绝不降级成初始 load。
        let load_initial_cursor = if method == "session/load" {
            // 分页/窗口准入按**当前连接**的协商结果（REQ §5.3）：`caps()` 对从未
            // `initialize` 的连接是兜底能力集，不能用来放行分页或窗口语义。
            let caps = connection_facts.caps.clone();
            match history_page::classify(
                &params,
                connection_facts.history_pagination,
                connection_facts.history_window_v1,
            ) {
                Ok(history_page::SessionLoadMode::HistoryPage(request)) => {
                    let result = history_page::handle_page(cfg, sessions, &caps, &request).await;
                    let _ = diagnostics.send(transport.as_ref(), result).await;
                    return;
                }
                Ok(history_page::SessionLoadMode::HistoryWindow {
                    session_id,
                    request,
                }) => {
                    let window_connection = history_window::WindowConnection::new(
                        connection_facts.connection_id.clone(),
                        Arc::new(history_window::TransportWindowSink::new(Arc::clone(
                            self.transport,
                        ))),
                    );
                    let result = tokio::select! {
                        biased;
                        _ = self.connection_cancellation.cancelled() => Err(
                            crate::transport::types::AcpError::new(-32800, "request cancelled"),
                        ),
                        result = history_window::handle(
                            cfg, sessions, &caps, &session_id, request, &window_connection,
                        ) => result,
                    };
                    let _ = diagnostics.send(transport.as_ref(), result).await;
                    return;
                }
                Ok(history_page::SessionLoadMode::InitialLoad) => {
                    // V1 游标身份不进入 V2 initial 响应（两套协议不混用）。
                    connection_facts.history_pagination && !v2_initial
                }
                Err(error) => {
                    let _ = diagnostics.send(transport.as_ref(), Err(error)).await;
                    return;
                }
            }
        } else {
            false
        };
        if matches!(
            method.as_str(),
            "session/new" | "session/fork" | "session/load" | "session/resume"
        ) {
            let registry = connection.lock().await.acp_mcp.clone();
            let owner = if matches!(method.as_str(), "session/load" | "session/resume") {
                params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            } else {
                ""
            };
            if let Err(error) = requests::acp_mcp::validate_declarations(&registry, &params, owner)
            {
                let _ = diagnostics.send(transport.as_ref(), Err(error)).await;
                return;
            }
        }
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
            let mut close_gate = false;
            if matches!(method.as_str(), "session/close" | "session/delete") {
                // 关闭要取得生命周期锁，必须先让在飞 turn 停下来：中断面与
                // `session/cancel` 相同（本轮根令牌 + Cascade 子 agent）。等锁期间
                // 只置可逆的准入闸门 `close_in_flight`（排队中的 prompt 不得抢在
                // 关闭之前拿锁）；`closing` 与 `pre_close_session` 属于已成立的关闭
                // 事实，只允许在 `close_session` 确认持久化关闭意图之后设置。
                // 锁超时/存储失败时客户端收到关闭失败，会话必须仍可续聊（#196）。
                {
                    let mut states = sessions.lock().await;
                    if let Some(state) = states.get_mut(&lifecycle_session_id) {
                        state.close_in_flight += 1;
                        close_gate = true;
                        if let Some(token) = &state.cancel_token {
                            token.cancel();
                        }
                    }
                }
                if let Some(runtime) = cfg.session_manager.get_session(&lifecycle_session_id) {
                    runtime.cancel_cascade_children();
                }
            }
            let lock = self
                .prompt_locks
                .lock()
                .await
                .entry(lifecycle_session_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone();
            if matches!(method.as_str(), "session/close" | "session/delete") {
                let acquired =
                    peri_time::timeout(std::time::Duration::from_secs(5), lock.lock_owned())
                        .await
                        .ok();
                if close_gate {
                    // 等锁结束即撤下临时闸门：关闭成功时会话已被移除；失败时
                    // 新执行入口恢复准入，会话仍可续聊（#196）。
                    let mut states = sessions.lock().await;
                    if let Some(state) = states.get_mut(&lifecycle_session_id) {
                        state.close_in_flight = state.close_in_flight.saturating_sub(1);
                    }
                }
                match acquired {
                    Some(guard) => Some(guard),
                    None => {
                        let _ = diagnostics
                            .send(
                                transport.as_ref(),
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
                        let _ = diagnostics
                            .send(
                                transport.as_ref(),
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
        // V2 initial 的实时流：响应发送成功后才 flush 暂停期缓冲；失败则关闭未发布的流。
        let mut live_v2_initial: Option<(String, Arc<super::live_delivery::LiveDelivery>)> = None;
        let mut result = if requests::session_io::handles(&method) {
            let prepared = {
                let sessions = sessions.lock().await;
                requests::session_io::prepare(&method, &params, &sessions)
            };
            match prepared {
                Ok(prepared) => {
                    let local = prepared
                        .as_ref()
                        .map(|environment| &environment.cfg)
                        .unwrap_or(cfg);
                    requests::session_io::handle(&method, &params, local, transport).await
                }
                Err(error) => Err(error),
            }
        } else if method == "session/load" {
            // 分阶段 load：DB/资源 I/O 在全局 sessions 锁之外；内部只做短锁读写。
            let history = match history_load {
                Some(request) => {
                    let live = { self.connection.lock().await.live.clone() };
                    live_v2_initial = Some((
                        extract_session_id(&params, "").to_string(),
                        Arc::clone(&live),
                    ));
                    Some(history_v2_load::LoadInitial {
                        request,
                        service: connection_history_v2.clone(),
                        limits: history_v2_load::limits(&connection_facts),
                        live,
                    })
                }
                None => None,
            };
            requests::session_lifecycle::handle_load_shared(
                &params,
                cfg,
                sessions,
                transport,
                connection_facts.clone(),
                history,
            )
            .await
        } else if method == "session/resume" {
            let result = requests::session_lifecycle::handle_resume_shared(
                &params, cfg, sessions, transport,
            )
            .await;
            // 标准连接恢复会话后不得继承其他连接的实时投递身份。
            if result.is_ok() && !connection_facts.history_v2 {
                let session_id = extract_session_id(&params, "").to_string();
                super::live_delivery::detach_session(sessions, &session_id).await;
            }
            result
        } else if method == "session/fork" {
            requests::session_lifecycle::handle_fork_shared(&params, cfg, sessions, transport).await
        } else {
            let mut sessions = sessions.lock().await;
            handle_request_for_connection(
                &method,
                &params,
                cfg,
                &mut sessions,
                transport,
                connection_facts,
            )
            .await
        };
        drop(lifecycle_lock);
        // 初始 load 的分页身份：只对协商了分页能力的当前连接附加（REQ §5.3）。
        // 放在执行锁之外：这是初始 load 路径上的有界读取，不必占着生命周期锁。
        if load_initial_cursor && result.is_ok() {
            let session_id = extract_session_id(&params, "").to_string();
            let failure = match result.as_mut() {
                Ok(response) if !session_id.is_empty() => {
                    history_page::attach_initial_cursor(cfg, sessions, &session_id, response)
                        .await
                        .err()
                }
                _ => None,
            };
            if let Some(error) = failure {
                result = Err(error);
            }
        }
        // 进入会话时的恢复（设计 §3/§4）：装配成功后、响应发出前完成准备。
        // 生命周期锁已在上面释放；准备阶段只读范围并接纳提醒，不启动执行。
        // 装配成功 + 恢复准备失败 = 部分成功：历史保留、响应 Ok，结果放 `_meta`。
        let mut recovery_report = None;
        if result.is_ok() && super::recovery::wiring::starts_session_recovery(&method) {
            if let Some(session_id) =
                super::recovery::wiring::session_setup(&method, &params, result.as_ref().ok())
            {
                // 视图态只有历史与占用，没有执行环境：保留 load 已附带的只读阻断与
                // 恢复元数据，不进入恢复协调（否则会清掉 executionBlocked）。
                let view_only = {
                    let states = self.sessions.lock().await;
                    states
                        .get(&session_id)
                        .is_some_and(|state| state.is_view_only())
                };
                if view_only {
                    // 不启动恢复；响应与阻断已由只读视图加载写入。
                } else {
                    match super::recovery::wiring::enter_recovery(
                        self.recovery,
                        self.sessions,
                        &session_id,
                    )
                    .await
                    {
                        Ok(Some(entered)) => {
                            let view = peri_acp_types::recovery::RecoveryView::from_report(
                                &entered.report,
                            );
                            super::recovery::wiring::apply_recovery_execution_block(
                                cfg,
                                &session_id,
                                &view,
                            );
                            super::recovery::wiring::attach_recovery_meta(&mut result, &view);
                            // 冷 child 已装配但还没有运行期绑定：响应后的 launch 启动 child 前
                            // 必须补齐 TaskManager 与在线 endpoint。
                            super::recovery::wiring::bind_recovered_sessions(
                                cfg,
                                self.sessions,
                                self.connection,
                                self.transport,
                                &entered.report,
                            )
                            .await;
                            recovery_report = Some(entered);
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let view = super::recovery::wiring::error_view(&session_id, &error);
                            super::recovery::wiring::apply_recovery_execution_block(
                                cfg,
                                &session_id,
                                &view,
                            );
                            super::recovery::wiring::attach_recovery_meta(&mut result, &view);
                        }
                    }
                }
            }
        }
        // `session/new` 的进入在响应之后：新 thread 的工作意图恒为库默认的 idle，
        // 不可能有候选；放在响应前只会推迟会话响应（响应必须是首个帧）。
        let new_session_id = if method == "session/new" && result.is_ok() {
            super::recovery::wiring::session_setup(&method, &params, result.as_ref().ok())
        } else {
            None
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
            // 协商值同时落到**本连接**：进程级 pending_caps 会被下一个连接覆盖，
            // 会话级 registry 会把旧连接的能力带给重连的同一会话。
            //
            // 同输入下本调用与 `handle_initialize` 的协商结果一致，正常不会失败；
            // 若意外失败则 fail-closed：不提交任何 caps，已发送的成功响应保持不变。
            match requests::session_caps::effective_peri_caps(self.cfg, &params) {
                Ok(caps) => connection.lock().await.commit_initialize(caps),
                Err(error) => {
                    tracing::error!(
                        code = error.code,
                        error = %error,
                        "initialize 成功后重复协商失败，未提交连接级 peri caps"
                    );
                }
            }
        }
        if result.is_ok() {
            // 会话级拆除（`session/close` / `session/delete`）：撤销本会话全部 App
            // record 与交付令牌、取消 session 层令牌（已 dispatch 的调用由 invocation
            // owner 收尾）。轮次取消（`session/cancel`）**不**走这里。
            if let Some(session_id) = closed_session_id.as_deref() {
                // History V2 窗口与实时流都不跨连接；只清理本连接上该会话的状态，
                // 不影响其他会话窗口或既有会话语义。
                connection_history_v2.close_session(session_id);
                let live = self.connection.lock().await.live.clone();
                live.close(session_id);
                let environment = sessions
                    .lock()
                    .await
                    .get(session_id)
                    .and_then(|state| state.environment.clone());
                let relay = environment
                    .as_ref()
                    .and_then(|env| env.cfg.mcp_apps_relay.clone())
                    .or_else(|| cfg.mcp_apps_relay.clone());
                if let Some(relay) = relay {
                    relay.close_session(session_id);
                }
            }
        }
        let setup_session_id =
            super::recovery::wiring::session_setup(&method, &params, result.as_ref().ok());
        // 会话级 endpoint 注册（审查中 3①）：**连接归属是会话，不是某一轮**。
        // `session/new` / `session/fork` / `session/load` / `session/resume` 建立会话后
        // 立刻把当前连接登记为在线 endpoint，否则「会话建立后从未 prompt」的连接在调度
        // 触发审批时不可达（弹窗退化为「未获批准」）。`session/fork` 同样派生**新**会话
        // id（`session_setup`），漏掉它等于 fork 出来的会话没有在线 endpoint。
        // `session/restore` 不是本仓的 wire 名：恢复流程即 `session/load` +
        // `session/resume`（`requests/session_restore.rs`）。
        // 注册幂等（同一连接重复注册保留首次纪元），与 `spawn_prompt` 的注册点不冲突。
        if let Some(session_id) = setup_session_id.as_deref() {
            // `session/load|resume|fork` 走分阶段恢复路径，不经
            // `handle_request_for_connection`；这里补齐会话任务绑定，否则恢复后
            // 的 MCP 工具调用会因 "session task manager unavailable" 全部失败。
            let environment = sessions
                .lock()
                .await
                .get(session_id)
                .and_then(|state| state.environment.clone());
            let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
            requests::bind_session_tasks(session_id, local, transport);
            if matches!(
                method.as_str(),
                "session/new" | "session/fork" | "session/load" | "session/resume"
            ) {
                register_session_endpoint(
                    self.sessions,
                    session_id,
                    self.connection,
                    self.transport,
                )
                .await;
            }
        }
        let v2_initial_ok = live_v2_initial.is_some() && result.is_ok();
        let response_sent = diagnostics.send(transport.as_ref(), result).await.is_ok();
        // 首屏响应先入通道，随后才补发 load 期间缓冲的实时更新。排空按批进行：
        // 排空期间新事件继续入缓冲，因此不会越过已分配序号但尚未发送的批次。
        if let Some((session_id, live)) = live_v2_initial {
            if v2_initial_ok && response_sent {
                flush_live_updates(&live, &session_id, transport, true).await;
            } else {
                // initial 未发布：候选期更新已并回旧流，先补发；从未发布首屏的暂停流
                // 仍按既有语义关闭。
                flush_live_updates(&live, &session_id, transport, false).await;
                live.close_if_paused(&session_id);
            }
        }
        // 恢复的启动门：响应已发出才放行执行；发送失败则丢弃本次启动资格。
        match (response_sent, recovery_report.take()) {
            (true, Some(entered)) => self.recovery.launch(&entered.report).await,
            (false, Some(entered)) => self.recovery.discard(&entered),
            _ => {}
        }
        if response_sent {
            // 新建会话的进入只可能是空范围：结果不需要启动，失败只记日志。
            if let Some(session_id) = new_session_id.as_deref() {
                if let Err(error) = super::recovery::wiring::enter_recovery(
                    self.recovery,
                    self.sessions,
                    session_id,
                )
                .await
                {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %error,
                        "new session recovery entry failed"
                    );
                }
            }
            if let Some(session_id) = setup_session_id {
                let environment = sessions
                    .lock()
                    .await
                    .get(&session_id)
                    .and_then(|state| state.environment.clone());
                let local = environment.as_ref().map(|env| &env.cfg).unwrap_or(cfg);
                if method == "session/new" {
                    requests::session_lifecycle::after_new_response(local, transport, &session_id)
                        .await;
                }
                // 会话 setup 声明的 acp 型 MCP server：响应之后才受理，客户端此时
                // 已拿到会话结果，后续 `mcp/message`（只带 serverId）才有确定的归属方。
                let registry = self.connection.lock().await.acp_mcp.clone();
                requests::acp_mcp::attach_session_servers(
                    registry,
                    local,
                    transport,
                    &params,
                    &session_id,
                );
            }
        }
    }

    async fn dispatch_notification(&self, method: String, params: Value) {
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
        // NULL。失败只记 error，不阻断取消/配置更新本身（设计 §5.4 / §2.2）。
        super::notify::persist_notification_effects(
            &method,
            cfg,
            outcome.stop_targets,
            outcome.context_usage_invalidations,
        )
        .await;
    }
}

/// 按批排空 History V2 实时缓冲，直到一次空批确认排空。
///
/// `resume_paused=true` 用于 initial 成功后恢复首屏边界；`false` 只补发 initial 失败
/// 时并回旧流的候选更新，从未发布首屏的暂停流留给调用方关闭。
async fn flush_live_updates(
    live: &super::live_delivery::LiveDelivery,
    session_id: &str,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    resume_paused: bool,
) {
    loop {
        let batch = if resume_paused {
            live.resume_and_take(session_id)
        } else {
            live.drain_flushing(session_id)
        };
        if batch.is_empty() {
            return;
        }
        for payload in batch {
            if let Err(error) = transport.send_notification("session/update", payload).await {
                tracing::warn!(
                    session_id,
                    error = %error,
                    "history v2 live flush failed; closing delivery stream"
                );
                live.close(session_id);
                return;
            }
        }
    }
}
