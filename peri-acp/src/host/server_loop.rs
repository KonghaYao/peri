//! Receive loop and request-class dispatch; long-running work stays host-owned.
//!
//! 请求/通知的长任务派生分别在 `server_loop/{spawn,notifications}.rs`：收包循环
//! 只做路由与短准入，避免单文件同时承载全部执行面。

use std::sync::Arc;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::diagnostics::ResponseDiagnostics;
use super::session_endpoint::register_session_endpoint;
use super::{
    connection::ConnectionContext, extract_session_id, handle_request_for_connection, history_page,
    history_window, requests, AcpServerConfig, PromptLocks, SharedSessions,
};
use crate::transport::types::{IncomingMessage, RequestId};

mod notifications;
mod spawn;

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
                .or_insert_with(|| Arc::new(super::PromptGate::new()))
                .clone();
            if matches!(method.as_str(), "session/close" | "session/delete") {
                let acquired = peri_time::timeout(
                    std::time::Duration::from_secs(5),
                    Arc::clone(&lock.turn).lock_owned(),
                )
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
                match Arc::clone(&lock.turn).try_lock_owned() {
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
            let caps = requests::session_caps::effective_peri_caps(self.cfg, &params);
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
        let response_sent = diagnostics.send(transport.as_ref(), result).await.is_ok();
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
}
