//! Receive loop and request-class dispatch; long-running work stays host-owned.

use std::sync::Arc;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::diagnostics::ResponseDiagnostics;
use super::{
    connection::ConnectionContext, dispatch_prompt_turn, extract_session_id, handle_notification,
    handle_request_for_connection, history_page, history_window, mcp_apps, requests,
    send_session_info_update, task_scope, AcpServerConfig, PromptLocks, SharedSessions,
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
                    "session/input/snapshot" => {
                        self.spawn_session_io(id, method, params).await;
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
        // 什么」（REQ §5.3）。
        let connection_facts = connection.lock().await.facts();
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
                    connection_facts.history_pagination
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
            if matches!(method.as_str(), "session/close" | "session/delete") {
                if let Some(state) = sessions.lock().await.get_mut(&lifecycle_session_id) {
                    state.closing = true;

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
                match peri_time::timeout(std::time::Duration::from_secs(5), lock.lock_owned()).await
                {
                    Ok(guard) => Some(guard),
                    Err(_) => {
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
            requests::session_lifecycle::handle_load_shared(
                &params,
                cfg,
                sessions,
                transport,
                connection_facts.clone(),
            )
            .await
        } else if method == "session/resume" {
            requests::session_lifecycle::handle_resume_shared(&params, cfg, sessions, transport)
                .await
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
            let caps = requests::session_lifecycle::initialize_peri_caps(&params);
            connection.lock().await.commit_initialize(caps);
        }
        if result.is_ok() {
            // 会话级拆除（`session/close` / `session/delete`）：撤销本会话全部 App
            // record 与交付令牌、取消 session 层令牌（已 dispatch 的调用由 invocation
            // owner 收尾）。轮次取消（`session/cancel`）**不**走这里。
            if let Some(session_id) = closed_session_id.as_deref() {
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
        let setup_session_id = session_setup(&method, &params, result.as_ref().ok());
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
        let response_sent = diagnostics.send(transport.as_ref(), result).await.is_ok();
        if response_sent {
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
        let cont_req = {
            let mut sessions = sessions.lock().await;
            handle_notification(&method, &params, &mut sessions, cfg)
        };
        if let Some(req) = cont_req {
            if cont_tx.send(req).is_err() {
                tracing::warn!(
                    method,
                    session_id = extract_session_id(&params, ""),
                    "ACP continuation scheduling failed: channel closed"
                );
            }
        }
    }
}

/// 把当前连接注册为本会话的交互 endpoint（幂等；表满/离线时记 warn 并继续——
/// 该连接的审批随后按无在线 endpoint 处理，是 fail-closed 而不是静默降级）。
pub(super) async fn register_session_endpoint(
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

/// 会话 setup 方法涉及的会话 id：`session/new` | `session/fork` 取响应里的新 id，
/// `session/load` | `session/resume` 取请求里的 `sessionId`；非 setup 方法与失败的
/// 调用返回 `None`（失败响应不做后置初始化）。
fn session_setup(method: &str, params: &Value, response: Option<&Value>) -> Option<String> {
    let response = response?;
    match method {
        "session/new" | "session/fork" => response
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        "session/load" | "session/resume" => {
            let session_id = extract_session_id(params, "");
            (!session_id.is_empty()).then(|| session_id.to_owned())
        }
        _ => None,
    }
}
