//! Session 生命周期命令 handler：initialize / new / load / list /
//! cancel-bg-task / close / delete / resume / fork / rename（自 requests.rs
//! 拆出，请求分发见 `host/requests.rs`）。

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    CloseSessionResponse, DeleteSessionResponse, ForkSessionResponse, ListSessionsResponse,
    LoadSessionResponse, NewSessionResponse, ResumeSessionResponse, SessionId, SessionNotification,
};
use peri_acp_types::ports::WorkflowMiddlewarePort;
use peri_acp_types::thread::ThreadMeta;
use peri_acp_types::PeriCaps;
use serde_json::Value;
use tracing::{info, warn};

use super::super::notify::{send_available_commands_update, send_config_option_update};
use super::super::{build_mode_state, AcpServerConfig, SessionState};
use crate::dispatch::config_update::make_config_options;
use crate::dispatch::ReplaySender;
use crate::{dispatch, transport::types::AcpError};

/// 创建 session 级 WorkflowMiddleware（session/new / load / resume 共用，GAP-05）。
///
/// 构造收拢在 host 装配面（`host/workflow_agent.rs` 薄壳：executor 注入面 +
/// 端口装配），命令面只持 `Arc<dyn WorkflowMiddlewarePort>`（3.0 批 2
/// 波 2 装配边界收口；p1-wa：执行体在 peri-agent，装配经
/// `workflow_middleware_factory` 端口）。
fn create_session_workflow_middleware(
    cfg: &AcpServerConfig,
    cwd: &str,
    session_id: &str,
    frozen_data: &crate::session::executor::FrozenSessionData,
) -> Option<Arc<dyn WorkflowMiddlewarePort>> {
    crate::host::workflow_agent::create_session_workflow_middleware(
        Arc::clone(&cfg.provider),
        &cfg.peri_config,
        cwd,
        session_id,
        frozen_data,
        Arc::clone(&cfg.workflow_middleware_factory),
        // session 级路径与迁移前一致，不启用事件发布（workflow 事件仅由
        // 内部 handler 消费：usage/progress）；统一发射接线留待单独裁定。
        None,
        Arc::clone(&cfg.skills),
    )
}

/// 创建 session 级 LSP 服务器池（session/new / load / resume / fork 共用，H1）。
///
/// 会话级实例跨 turn 复用（服务器进程 / initialized / 诊断状态不丢），
/// 宿主退出（`run_acp_server` 返回）时经端口 `shutdown` 优雅关闭。
/// 无 LSP 配置时返回 None（不注册 LSP 中间件）。
fn create_session_lsp_pool(
    cfg: &AcpServerConfig,
    cwd: &str,
) -> Option<Arc<dyn peri_acp_types::ports::LspPoolPort>> {
    peri_middlewares::assembly::create_session_lsp_pool(cwd, &cfg.plugin_lsp_servers)
}

pub(crate) fn handle_initialize(params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    let version = params
        .get("protocolVersion")
        .and_then(|v| v.as_u64())
        .unwrap_or(1);
    info!(protocol_version = %version, "ACP initialize");

    // 解析 clientCapabilities._meta 中的 peri 自定义 flag
    let peri_caps = params
        .get("clientCapabilities")
        .and_then(|c| c.get("_meta"))
        .and_then(|m| m.as_object())
        .map(PeriCaps::from_client_meta)
        .unwrap_or_default();

    // 暂存 caps，session/new 时 consume
    cfg.session_manager.set_pending_caps(peri_caps.clone());

    let resp = dispatch::build_initialize_response(&peri_caps);
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

pub(crate) async fn handle_new(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let cwd = params
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or(".")
        .to_string();
    let meta = ThreadMeta::new(&cwd);
    let thread_id = cfg
        .thread_store
        .create_thread(meta)
        .await
        .map_err(|e| AcpError::new(-32603, format!("Thread creation failed: {e}")))?;
    let session_id = thread_id.clone();

    // ── Freeze system prompt data at session creation ──
    // 通过 SessionManager 统一构造路径，并登记 AcpSession 记录以支撑
    // cascade cancel 子 agent 与 goal_state（见 SessionManager::ensure_session）。
    // GAP-05: frozen data 在 WorkflowMiddleware 创建前构建，注入到 executor。
    cfg.session_manager.ensure_session(&session_id, &cwd);
    let frozen_data = cfg.session_manager.build_frozen_data(
        &cwd,
        &cfg.plugin_skill_roots,
        &cfg.plugin_agent_dirs,
    );

    // Create session-scoped WorkflowMiddleware at session/new (GAP-05: inject frozen data)
    let workflow_middleware =
        create_session_workflow_middleware(cfg, &cwd, &session_id, &frozen_data);
    // Create session-scoped LspServerPool at session/new（H1：跨 turn 复用）
    let lsp_pool = create_session_lsp_pool(cfg, &cwd);

    sessions.insert(
        session_id.clone(),
        SessionState {
            session_id: session_id.clone(),
            thread_id: thread_id.clone(),
            cwd: cwd.clone(),
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
            lease: super::super::lease::WriterLease::acquired("default"),
        },
    );

    info!(session_id = %session_id, "ACP session created with ThreadStore");
    let modes = build_mode_state(&cfg.permission_mode);
    let config_options = {
        let c = cfg.peri_config.read();
        let p = cfg.provider.read();
        make_config_options(&c, &p, cfg.permission_mode.load())
    };
    let resp = NewSessionResponse::new(SessionId::new(&*session_id))
        .modes(modes)
        .config_options(config_options);
    // 将暂存的 peri caps 关联到新 session（MpscTransport 路径：若未
    // 显式调用 initialize（TUI 内部连接），默认全部 cap=true）。首次
    // AvailableCommandsUpdate 必须由 host 在 session/new response 成功发送后
    // 推送，确保客户端已能按 response 中的 sessionId 建立通知路由。
    cfg.session_manager.ensure_session_caps(&session_id);

    // BRIDGE_RESET_COUNTER handles stale committed cleanup; no explicit clear needed
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

/// `session/new` response 成功写入 transport 后执行的初始化通知。
///
/// commands 首发与 MCP 预热必须保持此顺序：先挂载命令注册表的 on_change
/// 回调并发送 snapshot，再启动 MCP 发现，避免发现结果抢在首次 snapshot 前推送。
pub(crate) async fn after_new_response(
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    session_id: &str,
) {
    let peri_caps = cfg.session_manager.ensure_session_caps(session_id);
    send_available_commands_update(
        transport,
        session_id,
        &peri_caps,
        cfg.session_manager.command_registry_for(session_id),
        cfg.stdio_command_filter,
    )
    .await;
    prewarm_session_mcp_discovery(cfg, session_id);
}

pub(crate) async fn handle_load(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let req_session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let cwd = params.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");

    // Load history from ThreadStore via Controller
    let history = dispatch::load_session_messages(cfg.controller.as_ref(), req_session_id).await;

    // ── 先构建 frozen + workflow_middleware，再插入 session ──
    cfg.session_manager.ensure_session(req_session_id, cwd);
    let caps = cfg.session_manager.ensure_session_caps(req_session_id);
    let frozen_data =
        cfg.session_manager
            .build_frozen_data(cwd, &cfg.plugin_skill_roots, &cfg.plugin_agent_dirs);
    let workflow_middleware =
        create_session_workflow_middleware(cfg, cwd, req_session_id, &frozen_data);
    let lsp_pool = create_session_lsp_pool(cfg, cwd);

    // Insert into sessions if not already present
    if let Some(state) = sessions.get_mut(req_session_id) {
        if state.history.is_empty() {
            state.history = history;
        }
        if state.frozen.is_none() {
            state.frozen = Some(frozen_data);
        }
        if state.workflow_middleware.is_none() {
            state.workflow_middleware = workflow_middleware;
        }
        if state.lsp_pool.is_none() {
            state.lsp_pool = lsp_pool;
        }
    } else {
        sessions.insert(
            req_session_id.to_string(),
            SessionState {
                session_id: req_session_id.to_string(),
                thread_id: req_session_id.to_string(),
                cwd: cwd.to_string(),
                history,
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
                lease: super::super::lease::WriterLease::acquired("default"),
            },
        );
    }

    // ── ACP v1 spec: replay history via session/update BEFORE responding ──
    let history_for_replay: Vec<_> = sessions
        .get(req_session_id)
        .map(|s| s.history.clone())
        .unwrap_or_default();
    let replay_sender = TuiReplaySender {
        transport: transport.as_ref(),
    };
    if let Err(e) =
        dispatch::replay_session_history(req_session_id, &history_for_replay, &replay_sender, &caps)
            .await
    {
        tracing::warn!(session_id = %req_session_id, error = %e, "session/load: history replay failed, continuing");
    }

    // modes/configOptions sent both via notification AND in response body
    // (notification for async update, response body for immediate availability)
    send_config_option_update(transport.as_ref(), req_session_id, cfg).await;

    let modes = build_mode_state(&cfg.permission_mode);
    let config_options = {
        let c = cfg.peri_config.read();
        let p = cfg.provider.read();
        make_config_options(&c, &p, cfg.permission_mode.load())
    };
    let resp = LoadSessionResponse::new()
        .modes(modes)
        .config_options(config_options);
    // Push AvailableCommandsUpdate notification（Phase 6 A4：投影 =
    // 注册表 snapshot；本地 skills / ui / 插件条目已在会话创建时注册）
    send_available_commands_update(
        transport,
        req_session_id,
        &caps,
        cfg.session_manager.command_registry_for(req_session_id),
        cfg.stdio_command_filter,
    )
    .await;
    // 与 session/new 同构（决策 B 扩展）：恢复会话同样预热 MCP skill
    // 发现——stdio 宿主 session/load 后无需等首 turn before_agent
    // 装配即有 mcp 命令（广播首发无 mcp 条目属预期，发现完成经注册表
    // on_change 重发）。幂等（Started 去重）；pool/registry 缺失或
    // 连接中 → 空跑，由首 turn 装配与连接完成事件兜底。
    prewarm_session_mcp_discovery(cfg, req_session_id);
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

pub(crate) async fn handle_list(params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    let cwd_filter = params.get("cwd").and_then(|v| v.as_str());
    let entries = dispatch::list_sessions_as_info(cfg.controller.as_ref(), cwd_filter)
        .await
        .map_err(|e| AcpError::new(-32603, format!("Failed to list sessions: {e}")))?;

    let resp = ListSessionsResponse::new(entries);
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

pub(super) fn handle_cancel_bg_task(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    let req_session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let task_id = params
        .get("taskId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing taskId"))?;

    // 会话不存在时如实报错（此前静默返回 success，掩盖取消未生效）
    let session = cfg
        .session_manager
        .get_session(req_session_id)
        .ok_or_else(|| AcpError::new(-32602, format!("session not found: {req_session_id}")))?;
    session
        .task_manager
        .cancel(task_id)
        .map_err(|e| AcpError::new(-32603, e.to_string()))?;
    info!(session_id = %req_session_id, task_id = %task_id, "Background task cancelled via ACP");
    Ok(serde_json::json!({ "success": true }))
}

pub(crate) async fn handle_close(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let req_session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;

    if let Some(state) = sessions.remove(req_session_id) {
        if let Some(ref token) = state.cancel_token {
            token.cancel();
        }
        info!(session_id = %req_session_id, "Session closed");
    }
    // 同步从 SessionManager 移除 AcpSession 记录（取消所有 cascade 子 agent）
    let _ = cfg.session_manager.close_session(req_session_id).await;
    let resp = CloseSessionResponse::new();
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

// session/delete（标准 ACP，agentclientprotocol.com/protocol/v1/session-delete）：
// 从 session history 中移除会话——先做与 session/close 相同的内存态清理，
// 再从 ThreadStore 持久化删除线程（消息级联删除）。存储层幂等：线程
// 不存在时不视为错误；真实 IO 失败仅记录日志（与 stdio 路径一致）。
pub(crate) async fn handle_delete(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
) -> Result<Value, AcpError> {
    let req_session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;

    // 与 stdio 路径（handle_delete）一致：锁外 shutdown LSP pool，
    // 避免删除活跃会话后 LSP 服务器子进程/read task 残留（M2）
    let lsp_pool = {
        if let Some(state) = sessions.remove(req_session_id) {
            if let Some(ref token) = state.cancel_token {
                token.cancel();
            }
            info!(session_id = %req_session_id, "Session removed on delete");
            state.lsp_pool
        } else {
            None
        }
    };
    if let Some(pool) = lsp_pool {
        pool.shutdown().await;
    }
    let _ = cfg.session_manager.close_session(req_session_id).await;
    if let Err(e) = cfg
        .thread_store
        .delete_thread(&req_session_id.to_string())
        .await
    {
        warn!(session_id = %req_session_id, error = %e, "session/delete: thread deletion failed");
    } else {
        info!(session_id = %req_session_id, "Session history deleted");
    }
    let resp = DeleteSessionResponse::new();
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

pub(crate) async fn handle_resume(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let req_session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let cwd = params.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");

    // Load history from ThreadStore via Controller (deferred load)
    let history = dispatch::load_session_messages(cfg.controller.as_ref(), req_session_id).await;

    // ── 先构建 frozen + workflow_middleware ──
    cfg.session_manager.ensure_session(req_session_id, cwd);
    let caps = cfg.session_manager.ensure_session_caps(req_session_id);
    let frozen_data =
        cfg.session_manager
            .build_frozen_data(cwd, &cfg.plugin_skill_roots, &cfg.plugin_agent_dirs);
    let workflow_middleware =
        create_session_workflow_middleware(cfg, cwd, req_session_id, &frozen_data);
    let lsp_pool = create_session_lsp_pool(cfg, cwd);

    if !sessions.contains_key(req_session_id) {
        sessions.insert(
            req_session_id.to_string(),
            SessionState {
                session_id: req_session_id.to_string(),
                thread_id: req_session_id.to_string(),
                cwd: cwd.to_string(),
                history,
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
                lease: super::super::lease::WriterLease::acquired("default"),
            },
        );
        info!(session_id = %req_session_id, "Session resumed (new)");
    } else {
        // Existing session: populate missing fields
        if let Some(s) = sessions.get_mut(req_session_id) {
            if s.history.is_empty() {
                s.history = history;
            }
            if s.frozen.is_none() {
                s.frozen = Some(frozen_data);
            }
            if s.workflow_middleware.is_none() {
                s.workflow_middleware = workflow_middleware;
            }
            if s.lsp_pool.is_none() {
                s.lsp_pool = lsp_pool;
            }
        }
        info!(session_id = %req_session_id, "Session resumed (existing)");
    }

    // Push AvailableCommandsUpdate notification + 预热 MCP skill 发现
    // （决策 B 扩展，与 session/load 同构；stdio 装配面同款行为——恢复会话
    // 后无需等首 turn before_agent 装配即有 mcp 命令）。幂等（Started 去重）；
    // pool/registry 缺失或连接中 → 空跑，由首 turn 装配兜底。
    send_available_commands_update(
        transport,
        req_session_id,
        &caps,
        cfg.session_manager.command_registry_for(req_session_id),
        cfg.stdio_command_filter,
    )
    .await;
    prewarm_session_mcp_discovery(cfg, req_session_id);

    let resp = ResumeSessionResponse::new();
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

pub(crate) async fn handle_fork(
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let source_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let cwd = params.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");

    let source_history = sessions
        .get(source_id)
        .map(|s| s.history.clone())
        .ok_or_else(|| AcpError::new(-32602, format!("source session not found: {source_id}")))?;

    let (new_thread_id, copied_history) =
        dispatch::fork_session(cfg.controller.as_ref(), source_id, &source_history, cwd)
            .await
            .map_err(|e| AcpError::new(-32603, format!("{e}")))?;

    let new_session_id = new_thread_id.clone();

    // ── 先构建 frozen + workflow_middleware ──
    cfg.session_manager.ensure_session(&new_session_id, cwd);
    let caps = cfg.session_manager.ensure_session_caps(&new_session_id);
    let frozen_data =
        cfg.session_manager
            .build_frozen_data(cwd, &cfg.plugin_skill_roots, &cfg.plugin_agent_dirs);
    let workflow_middleware =
        create_session_workflow_middleware(cfg, cwd, &new_session_id, &frozen_data);
    let lsp_pool = create_session_lsp_pool(cfg, cwd);

    sessions.insert(
        new_session_id.clone(),
        SessionState {
            session_id: new_session_id.clone(),
            thread_id: new_thread_id.clone(),
            cwd: cwd.to_string(),
            history: copied_history,
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
            lease: super::super::lease::WriterLease::acquired("default"),
        },
    );

    info!(source = %source_id, new = %new_session_id, "Session forked");
    // Push AvailableCommandsUpdate notification + 预热 MCP skill 发现
    // （决策 B 扩展，与 session/new 同构；stdio 装配面同款行为——fork 产生
    // 新 session 后无需等首 turn before_agent 装配即有 mcp 命令）。
    send_available_commands_update(
        transport,
        &new_session_id,
        &caps,
        cfg.session_manager.command_registry_for(&new_session_id),
        cfg.stdio_command_filter,
    )
    .await;
    prewarm_session_mcp_discovery(cfg, &new_session_id);
    let resp = ForkSessionResponse::new(SessionId::new(new_session_id));
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

pub(super) async fn handle_rename(
    params: &Value,
    cfg: &AcpServerConfig,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    let title = params
        .get("title")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing title"))?;

    cfg.thread_store
        .update_title(&session_id.to_string(), title)
        .await
        .map_err(|e| AcpError::new(-32603, format!("Failed to rename session: {e}")))?;

    // 通过 session/update 通知推送新的标题给外部客户端
    super::super::notify::send_session_info_update_with_title(
        transport.as_ref(),
        session_id,
        Some(title),
    )
    .await;

    info!(session_id = %session_id, title = %title, "Session renamed");

    Ok(serde_json::json!({
        "sessionId": session_id,
        "title": title,
    }))
}

/// 新会话 MCP skill 发现预热（决策 B 扩展）：session/new 完成时挂接连接
/// 事件 notifier + 触发幂等发现，chain 首 turn 装配前即可开始。任何组件
/// 缺失（pool 未装配 / registry 缺失）→ 空跑返回，由首 turn 装配兜底；
/// cancel 持 session token，会话关闭即早退。notifier 无 ExecutorEvent 通道
/// （通知展示由首 turn 装配覆盖为完整版），连接完成事件在此即触发发现。
fn prewarm_session_mcp_discovery(cfg: &AcpServerConfig, session_id: &str) {
    let Some(pool) = cfg.mcp_pool.clone() else {
        return;
    };
    let Ok(pool) = pool.downcast_arc::<peri_middlewares::mcp::McpClientPool>() else {
        return;
    };
    let Some(registry) = cfg.session_manager.mcp_skill_registry_for(session_id) else {
        return;
    };
    let Some(command_registry) = cfg.session_manager.command_registry_for(session_id) else {
        return;
    };
    let Some(cancel) = cfg
        .session_manager
        .inner_sessions()
        .get(session_id)
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

/// Adapts `&dyn AcpTransport` into a `ReplaySender` for the TUI path.
struct TuiReplaySender<'a> {
    transport: &'a dyn crate::transport::AcpTransport,
}

#[async_trait::async_trait]
impl ReplaySender for TuiReplaySender<'_> {
    async fn send(&self, notif: SessionNotification) -> Result<(), crate::dispatch::ReplayError> {
        let payload = serde_json::to_value(&notif)
            .map_err(|e| crate::dispatch::ReplayError::SendFailed(e.to_string()))?;
        self.transport
            .send_notification("session/update", payload)
            .await
            .map_err(|e| crate::dispatch::ReplayError::SendFailed(e.to_string()))
    }
}
