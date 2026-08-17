//! Session 创建：new / load / resume / fork。

use std::sync::Arc;

use crate::{dispatch, session::state_builders::build_mode_state};
use agent_client_protocol::{
    schema::v1::{
        ForkSessionRequest, ForkSessionResponse, LoadSessionRequest, LoadSessionResponse,
        NewSessionRequest, NewSessionResponse, ResumeSessionRequest, ResumeSessionResponse,
        SessionId, SessionNotification,
    },
    Client, ConnectionTo, Responder,
};

use crate::dispatch::ReplaySender;

use super::super::{commands, context::StdioContext, freeze, notification};

/// 构造会话级 LSP 服务器池（new / load / resume / fork 共用，H1：跨 turn 复用；
/// 服务器进程 / initialized / 诊断状态跨 turn 存活，宿主退出时经端口优雅关闭）。
///
/// LSP pool 有子进程副作用：置 None 会走装配面临时实例路径，turn 结束后
/// 子进程与 read task 一并残留，stdio 长驻宿主下服务器进程无限累积。
/// 无 LSP 配置时返回 None（装配面不注册 LSP 中间件）。
fn session_lsp_pool(
    ctx: &StdioContext,
    cwd: &str,
) -> Option<Arc<dyn peri_acp_types::ports::LspPoolPort>> {
    peri_middlewares::assembly::create_session_lsp_pool(cwd, &ctx.cfg.plugin_lsp_servers)
}

/// 会话 MCP skill 发现预热（决策 B 扩展，stdio 装配面）：new / load / resume
/// / fork 完成时挂接连接事件 notifier + 触发幂等发现——stdio 宿主「刚进入、
/// 未说话」即有 mcp 命令（广播首发无 mcp 条目属预期，发现完成经注册表
/// on_change 重发）。与 host/requests.rs `prewarm_session_mcp_discovery`
/// 同构，双轨装配各自挂载（stdio 走 agent-client-protocol handler，不经
/// run_acp_server 的 handle_request）。任何组件缺失（pool 未装配 / registry
/// 缺失）→ 空跑返回，由首 turn 装配兜底；cancel 持 session token，会话
/// 关闭即早退。notifier 无 ExecutorEvent 通道（通知展示由首 turn 装配覆盖
/// 为完整版），连接完成事件在此即触发发现。
fn prewarm_mcp_discovery(ctx: &StdioContext, sid: &str) {
    let Some(pool) = ctx.cfg.mcp_pool.clone() else {
        return;
    };
    let Ok(pool) = pool.downcast_arc::<peri_middlewares::mcp::McpClientPool>() else {
        return;
    };
    let Some(registry) = ctx.cfg.session_manager.mcp_skill_registry_for(sid) else {
        return;
    };
    let Some(command_registry) = ctx.cfg.session_manager.command_registry_for(sid) else {
        return;
    };
    let Some(cancel) = ctx
        .cfg
        .session_manager
        .inner_sessions()
        .get(sid)
        .map(|s| s.cancel_token.clone())
    else {
        return;
    };
    peri_middlewares::mcp::middleware::attach_connection_notifier(
        &pool,
        Some(&registry),
        Some(&command_registry),
        &cancel,
        None,
    );
    peri_middlewares::mcp::middleware::prewarm_discovery(
        &pool,
        &registry,
        &command_registry,
        &cancel,
    );
}

/// session/new 处理器：创建 ThreadStore 线程、冻结系统提示词、返回模式/模型/配置选项。
pub(crate) async fn handle_new(
    ctx: &StdioContext,
    req: NewSessionRequest,
    responder: Responder<NewSessionResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let cwd_str = req.cwd.to_string_lossy().to_string();
    let meta = peri_acp_types::thread::ThreadMeta::new(&cwd_str);
    let thread_id = match ctx.cfg.thread_store.create_thread(meta).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "Thread creation failed");
            let _ = responder.respond(NewSessionResponse::new(SessionId::new("error")));
            return Ok(());
        }
    };
    let sid = thread_id.clone();
    // ── Freeze system prompt data at session creation ──
    // 通过 SessionManager 统一构造路径，并登记 AcpSession 记录以支撑
    // cascade cancel 子 agent 与 goal_state（见 SessionManager::ensure_session）。
    ctx.cfg.session_manager.ensure_session(&sid, &cwd_str);
    let frozen_data = freeze::build(ctx, &cwd_str);

    // Create session-scoped WorkflowMiddleware at session/new (GAP-05: inject frozen data)
    // 构造收拢在 host 装配面（workflow_agent 薄壳，p1-wa：执行体在
    // peri-agent，装配经 workflow_middleware_factory 端口），此处只持端口句柄
    // （3.0 批 2 波 2 装配边界收口）。
    let workflow_middleware = crate::host::workflow_agent::create_session_workflow_middleware(
        Arc::clone(&ctx.cfg.provider),
        &ctx.cfg.peri_config,
        &cwd_str,
        &sid,
        &frozen_data,
        Arc::clone(&ctx.cfg.workflow_middleware_factory),
        // session 级路径与迁移前一致，不启用事件发布（workflow 事件仅由
        // 内部 handler 消费：usage/progress）；统一发射接线留待单独裁定。
        None,
        Arc::clone(&ctx.cfg.skills),
    );
    // Create session-scoped LspServerPool at session/new（H1：跨 turn 复用；
    // load/resume/fork 分支同样创建会话级 pool——LSP pool 有子进程副作用，
    // 置 None 走临时实例路径会导致服务器子进程跨 turn 泄漏）
    let lsp_pool = session_lsp_pool(ctx, &cwd_str);

    {
        let mut sessions = ctx.sessions.write();
        sessions.insert(
            sid.clone(),
            crate::host::SessionState {
                session_id: sid.clone(),
                thread_id: thread_id.clone(),
                cwd: cwd_str,
                history: Vec::new(),
                cancel_token: None,
                frozen: Some(frozen_data),
                recall_items: Vec::new(),
                agent_pool: crate::session::agent_pool::AgentPool::new(),
                workflow_middleware,
                lsp_pool,
                title: None,
                tags: Vec::new(),
                continuation_armed: false,
                continuation_epoch: 0,
                continuation_in_flight: false,
                // 会话创建方即 writer（§6：不建立 lease 会导致后续 cancel
                // 被通知侧静默忽略）。
                lease: crate::host::lease::WriterLease::acquired("default"),
            },
        );
    }
    // 将 initialize 时暂存的 peri caps 关联到新 session
    let peri_caps = ctx.cfg.session_manager.consume_pending_caps(&sid);
    tracing::info!(session_id = %sid, "ACP session created with ThreadStore");
    let modes = build_mode_state(&ctx.cfg.permission_mode);
    let config_options = {
        let c = ctx.cfg.peri_config.read();
        let p = ctx.cfg.provider.read();
        dispatch::config_update::make_config_options(&c, &p, ctx.cfg.permission_mode.load())
    };
    let _ = responder.respond(
        NewSessionResponse::new(SessionId::new(&*sid))
            .modes(modes)
            .config_options(config_options),
    );
    // Push AvailableCommandsUpdate notification（Phase 6 A4：投影 = 注册表
    // snapshot，本地 skills / ui / 插件条目已在会话创建时注册）
    commands::send_available_commands(
        &SessionId::new(&*sid),
        &cx,
        &peri_caps,
        ctx.cfg.session_manager.command_registry_for(&sid),
    );
    // 会话预热 MCP skill 发现（决策 B 扩展）：stdio「刚进入、未说话」即有
    // mcp 命令，无需等首 turn before_agent 装配。幂等（Started 去重）。
    prewarm_mcp_discovery(ctx, &sid);
    Ok(())
}

/// session/load 处理器：从 ThreadStore 加载历史、冻结数据、构建响应。
pub(crate) async fn handle_load(
    ctx: &StdioContext,
    req: LoadSessionRequest,
    responder: Responder<LoadSessionResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let sid = req.session_id.0.to_string();
    let cwd = req.cwd.to_string_lossy().to_string();

    // 登记到 SessionManager 以支撑 cascade cancel / goal_state
    ctx.cfg.session_manager.ensure_session(&sid, &cwd);
    // 确保 caps 已在 registry 中注册
    let caps = ctx.cfg.session_manager.ensure_session_caps(&sid);
    // Build frozen data for session
    let frozen_data = freeze::build(ctx, &cwd);

    // Load history from ThreadStore via dispatch function（经 Controller 通道）
    let history = dispatch::load_session_messages(ctx.cfg.controller.as_ref(), &sid).await;

    // ── ACP v1 spec: replay history via session/update BEFORE responding ──
    let replay_sender = StdioReplaySender { cx: cx.clone() };
    if let Err(e) = dispatch::replay_session_history(&sid, &history, &replay_sender, &caps).await {
        tracing::warn!(session_id = %sid, error = %e, "session/load: history replay failed, continuing");
    }

    // Insert into sessions if not already present
    {
        let mut sessions = ctx.sessions.write();
        if let Some(s) = sessions.get_mut(&sid) {
            if s.history.is_empty() {
                s.history = history;
            }
        } else {
            // 与 session/new 一致创建会话级 LSP 池（H1：跨 turn 复用，避免
            // 临时实例路径下服务器子进程跨 turn 泄漏）
            let lsp_pool = session_lsp_pool(ctx, &cwd);
            sessions.insert(
                sid.clone(),
                crate::host::SessionState {
                    session_id: sid.clone(),
                    thread_id: sid.clone(),
                    cwd,
                    history,
                    cancel_token: None,
                    frozen: Some(frozen_data),
                    recall_items: Vec::new(),
                    agent_pool: crate::session::agent_pool::AgentPool::new(),
                    workflow_middleware: None,
                    lsp_pool,
                    title: None,
                    tags: Vec::new(),
                    continuation_armed: false,
                    continuation_epoch: 0,
                    continuation_in_flight: false,
                    // 会话创建方即 writer（§6：不建立 lease 会导致后续 cancel
                    // 被通知侧静默忽略）。
                    lease: crate::host::lease::WriterLease::acquired("default"),
                },
            );
        }
    }

    // Send config options via session/update notification (for async update)
    notification::send_config_update(ctx, &SessionId::new(&*sid), &cx);

    let modes = build_mode_state(&ctx.cfg.permission_mode);
    let config_options = {
        let c = ctx.cfg.peri_config.read();
        let p = ctx.cfg.provider.read();
        dispatch::config_update::make_config_options(&c, &p, ctx.cfg.permission_mode.load())
    };
    let _ = responder.respond(
        LoadSessionResponse::new()
            .modes(modes)
            .config_options(config_options),
    );

    // Push AvailableCommandsUpdate notification（Phase 6 A4：投影 = 注册表
    // snapshot，本地 skills / ui / 插件条目已在会话创建时注册）
    commands::send_available_commands(
        &SessionId::new(&*sid),
        &cx,
        &caps,
        ctx.cfg.session_manager.command_registry_for(&sid),
    );
    // 会话预热 MCP skill 发现（决策 B 扩展）：与 handle_new 同构，恢复会话
    // 后无需等首 turn 装配即有 mcp 命令。
    prewarm_mcp_discovery(ctx, &sid);
    Ok(())
}

/// session/resume 处理器：按需注入新的冻结数据到已有或新会话。
pub(crate) async fn handle_resume(
    ctx: &StdioContext,
    req: ResumeSessionRequest,
    responder: Responder<ResumeSessionResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let sid = req.session_id.0.to_string();
    let cwd = req.cwd.to_string_lossy().to_string();
    // 登记到 SessionManager 以支撑 cascade cancel / goal_state
    ctx.cfg.session_manager.ensure_session(&sid, &cwd);
    // 确保 caps 已在 registry 中注册
    let caps = ctx.cfg.session_manager.ensure_session_caps(&sid);
    // Build frozen data for session
    let frozen_data = freeze::build(ctx, &cwd);

    // Load history from ThreadStore (deferred load — emit view-commit if any)
    let history = dispatch::load_session_messages(ctx.cfg.controller.as_ref(), &sid).await;

    {
        let mut sessions = ctx.sessions.write();
        if !sessions.contains_key(&sid) {
            // 与 session/new 一致创建会话级 LSP 池（H1：跨 turn 复用，避免
            // 临时实例路径下服务器子进程跨 turn 泄漏）
            let lsp_pool = session_lsp_pool(ctx, &cwd);
            sessions.insert(
                sid.clone(),
                crate::host::SessionState {
                    session_id: sid.clone(),
                    thread_id: sid.clone(),
                    cwd,
                    history,
                    cancel_token: None,
                    frozen: Some(frozen_data),
                    recall_items: Vec::new(),
                    agent_pool: crate::session::agent_pool::AgentPool::new(),
                    workflow_middleware: None,
                    lsp_pool,
                    title: None,
                    tags: Vec::new(),
                    continuation_armed: false,
                    continuation_epoch: 0,
                    continuation_in_flight: false,
                    // 会话创建方即 writer（§6：不建立 lease 会导致后续 cancel
                    // 被通知侧静默忽略）。
                    lease: crate::host::lease::WriterLease::acquired("default"),
                },
            );
            tracing::info!(session_id = %sid, "Session resumed (new)");
        } else {
            // Existing session: if history is empty, populate from ThreadStore
            if let Some(s) = sessions.get_mut(&sid) {
                if s.history.is_empty() {
                    s.history = history;
                }
            }
            tracing::info!(session_id = %sid, "Session resumed (existing)");
        }
    }

    let _ = responder.respond(ResumeSessionResponse::new());
    // Push AvailableCommandsUpdate notification（Phase 6 A4，P2-3）：resume
    // 可能在新连接上恢复会话（连接已重建），需重广播投影 + 重挂注册表
    // on_change（与 handle_load 同构），否则后续 MCP 发现/断连的投影更新
    // 发往旧连接快照。
    commands::send_available_commands(
        &SessionId::new(&*sid),
        &cx,
        &caps,
        ctx.cfg.session_manager.command_registry_for(&sid),
    );
    // 会话预热 MCP skill 发现（决策 B 扩展）：与 handle_new 同构，新连接
    // 恢复会话后无需等首 turn 装配即有 mcp 命令。
    prewarm_mcp_discovery(ctx, &sid);
    Ok(())
}

/// session/fork 处理器：从源会话复制历史到新 ThreadStore 线程。
pub(crate) async fn handle_fork(
    ctx: &StdioContext,
    req: ForkSessionRequest,
    responder: Responder<ForkSessionResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let source_id = req.session_id.0.to_string();
    let cwd_str = req.cwd.to_string_lossy().to_string();

    // Get source history
    let source_history = {
        let sessions = ctx.sessions.read();
        sessions
            .get(&source_id)
            .map(|s| s.history.clone())
            .ok_or_else(|| String::from("source session not found"))
    };
    let source_history = match source_history {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(session_id = %source_id, error = %e, "session/fork: source session not found");
            let _ = responder.respond(ForkSessionResponse::new(SessionId::new("error")));
            return Ok(());
        }
    };

    if source_history.is_empty() {
        let _ = responder.respond(ForkSessionResponse::new(SessionId::new("error")));
        return Ok(());
    }

    // Fork via dispatch function（经 Controller 通道）
    let (new_thread_id, copied_history) = match dispatch::fork_session(
        ctx.cfg.controller.as_ref(),
        &source_id,
        &source_history,
        &cwd_str,
    )
    .await
    {
        Ok((id, msgs)) => (id, msgs),
        Err(e) => {
            tracing::error!(error = %e, "session/fork: fork failed");
            let _ = responder.respond(ForkSessionResponse::new(SessionId::new("error")));
            return Ok(());
        }
    };

    // Insert new session
    let new_session_id = new_thread_id.clone();
    // 登记到 SessionManager 以支撑 cascade cancel / goal_state
    ctx.cfg
        .session_manager
        .ensure_session(&new_session_id, &cwd_str);
    // 确保 caps 已在 registry 中注册
    let caps = ctx.cfg.session_manager.ensure_session_caps(&new_session_id);
    // Build frozen data for forked session
    let frozen_data = freeze::build(ctx, &cwd_str);
    // 与 session/new 一致创建会话级 LSP 池（H1：跨 turn 复用，避免
    // 临时实例路径下服务器子进程跨 turn 泄漏）
    let lsp_pool = session_lsp_pool(ctx, &cwd_str);
    {
        let mut sessions = ctx.sessions.write();
        sessions.insert(
            new_session_id.clone(),
            crate::host::SessionState {
                session_id: new_session_id.clone(),
                thread_id: new_thread_id.clone(),
                cwd: cwd_str,
                history: copied_history,
                cancel_token: None,
                frozen: Some(frozen_data),
                recall_items: Vec::new(),
                agent_pool: crate::session::agent_pool::AgentPool::new(),
                workflow_middleware: None,
                lsp_pool,
                title: None,
                tags: Vec::new(),
                continuation_armed: false,
                continuation_epoch: 0,
                continuation_in_flight: false,
                // 会话创建方即 writer（§6：不建立 lease 会导致后续 cancel
                // 被通知侧静默忽略）。
                lease: crate::host::lease::WriterLease::acquired("default"),
            },
        );
    }

    let resp = ForkSessionResponse::new(SessionId::new(new_session_id.clone()));
    let _ = responder.respond(resp);
    // Push AvailableCommandsUpdate notification（Phase 6 A4，P2-3）：fork
    // 产生新 session（新 ThreadStore 线程），需广播投影 + 挂注册表
    // on_change（与 handle_new 同构），否则新会话收不到动态条目更新。
    commands::send_available_commands(
        &SessionId::new(&*new_session_id),
        &cx,
        &caps,
        ctx.cfg
            .session_manager
            .command_registry_for(&new_session_id),
    );
    // 会话预热 MCP skill 发现（决策 B 扩展）：fork 产生新 session，与
    // handle_new 同构，无需等首 turn 装配即有 mcp 命令。
    prewarm_mcp_discovery(ctx, &new_session_id);
    Ok(())
}

/// Adapts `ConnectionTo<Client>` into a `ReplaySender` for the stdio path.
struct StdioReplaySender {
    cx: ConnectionTo<Client>,
}

#[async_trait::async_trait]
impl ReplaySender for StdioReplaySender {
    async fn send(&self, notif: SessionNotification) -> Result<(), crate::dispatch::ReplayError> {
        self.cx
            .send_notification(notif)
            .map_err(|e| crate::dispatch::ReplayError::SendFailed(e.to_string()))
    }
}

#[cfg(test)]
#[path = "create_test.rs"]
mod tests;
