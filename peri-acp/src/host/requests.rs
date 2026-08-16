//! ACP Request dispatch — handles all ACP protocol request methods.
//! Extracted from original acp_server.rs (2026-05-20 split).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::dispatch::config_update::make_config_options;
use crate::dispatch::ReplaySender;
use crate::{dispatch, transport::types::AcpError};
use agent_client_protocol::schema::v1::{
    CloseSessionResponse, DeleteSessionResponse, ForkSessionResponse, ListSessionsResponse,
    LoadSessionResponse, NewSessionResponse, ResumeSessionResponse, SessionId, SessionNotification,
    SetSessionConfigOptionResponse, SetSessionModeResponse,
};
use peri_acp_types::event_data::{
    PluginActionResult, PluginSearchResult, PluginSnapshot, PluginSnapshotEntry,
};
use peri_acp_types::ports::WorkflowMiddlewarePort;
use peri_acp_types::thread::ThreadMeta;
use peri_acp_types::PeriCaps;
use serde_json::Value;
use tracing::{debug, info, warn};

use super::{
    apply_profile_effort, build_mode_state,
    notify::{extract_session_id, send_available_commands_update, send_config_option_update},
    parse_permission_mode, AcpServerConfig, SessionState,
};
use crate::provider::LlmProvider;

fn persist_config(cfg: &AcpServerConfig) {
    let c = cfg.peri_config.read();
    // 写回当前生效层：路径决策在 ConfigSource 加载时一次性确定（工作区存在则
    // 分层写回工作区，否则写全局），与读取完全对称，不存在第二套实现。
    if let Err(e) = cfg.config_source.save(&c) {
        tracing::warn!(error = %e, "Failed to persist config");
    }
}

/// Phase 6 B3：插件 install / uninstall 成功后刷新 plugin 域命令条目——
/// 注销全部旧条目 → 重载已启用插件 → 重新注册（`reconcile` 单次写锁原子
/// 完成，任一内容变化只触发**一次** `on_change` → 投影推送，不经 TUI
/// 协议）。
///
/// 重载失败 → 注销全部旧条目（plugin 域保持空：磁盘状态已变，过时条目
/// 不得残留展示）+ 日志告警，不阻塞 RPC 回包。
///
/// Phase 6 遗留登记（P2-4，跨主题确认事项）：插件 mcpServers 变更
/// （install/uninstall 改插件 manifest 的 mcpServers）**无 client 池刷新
/// 触发点**——`McpPoolPort` 仅暴露 shutdown/snapshot，池配置为装配时
/// 快照（`run_initialize` 一次性读取聚合配置含插件 mcpServers，
/// assemble.rs / stdio/init.rs），`reconnect(name)` 仅按既有配置键重连，
/// 无法接入新装插件的服务器；新装插件 `mcp:*` 命令条目依赖既有池重连
/// 机制 + A3 发现链路自愈，需下次装配/会话重启生效，未在本 Phase 触发。
fn refresh_plugin_command_entries(
    cfg: &AcpServerConfig,
    session_id: &str,
    claude_dir: &Path,
    session_cwd: Option<&str>,
) {
    let Some(command_registry) = cfg.session_manager.command_registry_for(session_id) else {
        tracing::warn!(
            session_id,
            "plugin 命令刷新：无 session 级命令注册表，跳过（RPC 回包不受影响）"
        );
        return;
    };
    // stale = 当前 plugin 域全部条目（reconcile 精确键注销，未命中静默跳过）。
    let stale: Vec<String> = command_registry
        .snapshot()
        .iter()
        .filter(|e| e.fullname.to_lowercase().starts_with("plugin:"))
        .map(|e| e.fullname.clone())
        .collect();
    // 重载：与装配面同源（`load_enabled_plugins` → all_commands 聚合）；
    // 无 session 上下文（session_cwd = None）时仅用户级 enabledPlugins。
    let fresh_commands = match peri_middlewares::plugin::load_enabled_plugins(
        claude_dir,
        session_cwd.map(Path::new),
    ) {
        Ok(plugins) => plugins
            .iter()
            .flat_map(|p| p.commands.clone())
            .collect::<Vec<_>>(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "插件重载失败：plugin 域清空（保留空 plugin 域），不阻塞 RPC 回包"
            );
            Vec::new()
        }
    };
    let (removed, added) = command_registry.reconcile(
        &stale,
        peri_middlewares::plugin::plugin_route_entries(&fresh_commands),
    );
    tracing::info!(
        session_id,
        removed,
        added,
        "插件命令条目动态刷新完成（install/uninstall 后；注册表 on_change 已触发投影推送）"
    );
}

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

pub(crate) async fn handle_request(
    method: &str,
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn crate::transport::AcpTransport>,
) -> Result<Value, AcpError> {
    match method {
        "initialize" => {
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
            serde_json::to_value(resp)
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }

        "session/new" => {
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
                    lease: super::lease::WriterLease::acquired("default"),
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
            // 显式调用 initialize（TUI 内部连接），默认全部 cap=true）。
            let peri_caps = cfg.session_manager.ensure_session_caps(&session_id);
            // Push AvailableCommandsUpdate notification（Phase 6 A4：投影 =
            // 注册表 snapshot；本地 skills / ui / 插件条目已在会话创建时注册）
            send_available_commands_update(
                transport,
                &session_id,
                &peri_caps,
                cfg.session_manager.command_registry_for(&session_id),
            )
            .await;

            // 新会话预热 MCP skill 发现（决策 B 扩展）：chain 首 turn 装配前
            // 即 spawn 发现——/clear 后面板无需等首轮消息即有 mcp 命令。
            // 幂等（Started 去重）；pool/registry 缺失或连接中 → 空跑，
            // 由首 turn 装配与连接完成事件兜底。
            prewarm_session_mcp_discovery(cfg, &session_id);

            // BRIDGE_RESET_COUNTER handles stale committed cleanup; no explicit clear needed
            serde_json::to_value(resp)
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }

        "session/set_mode" => {
            let mode_id = params
                .get("modeId")
                .and_then(|v| v.as_str())
                .unwrap_or("default");
            let session_id = extract_session_id(params, "");
            let mode = parse_permission_mode(mode_id);
            cfg.permission_mode.store(mode);
            info!(mode_id = %mode_id, "Permission mode changed");
            let resp = SetSessionModeResponse::new();
            send_config_option_update(transport.as_ref(), session_id, cfg).await;
            serde_json::to_value(resp)
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }

        "session/set_config_option" => {
            let config_id = params
                .get("configId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let session_id = extract_session_id(params, "");
            let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
            match config_id {
                "mode" => {
                    let mode = parse_permission_mode(value);
                    cfg.permission_mode.store(mode);
                    info!(mode = %value, "Permission mode changed via configOption");
                }
                "model" => {
                    {
                        let mut c = cfg.peri_config.write();
                        c.config.active_alias = value.to_string();
                    }
                    let new_provider = {
                        let c = cfg.peri_config.read();
                        LlmProvider::from_config_for_alias(&c, value)
                    };
                    if let Some(new_provider) = new_provider {
                        info!(model_id = %value, model = %new_provider.model_name(), "Model changed via configOption");
                        *cfg.provider.write() = new_provider;
                    }
                    // Model switch → invalidate cached LLM instances
                    if let Some(s) = sessions.get_mut(session_id) {
                        s.agent_pool.invalidate();
                    }
                    persist_config(cfg);
                }
                "thinking_effort" => {
                    apply_profile_effort(&cfg.peri_config, value);
                    // 同步更新 LlmProvider（thinking 变更需要重建 provider）
                    let new_provider = {
                        let c = cfg.peri_config.read();
                        LlmProvider::from_config(&c)
                    };
                    if let Some(new_provider) = new_provider {
                        *cfg.provider.write() = new_provider;
                    }
                    // Thinking 变更 → invalidate cached LLM 实例
                    if let Some(s) = sessions.get_mut(session_id) {
                        s.agent_pool.invalidate();
                    }
                    persist_config(cfg);
                    info!(effort = %value, "Thinking effort changed via configOption");
                }
                "context_1m" => {
                    let enabled = value == "true" || value == "1";
                    let mut updated = false;
                    {
                        let mut c = cfg.peri_config.write();
                        let alias = c.config.active_alias.clone();
                        if let Some(profile) = c.config.profiles.get_mut(&alias) {
                            profile.context_1m = enabled;
                            updated = true;
                        }
                    }
                    if updated {
                        persist_config(cfg);
                        info!(enabled = %enabled, "Context 1M changed via configOption (persisted)");
                    } else {
                        warn!(enabled = %enabled, "Context 1M configOption skipped: active profile not found");
                    }
                }
                _ => {
                    debug!(config_id = %config_id, "Unknown config option");
                }
            }
            let config_options = {
                let c = cfg.peri_config.read();
                let p = cfg.provider.read();
                make_config_options(&c, &p, cfg.permission_mode.load())
            };
            let resp = SetSessionConfigOptionResponse::new(config_options);
            send_config_option_update(transport.as_ref(), session_id, cfg).await;
            serde_json::to_value(resp)
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }

        "session/load" => {
            let req_session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
            let cwd = params.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");

            // Load history from ThreadStore via Controller
            let history =
                dispatch::load_session_messages(cfg.controller.as_ref(), req_session_id).await;

            // ── 先构建 frozen + workflow_middleware，再插入 session ──
            cfg.session_manager.ensure_session(req_session_id, cwd);
            let caps = cfg.session_manager.ensure_session_caps(req_session_id);
            let frozen_data = cfg.session_manager.build_frozen_data(
                cwd,
                &cfg.plugin_skill_roots,
                &cfg.plugin_agent_dirs,
            );
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
                        lease: super::lease::WriterLease::acquired("default"),
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
            if let Err(e) = dispatch::replay_session_history(
                req_session_id,
                &history_for_replay,
                &replay_sender,
                &caps,
            )
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
            )
            .await;
            serde_json::to_value(resp)
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }

        "session/list" => {
            let cwd_filter = params.get("cwd").and_then(|v| v.as_str());
            let entries = dispatch::list_sessions_as_info(cfg.controller.as_ref(), cwd_filter)
                .await
                .map_err(|e| AcpError::new(-32603, format!("Failed to list sessions: {e}")))?;

            let resp = ListSessionsResponse::new(entries);
            serde_json::to_value(resp)
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }

        "workflow/list_runs" => {
            let req_session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;

            let runs = sessions
                .get(req_session_id)
                .and_then(|s| s.workflow_middleware.as_ref())
                .map(|mw| mw.runs_snapshot())
                .unwrap_or_default();

            let resp = serde_json::json!({ "runs": runs });
            Ok(resp)
        }

        "workflow/kill_agent" => {
            // 显式按请求 sessionId 查找（与 workflow/list_runs 一致），
            // 多 session 时不得取第一个带 middleware 的 session（issue 2026-08-05）
            let req_session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
            let run_id = params
                .get("runId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing runId"))?;
            let agent_id = params
                .get("agentId")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| AcpError::new(-32602, "missing agentId"))?;

            let mw = sessions
                .get(req_session_id)
                .and_then(|s| s.workflow_middleware.as_ref())
                .ok_or_else(|| {
                    AcpError::new(-32602, format!("session not found: {req_session_id}"))
                })?;
            let killed = mw.kill_agent(run_id, agent_id).await;

            if killed {
                info!(run_id, agent_id, "Workflow agent killed via ACP");
            } else {
                warn!(run_id, agent_id, "Workflow agent kill failed (not found)");
            }
            Ok(serde_json::json!({ "killed": killed }))
        }

        "workflow/kill_run" => {
            // 显式按请求 sessionId 查找（与 workflow/list_runs 一致），
            // 多 session 时不得取第一个带 middleware 的 session（issue 2026-08-05）
            let req_session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
            let run_id = params
                .get("runId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing runId"))?;

            let mw = sessions
                .get(req_session_id)
                .and_then(|s| s.workflow_middleware.as_ref())
                .ok_or_else(|| {
                    AcpError::new(-32602, format!("session not found: {req_session_id}"))
                })?;
            let killed = mw.kill_run(run_id);

            if killed {
                info!(run_id, "Workflow run killed via ACP");
            } else {
                warn!(run_id, "Workflow run kill failed (not found)");
            }
            Ok(serde_json::json!({ "killed": killed }))
        }

        "workflow/resume" => {
            // 显式按请求 sessionId 查找（与 workflow/list_runs、kill_run 一致），
            // 多 session 时不得取第一个带 middleware 的 session（issue 2026-08-05）
            let req_session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
            let run_id = params
                .get("runId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing runId"))?;

            let mw = sessions
                .get(req_session_id)
                .and_then(|s| s.workflow_middleware.as_ref())
                .ok_or_else(|| {
                    AcpError::new(-32602, format!("session not found: {req_session_id}"))
                })?;

            let new_run_id = mw
                .resume(run_id)
                .await
                .map_err(|e| AcpError::new(-32603, e))?;

            info!(old_run = %run_id, new_run = %new_run_id, "Workflow resumed via ACP");
            Ok(serde_json::json!({
                "runId": new_run_id,
                "resumedFrom": run_id
            }))
        }

        "session/cancel-bg-task" => {
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
                .ok_or_else(|| {
                    AcpError::new(-32602, format!("session not found: {req_session_id}"))
                })?;
            session
                .task_manager
                .cancel(task_id)
                .map_err(|e| AcpError::new(-32603, e.to_string()))?;
            info!(session_id = %req_session_id, task_id = %task_id, "Background task cancelled via ACP");
            Ok(serde_json::json!({ "success": true }))
        }

        "session/close" => {
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
            serde_json::to_value(resp)
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }

        // session/delete（标准 ACP，agentclientprotocol.com/protocol/v1/session-delete）：
        // 从 session history 中移除会话——先做与 session/close 相同的内存态清理，
        // 再从 ThreadStore 持久化删除线程（消息级联删除）。存储层幂等：线程
        // 不存在时不视为错误；真实 IO 失败仅记录日志（与 stdio 路径一致）。
        "session/delete" => {
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
            serde_json::to_value(resp)
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }

        "session/resume" => {
            let req_session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
            let cwd = params.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");

            // Load history from ThreadStore via Controller (deferred load)
            let history =
                dispatch::load_session_messages(cfg.controller.as_ref(), req_session_id).await;

            // ── 先构建 frozen + workflow_middleware ──
            cfg.session_manager.ensure_session(req_session_id, cwd);
            cfg.session_manager.ensure_session_caps(req_session_id);
            let frozen_data = cfg.session_manager.build_frozen_data(
                cwd,
                &cfg.plugin_skill_roots,
                &cfg.plugin_agent_dirs,
            );
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
                        lease: super::lease::WriterLease::acquired("default"),
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

            let resp = ResumeSessionResponse::new();
            serde_json::to_value(resp)
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }

        "session/fork" => {
            let source_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
            let cwd = params.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");

            let source_history = sessions
                .get(source_id)
                .map(|s| s.history.clone())
                .ok_or_else(|| {
                    AcpError::new(-32602, format!("source session not found: {source_id}"))
                })?;

            let (new_thread_id, copied_history) =
                dispatch::fork_session(cfg.controller.as_ref(), source_id, &source_history, cwd)
                    .await
                    .map_err(|e| AcpError::new(-32603, format!("{e}")))?;

            let new_session_id = new_thread_id.clone();

            // ── 先构建 frozen + workflow_middleware ──
            cfg.session_manager.ensure_session(&new_session_id, cwd);
            cfg.session_manager.ensure_session_caps(&new_session_id);
            let frozen_data = cfg.session_manager.build_frozen_data(
                cwd,
                &cfg.plugin_skill_roots,
                &cfg.plugin_agent_dirs,
            );
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
                    lease: super::lease::WriterLease::acquired("default"),
                },
            );

            info!(source = %source_id, new = %new_session_id, "Session forked");
            let resp = ForkSessionResponse::new(SessionId::new(new_session_id));
            serde_json::to_value(resp)
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }

        "session/update_config" => {
            let session_id = extract_session_id(params, "");
            let new_cfg: crate::provider::PeriConfig =
                serde_json::from_value(params.get("config").cloned().unwrap_or_default())
                    .map_err(|e| AcpError::new(-32602, format!("Invalid config: {e}")))?;

            if new_cfg.config.providers.is_empty() {
                return Err(AcpError::new(-32602, "providers cannot be empty"));
            }
            // Profile 是唯一事实源：各 profile 引用的 provider 必须存在于 providers
            for alias in crate::provider::Profiles::ALL {
                let pid = new_cfg
                    .config
                    .profiles
                    .get(alias)
                    .map(|p| p.provider.as_str())
                    .unwrap_or("");
                if !pid.is_empty() && !new_cfg.config.providers.iter().any(|p| p.id == pid) {
                    return Err(AcpError::new(
                        -32602,
                        format!("profile {alias}: provider '{pid}' not found"),
                    ));
                }
            }

            *cfg.peri_config.write() = new_cfg.clone();

            if let Some(p) = LlmProvider::from_config(&new_cfg) {
                tracing::debug!(
                    provider = %p.display_name(),
                    model = %p.model_name(),
                    "update_config: provider updated"
                );
                *cfg.provider.write() = p;
            } else {
                let active_profile_provider = new_cfg
                    .config
                    .profiles
                    .get(&new_cfg.config.active_alias)
                    .map(|p| p.provider.as_str())
                    .unwrap_or("");
                tracing::warn!(
                    active_provider = %active_profile_provider,
                    active_alias = %new_cfg.config.active_alias,
                    providers = new_cfg.config.providers.len(),
                    "update_config: LlmProvider::from_config returned None, provider NOT updated"
                );
            }

            // Model switch → invalidate cached LLM instances (Main Agent + SubAgent)
            if let Some(s) = sessions.get_mut(session_id) {
                s.agent_pool.invalidate();
            }

            persist_config(cfg);

            let config_options = {
                let c = cfg.peri_config.read();
                let p = cfg.provider.read();
                make_config_options(&c, &p, cfg.permission_mode.load())
            };
            send_config_option_update(transport.as_ref(), session_id, cfg).await;
            serde_json::to_value(SetSessionConfigOptionResponse::new(config_options))
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }

        "plugin/install" => {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing 'name'"))?;
            let marketplace = params
                .get("marketplace")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing 'marketplace'"))?;
            let scope_str = params
                .get("scope")
                .and_then(|v| v.as_str())
                .unwrap_or("user");
            let scope = match scope_str {
                "project" => peri_acp_types::plugin::InstallScope::Project,
                "local" => peri_acp_types::plugin::InstallScope::Local,
                _ => peri_acp_types::plugin::InstallScope::User,
            };
            let session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let claude_dir = peri_middlewares::plugin::claude_home();
            let cache_dir = cfg.plugin_manager.cache_dir();

            let caps = cfg.session_manager.get_caps(session_id);

            match cfg
                .plugin_manager
                .install(name, marketplace, scope, &cache_dir, &claude_dir)
                .await
            {
                Ok(installed) => {
                    let _ = push_plugin_action_result(
                        transport.as_ref(),
                        session_id,
                        "install",
                        name,
                        true,
                        None,
                        &caps,
                    )
                    .await;
                    let _ = push_plugin_snapshot(
                        transport.as_ref(),
                        session_id,
                        &cfg.plugin_manager.snapshot(&claude_dir),
                        &caps,
                    )
                    .await;
                    // Phase 6 B3：install 成功 → plugin 域命令条目动态刷新
                    //（注册表 on_change 自动触发投影推送；重载失败 → 保留
                    // 空 plugin 域 + 告警，不阻塞回包）
                    // 遗留登记（P2-4）：插件 mcpServers 变更自愈依赖既有池
                    // 重连机制（池为装配时快照），未在本 Phase 触发，详见
                    // `refresh_plugin_command_entries` doc 注释。
                    refresh_plugin_command_entries(
                        cfg,
                        session_id,
                        &claude_dir,
                        sessions.get(session_id).map(|s| s.cwd.as_str()),
                    );
                    Ok(serde_json::json!({ "success": true, "plugin": installed.id }))
                }
                Err(e) => {
                    let _ = push_plugin_action_result(
                        transport.as_ref(),
                        session_id,
                        "install",
                        name,
                        false,
                        Some(&e.to_string()),
                        &caps,
                    )
                    .await;
                    Err(AcpError::new(-32603, e.to_string()))
                }
            }
        }

        "plugin/uninstall" => {
            let plugin_id = params
                .get("pluginId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing 'pluginId'"))?;
            let session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let claude_dir = peri_middlewares::plugin::claude_home();

            let caps = cfg.session_manager.get_caps(session_id);

            match cfg.plugin_manager.uninstall(plugin_id, &claude_dir).await {
                Ok(()) => {
                    let _ = push_plugin_action_result(
                        transport.as_ref(),
                        session_id,
                        "uninstall",
                        plugin_id,
                        true,
                        None,
                        &caps,
                    )
                    .await;
                    let _ = push_plugin_snapshot(
                        transport.as_ref(),
                        session_id,
                        &cfg.plugin_manager.snapshot(&claude_dir),
                        &caps,
                    )
                    .await;
                    // Phase 6 B3：uninstall 成功 → plugin 域命令条目动态刷新
                    //（注册表 on_change 自动触发投影推送；重载失败 → 保留
                    // 空 plugin 域 + 告警，不阻塞回包）
                    // 遗留登记（P2-4）：插件 mcpServers 变更自愈依赖既有池
                    // 重连机制（池为装配时快照），未在本 Phase 触发，详见
                    // `refresh_plugin_command_entries` doc 注释。
                    refresh_plugin_command_entries(
                        cfg,
                        session_id,
                        &claude_dir,
                        sessions.get(session_id).map(|s| s.cwd.as_str()),
                    );
                    Ok(serde_json::json!({ "success": true }))
                }
                Err(e) => {
                    let _ = push_plugin_action_result(
                        transport.as_ref(),
                        session_id,
                        "uninstall",
                        plugin_id,
                        false,
                        Some(&e.to_string()),
                        &caps,
                    )
                    .await;
                    Err(AcpError::new(-32603, e.to_string()))
                }
            }
        }

        "plugin/toggle" => {
            let plugin_id = params
                .get("pluginId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing 'pluginId'"))?;
            let enable = params
                .get("enable")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let scope_str = params
                .get("scope")
                .and_then(|v| v.as_str())
                .unwrap_or("user");
            let scope = match scope_str {
                "project" => peri_acp_types::plugin::InstallScope::Project,
                "local" => peri_acp_types::plugin::InstallScope::Local,
                _ => peri_acp_types::plugin::InstallScope::User,
            };
            let session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let claude_dir = peri_middlewares::plugin::claude_home();

            let result = cfg
                .plugin_manager
                .set_enabled(plugin_id, scope, &claude_dir, enable);

            let caps = cfg.session_manager.get_caps(session_id);

            match result {
                Ok(()) => {
                    let action = if enable { "enable" } else { "disable" };
                    let _ = push_plugin_action_result(
                        transport.as_ref(),
                        session_id,
                        action,
                        plugin_id,
                        true,
                        None,
                        &caps,
                    )
                    .await;
                    let _ = push_plugin_snapshot(
                        transport.as_ref(),
                        session_id,
                        &cfg.plugin_manager.snapshot(&claude_dir),
                        &caps,
                    )
                    .await;
                    Ok(serde_json::json!({ "success": true }))
                }
                Err(e) => {
                    let action = if enable { "enable" } else { "disable" };
                    let _ = push_plugin_action_result(
                        transport.as_ref(),
                        session_id,
                        action,
                        plugin_id,
                        false,
                        Some(&e.to_string()),
                        &caps,
                    )
                    .await;
                    Err(AcpError::new(-32603, e.to_string()))
                }
            }
        }

        "plugin/search" => {
            let query = params
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing 'query'"))?;
            let session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let cache_dir = cfg.plugin_manager.cache_dir();
            let results = search_marketplace_plugins(query, &cache_dir);

            let caps = cfg.session_manager.get_caps(session_id);
            let _ =
                push_plugin_search_result(transport.as_ref(), session_id, query, &results, &caps)
                    .await;
            Ok(serde_json::json!({ "results": results.iter().map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "version": r.version,
                    "description": r.description,
                    "marketplace": r.marketplace,
                })
            }).collect::<Vec<_>>() }))
        }

        "plugin/update" => {
            let plugin_id = params
                .get("pluginId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing 'pluginId'"))?;
            let session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let claude_dir = peri_middlewares::plugin::claude_home();
            let cache_dir = cfg.plugin_manager.cache_dir();

            let caps = cfg.session_manager.get_caps(session_id);

            match cfg
                .plugin_manager
                .update(plugin_id, &cache_dir, &claude_dir)
                .await
            {
                Ok(updated) => {
                    let _ = push_plugin_action_result(
                        transport.as_ref(),
                        session_id,
                        "update",
                        plugin_id,
                        true,
                        None,
                        &caps,
                    )
                    .await;
                    let _ = push_plugin_snapshot(
                        transport.as_ref(),
                        session_id,
                        &cfg.plugin_manager.snapshot(&claude_dir),
                        &caps,
                    )
                    .await;
                    Ok(serde_json::json!({ "success": true, "plugin": updated.id }))
                }
                Err(e) => {
                    let _ = push_plugin_action_result(
                        transport.as_ref(),
                        session_id,
                        "update",
                        plugin_id,
                        false,
                        Some(&e.to_string()),
                        &caps,
                    )
                    .await;
                    Err(AcpError::new(-32603, e.to_string()))
                }
            }
        }

        "session/rename" => {
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
            super::notify::send_session_info_update_with_title(
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

        "session/rewind-candidates" => {
            let session_id = params
                .get("sessionId")
                .or_else(|| params.get("session_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
            require_rewind_cap(&cfg.session_manager.get_caps(session_id))?;
            let history = sessions
                .get(session_id)
                .map(|s| s.history.clone())
                .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
            dispatch::rewind_candidates(&history)
        }

        "session/rewind-preview" => {
            let session_id = params
                .get("sessionId")
                .or_else(|| params.get("session_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
                .to_string();
            require_rewind_cap(&cfg.session_manager.get_caps(&session_id))?;
            let (cwd, history) = sessions
                .get(&session_id)
                .map(|s| (s.cwd.clone(), s.history.clone()))
                .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
            // Phase 5 Step 5：RewindError 变体删除，preview 为只读路径
            // 零事件——不再需要 event_sink。
            dispatch::rewind_preview(params, &history, &cwd, &session_id).await
        }

        "session/rewind" => {
            let session_id = params
                .get("sessionId")
                .or_else(|| params.get("session_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
                .to_string();
            require_rewind_cap(&cfg.session_manager.get_caps(&session_id))?;
            let (cwd, history) = {
                let s = sessions
                    .get_mut(&session_id)
                    .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
                (s.cwd.clone(), s.history.clone())
            };
            let event_sink: Arc<dyn crate::session::event_sink::EventSink> =
                Arc::new(crate::session::event_sink::TransportEventSink::new(
                    transport.clone(), // transport: &Arc<dyn AcpTransport>（签名改动见下方实现注记）
                    cfg.session_manager.caps_registry(),
                ));
            let peri_config_snapshot = Arc::new(cfg.peri_config.read().clone());
            dispatch::rewind_execute(
                params,
                history,
                &cwd,
                &peri_config_snapshot,
                &event_sink,
                None, // auxiliary_model：RewindCommand 不使用
                &tokio_util::sync::CancellationToken::new(),
                cfg.controller.as_ref(),
                Some(session_id.clone()),
                None, // bg_event_tx
                None, // task_manager
                None,
                None,
                None,
                None, // frozen_*：RewindCommand 不使用
            )
            .await
            .inspect(|resp| {
                // P1：回写截断后的 history——SessionState.history 是后续
                // session/rewind-candidates 与 session/rewind-preview 的数据源，
                // 必须与 RewindCompleted 事件中的结果一致。
                if let (Some(h), Some(s)) = (
                    resp.get("history").and_then(|v| v.as_array()),
                    sessions.get_mut(&session_id),
                ) {
                    let h = h.clone();
                    if let Ok(msgs) = serde_json::from_value::<
                        Vec<peri_acp_types::messages::BaseMessage>,
                    >(serde_json::Value::Array(h))
                    {
                        s.history = msgs;
                    }
                }
            })
        }

        "marketplace/refresh" => {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing 'name'"))?;
            // 定位 known_marketplaces 条目 + 刷新（实现留在插件管理端口，
            // 命令面不触碰 marketplace 目录结构）
            match cfg.plugin_manager.refresh_marketplace(name).await {
                Ok(plugin_count) => {
                    Ok(serde_json::json!({ "success": true, "pluginCount": plugin_count }))
                }
                Err(e) => Err(AcpError::new(-32603, e)),
            }
        }

        // ── MCP OAuth 授权交互（专用 peri.oauth + legacy TUI 兼容）──
        "mcp/list" => {
            let caps = cfg.session_manager.effective_host_caps();
            if !caps.oauth {
                return Err(AcpError::new(
                    -32601,
                    "peri.oauth capability not negotiated",
                ));
            }
            let pool = cfg
                .mcp_pool
                .clone()
                .ok_or_else(|| AcpError::new(-32603, "mcp pool not available"))?
                .downcast_arc::<peri_middlewares::mcp::McpClientPool>()
                .map_err(|_| AcpError::new(-32603, "mcp pool type mismatch"))?;
            let mut servers = pool.all_server_infos();
            servers.sort_by(|left, right| left.name.cmp(&right.name));
            let servers = servers
                .into_iter()
                .filter(|server| crate::event::oauth::validate_server_name(&server.name).is_ok())
                .take(256)
                .map(|server| {
                    let connection_status = match server.status {
                        peri_middlewares::mcp::ClientStatus::Connected => "connected",
                        peri_middlewares::mcp::ClientStatus::Failed(_) => "failed",
                        peri_middlewares::mcp::ClientStatus::Disconnected => "disconnected",
                        peri_middlewares::mcp::ClientStatus::Disabled => "disabled",
                        peri_middlewares::mcp::ClientStatus::Uninitialized => "uninitialized",
                    };
                    let oauth_status = match server.oauth_status {
                        peri_middlewares::mcp::OAuthStatus::None => "none",
                        peri_middlewares::mcp::OAuthStatus::Authorized => "authorized",
                        peri_middlewares::mcp::OAuthStatus::NeedsAuthorization => {
                            "needs_authorization"
                        }
                    };
                    serde_json::json!({
                        "name": server.name,
                        "transport": server.transport_type,
                        "connectionStatus": connection_status,
                        "oauthStatus": oauth_status,
                        "activeFlowId": pool.active_oauth_flow(&server.name),
                        "toolsCount": server.tool_count,
                        "resourcesCount": server.resource_count,
                    })
                })
                .collect::<Vec<_>>();
            Ok(serde_json::json!({ "servers": servers }))
        }
        "mcp/oauth_start" => {
            // 用户经 MCP 面板显式发起授权：host pool 异步执行 OAuth 流程
            // （spawn_oauth_flow 内部标记 NeedsAuthorization → run_oauth_flow
            // → AuthorizationNeeded 事件 → TUI 弹 popup）。不阻塞请求。
            let server_name = params
                .get("server_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing 'server_name'"))?
                .to_string();
            crate::event::oauth::validate_server_name(&server_name)
                .map_err(|error| AcpError::new(-32602, error.to_string()))?;
            let caps = cfg.session_manager.effective_host_caps();
            if !caps.oauth && !caps.agent_event {
                return Err(AcpError::new(-32601, "OAuth capability not negotiated"));
            }
            let flow_id = match params.get("flow_id").and_then(Value::as_str) {
                Some(flow_id) => {
                    crate::event::oauth::validate_identifier(flow_id)
                        .map_err(|error| AcpError::new(-32602, error.to_string()))?;
                    flow_id.to_string()
                }
                None if caps.agent_event && !caps.oauth => uuid::Uuid::now_v7().to_string(),
                None => return Err(AcpError::new(-32602, "missing 'flow_id'")),
            };
            let pool = cfg
                .mcp_pool
                .clone()
                .ok_or_else(|| AcpError::new(-32603, "mcp pool not available"))?;
            match pool.downcast_arc::<peri_middlewares::mcp::McpClientPool>() {
                Ok(p) => {
                    let disposition = p.spawn_oauth_flow_with_id(&server_name, &flow_id);
                    let (status, active_flow_id) = match disposition {
                        peri_middlewares::mcp::OAuthStartDisposition::Started => {
                            ("started", flow_id.clone())
                        }
                        peri_middlewares::mcp::OAuthStartDisposition::AlreadyActive => {
                            ("already_active", flow_id.clone())
                        }
                        peri_middlewares::mcp::OAuthStartDisposition::Conflict {
                            active_flow_id,
                        } => ("conflict", active_flow_id),
                    };
                    Ok(serde_json::json!({
                        "success": status != "conflict",
                        "status": status,
                        "flowId": flow_id,
                        "activeFlowId": active_flow_id,
                    }))
                }
                Err(_) => Err(AcpError::new(-32603, "mcp pool type mismatch")),
            }
        }
        "mcp/oauth_callback" => {
            let caps = cfg.session_manager.effective_host_caps();
            if caps.oauth {
                return Err(AcpError::new(
                    -32601,
                    "peri.oauth uses the loopback callback; callback codes are not accepted over ACP",
                ));
            }
            if !caps.agent_event {
                return Err(AcpError::new(-32601, "OAuth capability not negotiated"));
            }
            let server_name = params
                .get("server_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing 'server_name'"))?
                .to_string();
            let code = params
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let state = params
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if code.len() > 4096 || state.len() > 4096 {
                return Err(AcpError::new(-32602, "OAuth callback value too long"));
            }
            let pool = cfg
                .mcp_pool
                .clone()
                .ok_or_else(|| AcpError::new(-32603, "mcp pool not available"))?;
            let pool = pool
                .downcast_arc::<peri_middlewares::mcp::McpClientPool>()
                .map_err(|_| AcpError::new(-32603, "mcp pool type mismatch"))?;
            let result = pool.deliver_oauth_callback(&server_name, code, state);
            result
                .map(|_| serde_json::json!({ "success": true }))
                .map_err(|e| AcpError::new(-32603, e))
        }
        "mcp/oauth_cancel" => {
            let caps = cfg.session_manager.effective_host_caps();
            if !caps.oauth && !caps.agent_event {
                return Err(AcpError::new(-32601, "OAuth capability not negotiated"));
            }
            let pool = cfg
                .mcp_pool
                .clone()
                .ok_or_else(|| AcpError::new(-32603, "mcp pool not available"))?;
            match pool.downcast_arc::<peri_middlewares::mcp::McpClientPool>() {
                Ok(p) => {
                    let cancelled =
                        if let Some(flow_id) = params.get("flow_id").and_then(Value::as_str) {
                            crate::event::oauth::validate_identifier(flow_id)
                                .map_err(|error| AcpError::new(-32602, error.to_string()))?;
                            p.cancel_oauth_flow(flow_id)
                        } else if caps.agent_event && !caps.oauth {
                            let server_name = params
                                .get("server_name")
                                .and_then(Value::as_str)
                                .ok_or_else(|| AcpError::new(-32602, "missing 'flow_id'"))?;
                            p.cancel_oauth_callback(server_name)
                        } else {
                            return Err(AcpError::new(-32602, "missing 'flow_id'"));
                        };
                    Ok(serde_json::json!({ "success": true, "cancelled": cancelled }))
                }
                Err(_) => Err(AcpError::new(-32603, "mcp pool type mismatch")),
            }
        }

        _ => Err(AcpError::new(-32601, format!("Method not found: {method}"))),
    }
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

fn require_rewind_cap(caps: &PeriCaps) -> Result<(), AcpError> {
    if caps.rewind {
        Ok(())
    } else {
        Err(AcpError::new(
            -32601,
            "peri.rewind capability not negotiated",
        ))
    }
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

// ── Plugin event pushers ──────────────────────────────────────────────────

async fn push_plugin_action_result(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
    action: &str,
    plugin_name: &str,
    success: bool,
    error: Option<&str>,
    caps: &PeriCaps,
) {
    if !caps.unstable_event {
        return;
    }
    let payload = PluginActionResult {
        action: action.to_string(),
        plugin_name: plugin_name.to_string(),
        success,
        error: error.map(|s| s.to_string()),
    };
    let data = serde_json::to_value(&payload).unwrap_or_default();
    let envelope = serde_json::json!({
        "sessionId": session_id,
        "event": "plugin-action-result",
        "data": data,
    });
    if let Err(e) = transport
        .send_notification("peri/unstable_event", envelope)
        .await
    {
        tracing::warn!(error = %e, "Failed to push plugin-action-result");
    }
}

async fn push_plugin_snapshot(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
    plugins: &[PluginSnapshotEntry],
    caps: &PeriCaps,
) {
    if !caps.unstable_event {
        return;
    }
    let payload = PluginSnapshot {
        plugins: plugins.to_vec(),
    };
    let data = serde_json::to_value(&payload).unwrap_or_default();
    let envelope = serde_json::json!({
        "sessionId": session_id,
        "event": "plugin-snapshot",
        "data": data,
    });
    if let Err(e) = transport
        .send_notification("peri/unstable_event", envelope)
        .await
    {
        tracing::warn!(error = %e, "Failed to push plugin-snapshot");
    }
}

async fn push_plugin_search_result(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
    query: &str,
    results: &[PluginSnapshotEntry],
    caps: &PeriCaps,
) {
    if !caps.unstable_event {
        return;
    }
    let payload = PluginSearchResult {
        query: query.to_string(),
        results: results.to_vec(),
        from_cache: true,
    };
    let data = serde_json::to_value(&payload).unwrap_or_default();
    let envelope = serde_json::json!({
        "sessionId": session_id,
        "event": "plugin-search-result",
        "data": data,
    });
    if let Err(e) = transport
        .send_notification("peri/unstable_event", envelope)
        .await
    {
        tracing::warn!(error = %e, "Failed to push plugin-search-result");
    }
}

fn search_marketplace_plugins(
    query: &str,
    cache_dir: &std::path::Path,
) -> Vec<PluginSnapshotEntry> {
    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        for entry in entries.flatten() {
            let mp_dir = entry.path();
            let mp_name = mp_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let manifest_path = mp_dir.join("marketplace.json");
            let Ok(content) = std::fs::read_to_string(&manifest_path) else {
                continue;
            };
            let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) else {
                continue;
            };
            if let Some(plugins) = manifest.get("plugins").and_then(|v| v.as_array()) {
                for p in plugins {
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let desc = p.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    if name.to_lowercase().contains(&query_lower)
                        || desc.to_lowercase().contains(&query_lower)
                    {
                        results.push(PluginSnapshotEntry {
                            name: name.to_string(),
                            version: p
                                .get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            enabled: false,
                            root: String::new(),
                            description: desc.to_string(),
                            marketplace: mp_name.clone(),
                            author: p
                                .get("author")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            skills_count: 0,
                            commands_count: 0,
                            agents_count: 0,
                            mcp_count: 0,
                            install_scope: String::new(),
                            load_error: None,
                        });
                    }
                }
            }
        }
    }
    results
}

#[cfg(test)]
#[path = "requests_test.rs"]
mod tests;
