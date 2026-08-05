//! ACP Request dispatch — handles all ACP protocol request methods.
//! Extracted from original acp_server.rs (2026-05-20 split).

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    CloseSessionResponse, ForkSessionResponse, ListSessionsResponse, LoadSessionResponse,
    NewSessionResponse, ResumeSessionResponse, SessionId, SessionInfo, SessionNotification,
    SetSessionConfigOptionResponse, SetSessionModeResponse,
};
use peri_acp::dispatch::ReplaySender;
use peri_acp::dispatch::config_update::make_config_options;
use peri_acp::{dispatch, transport::types::AcpError};
use peri_acp_types::PeriCaps;
use peri_acp_types::event_data::{
    PluginActionResult, PluginSearchResult, PluginSnapshot, PluginSnapshotEntry,
};
use peri_agent::thread::ThreadMeta;
use serde_json::Value;
use tracing::{debug, info, warn};

use super::{
    AcpServerConfig, SessionState, apply_profile_effort, build_mode_state,
    notify::{extract_session_id, send_available_commands_update, send_config_option_update},
    parse_permission_mode,
};
use crate::{app::agent::LlmProvider, config::save_to};

fn persist_config(cfg: &AcpServerConfig) {
    let c = cfg.peri_config.read();
    if let Err(e) = save_to(&c, &cfg.config_path) {
        tracing::warn!(error = %e, "Failed to persist config");
    }
}

/// 创建 session 级 WorkflowMiddleware。
fn create_session_workflow_middleware(
    cfg: &AcpServerConfig,
    cwd: &str,
    session_id: &str,
    frozen_data: &peri_acp::session::executor::FrozenSessionData,
) -> Option<Arc<peri_middlewares::workflow::WorkflowMiddleware>> {
    let mut compact_config = peri_agent::agent::CompactConfig::default();
    compact_config.apply_env_overrides();
    let (progress_tx, progress_rx) =
        tokio::sync::mpsc::unbounded_channel::<peri_workflow::protocol::ProgressEvent>();
    let wf_executor = peri_acp::agent::workflow_agent::create_executor(
        peri_acp::agent::workflow_agent::WorkflowAgentContext {
            provider: Arc::clone(&cfg.provider),
            cwd: cwd.to_string(),
            frozen_claude_md: frozen_data.claude_md().map(|s| s.to_string()),
            frozen_claude_local_md: frozen_data.claude_local_md().map(|s| s.to_string()),
            frozen_skill_summary: frozen_data.skill_summary().map(|s| s.to_string()),
            session_id: Some(session_id.to_string()),
            compact_config: Some(compact_config),
            cancel: None,
            // 无 16_workflow 版本（P2-2026-08-02）：workflow agent 链不
            // 注册 WorkflowTool，不得复用带 workflow 声明的主 prompt。
            system_prompt: Some(frozen_data.subagent_system_prompt().to_string()),
            broker: None,
            permission_mode: None,
            frozen_date: Some(frozen_data.date().to_string()),
            frozen_language: frozen_data.language().map(|s| s.to_string()),
            agent_pool: None,
            langfuse_session: None,
            thread_store: None,
            peri_config: Some(Arc::new(cfg.peri_config.read().clone())),
            progress_tx: Some(progress_tx),
        },
    );
    let (notification_tx, _) = tokio::sync::broadcast::channel(32);
    Some(Arc::new(
        peri_middlewares::workflow::WorkflowMiddleware::new(
            wf_executor,
            cwd,
            notification_tx,
            Some(progress_rx),
        ),
    ))
}

pub(crate) async fn handle_request(
    method: &str,
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &Arc<dyn peri_acp::transport::AcpTransport>,
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
                true, // workflow_enabled：session 路径随后创建 WorkflowMiddleware
            );

            // Create session-scoped WorkflowMiddleware at session/new (GAP-05: inject frozen data)
            let workflow_middleware =
                create_session_workflow_middleware(cfg, &cwd, &session_id, &frozen_data);

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
                    agent_pool: peri_acp::session::agent_pool::AgentPool::new(),
                    workflow_middleware,
                    title: None,
                    tags: Vec::new(),
                    continuation_armed: false,
                    continuation_epoch: 0,
                    continuation_in_flight: false,
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
            // Scan skills for AvailableCommands
            let disable_bundled = peri_middlewares::skills::load_disable_bundled_skills();
            let skill_roots = peri_middlewares::SkillsMiddleware::resolve_roots_static(
                &cwd,
                cfg.plugin_skill_roots.clone(),
                disable_bundled, // TUI 侧仅用于显示
            );
            let skills = peri_middlewares::skills::scan_skill_roots(&skill_roots);

            // 将暂存的 peri caps 关联到新 session。
            // MpscTransport 路径：若未显式调用 initialize（TUI 内部连接），
            // 默认全部 cap=true（TUI 需要接收所有自定义事件）。
            let peri_caps = cfg.session_manager.ensure_session_caps(&session_id);

            send_available_commands_update(transport.as_ref(), &session_id, &skills, &peri_caps)
                .await;

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

            // Load history from ThreadStore
            let history =
                dispatch::load_session_messages(cfg.thread_store.as_ref(), req_session_id).await;

            // ── 先构建 frozen + workflow_middleware，再插入 session ──
            cfg.session_manager.ensure_session(req_session_id, cwd);
            let caps = cfg.session_manager.ensure_session_caps(req_session_id);
            let frozen_data = cfg.session_manager.build_frozen_data(
                cwd,
                &cfg.plugin_skill_roots,
                &cfg.plugin_agent_dirs,
                true, // workflow_enabled：session 路径随后创建 WorkflowMiddleware
            );
            let workflow_middleware =
                create_session_workflow_middleware(cfg, cwd, req_session_id, &frozen_data);

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
                        agent_pool: peri_acp::session::agent_pool::AgentPool::new(),
                        workflow_middleware,
                        title: None,
                        tags: Vec::new(),
                        continuation_armed: false,
                        continuation_epoch: 0,
                        continuation_in_flight: false,
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
            // Scan skills for AvailableCommands (same as session/new)
            let disable_bundled = peri_middlewares::skills::load_disable_bundled_skills();
            let skill_roots = peri_middlewares::SkillsMiddleware::resolve_roots_static(
                cwd,
                cfg.plugin_skill_roots.clone(),
                disable_bundled, // TUI 侧仅用于显示
            );
            let skills = peri_middlewares::skills::scan_skill_roots(&skill_roots);
            send_available_commands_update(transport.as_ref(), req_session_id, &skills, &caps)
                .await;
            serde_json::to_value(resp)
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }

        "session/list" => {
            let threads = cfg
                .thread_store
                .list_threads()
                .await
                .map_err(|e| AcpError::new(-32603, format!("Failed to list sessions: {e}")))?;

            let cwd_filter = params.get("cwd").and_then(|v| v.as_str());

            let entries: Vec<SessionInfo> = threads
                .into_iter()
                .filter(|t| {
                    if let Some(cwd) = cwd_filter {
                        t.cwd == cwd
                    } else {
                        true
                    }
                })
                .map(|t| {
                    SessionInfo::new(
                        SessionId::new(t.id.clone()),
                        std::path::PathBuf::from(t.cwd.clone()),
                    )
                    .title(t.title.clone())
                    .updated_at(t.updated_at.to_rfc3339())
                })
                .collect();

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
                .map(|mw| mw.progress_store().get_all_runs_snapshot())
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
            let killed = mw.runner().kill_agent(run_id, agent_id).await;

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
            let killed = mw.registry().kill(run_id).is_ok();

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
                .resume_workflow(run_id)
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
                .background_registry
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

        "session/resume" => {
            let req_session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
            let cwd = params.get("cwd").and_then(|v| v.as_str()).unwrap_or(".");

            // Load history from ThreadStore (deferred load)
            let history =
                dispatch::load_session_messages(cfg.thread_store.as_ref(), req_session_id).await;

            // ── 先构建 frozen + workflow_middleware ──
            cfg.session_manager.ensure_session(req_session_id, cwd);
            cfg.session_manager.ensure_session_caps(req_session_id);
            let frozen_data = cfg.session_manager.build_frozen_data(
                cwd,
                &cfg.plugin_skill_roots,
                &cfg.plugin_agent_dirs,
                true, // workflow_enabled：session 路径随后创建 WorkflowMiddleware
            );
            let workflow_middleware =
                create_session_workflow_middleware(cfg, cwd, req_session_id, &frozen_data);

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
                        agent_pool: peri_acp::session::agent_pool::AgentPool::new(),
                        workflow_middleware,
                        title: None,
                        tags: Vec::new(),
                        continuation_armed: false,
                        continuation_epoch: 0,
                        continuation_in_flight: false,
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
                dispatch::fork_session(cfg.thread_store.as_ref(), source_id, &source_history, cwd)
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
                true, // workflow_enabled：session 路径随后创建 WorkflowMiddleware
            );
            let workflow_middleware =
                create_session_workflow_middleware(cfg, cwd, &new_session_id, &frozen_data);

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
                    agent_pool: peri_acp::session::agent_pool::AgentPool::new(),
                    workflow_middleware,
                    title: None,
                    tags: Vec::new(),
                    continuation_armed: false,
                    continuation_epoch: 0,
                    continuation_in_flight: false,
                },
            );

            info!(source = %source_id, new = %new_session_id, "Session forked");
            let resp = ForkSessionResponse::new(SessionId::new(new_session_id));
            serde_json::to_value(resp)
                .map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
        }

        "session/update_config" => {
            let session_id = extract_session_id(params, "");
            let new_cfg: crate::config::PeriConfig =
                serde_json::from_value(params.get("config").cloned().unwrap_or_default())
                    .map_err(|e| AcpError::new(-32602, format!("Invalid config: {e}")))?;

            if new_cfg.config.providers.is_empty() {
                return Err(AcpError::new(-32602, "providers cannot be empty"));
            }
            // Profile 是唯一事实源：各 profile 引用的 provider 必须存在于 providers
            for alias in crate::config::Profiles::ALL {
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
                "project" => peri_middlewares::plugin::InstallScope::Project,
                "local" => peri_middlewares::plugin::InstallScope::Local,
                _ => peri_middlewares::plugin::InstallScope::User,
            };
            let session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let claude_dir = dirs_next::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".claude");
            let cache_dir = peri_middlewares::plugin::config::marketplaces_cache_dir();

            let caps = cfg.session_manager.get_caps(session_id);

            match peri_middlewares::plugin::install_plugin(
                name,
                marketplace,
                scope,
                &cache_dir,
                &claude_dir,
                None,
            )
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
                    let _ =
                        push_plugin_snapshot(transport.as_ref(), session_id, &claude_dir, &caps)
                            .await;
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

            let claude_dir = dirs_next::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".claude");

            let caps = cfg.session_manager.get_caps(session_id);

            match peri_middlewares::plugin::uninstall_plugin(plugin_id, &claude_dir, None).await {
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
                    let _ =
                        push_plugin_snapshot(transport.as_ref(), session_id, &claude_dir, &caps)
                            .await;
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
                "project" => peri_middlewares::plugin::InstallScope::Project,
                "local" => peri_middlewares::plugin::InstallScope::Local,
                _ => peri_middlewares::plugin::InstallScope::User,
            };
            let session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let claude_dir = dirs_next::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".claude");

            let result = if enable {
                peri_middlewares::plugin::update_enabled_plugins(
                    plugin_id,
                    scope,
                    &claude_dir,
                    None,
                )
            } else {
                peri_middlewares::plugin::remove_from_enabled_plugins(
                    plugin_id,
                    &scope,
                    &claude_dir,
                    None,
                )
            };

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
                    let _ =
                        push_plugin_snapshot(transport.as_ref(), session_id, &claude_dir, &caps)
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

            let cache_dir = peri_middlewares::plugin::config::marketplaces_cache_dir();
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

            let claude_dir = dirs_next::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".claude");
            let cache_dir = peri_middlewares::plugin::config::marketplaces_cache_dir();

            let caps = cfg.session_manager.get_caps(session_id);

            match peri_middlewares::plugin::update_plugin(plugin_id, &cache_dir, &claude_dir, None)
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
                    let _ =
                        push_plugin_snapshot(transport.as_ref(), session_id, &claude_dir, &caps)
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
            let history = sessions
                .get(&session_id)
                .map(|s| s.history.clone())
                .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
            let event_sink: Arc<dyn peri_acp::session::event_sink::EventSink> =
                Arc::new(peri_acp::session::event_sink::TransportEventSink::new(
                    transport.clone(), // transport: &Arc<dyn AcpTransport>（签名改动见下方实现注记）
                    cfg.session_manager.caps_registry(),
                ));
            dispatch::rewind_preview(params, &history, &event_sink, &session_id).await
        }

        "session/rewind" => {
            let session_id = params
                .get("sessionId")
                .or_else(|| params.get("session_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
                .to_string();
            let (cwd, history) = {
                let s = sessions
                    .get_mut(&session_id)
                    .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
                (s.cwd.clone(), s.history.clone())
            };
            let event_sink: Arc<dyn peri_acp::session::event_sink::EventSink> =
                Arc::new(peri_acp::session::event_sink::TransportEventSink::new(
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
                &peri_agent::agent::AgentCancellationToken::new(),
                Some(cfg.thread_store.clone()),
                Some(session_id.clone()),
                None, // bg_event_tx
                None, // bg_registry
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
                        Vec<peri_agent::messages::BaseMessage>,
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
            // 从 known_marketplaces.json 查找 source
            let kms = peri_middlewares::plugin::load_known_marketplaces(None)
                .map_err(|e| AcpError::new(-32603, format!("Failed to load marketplaces: {e}")))?;
            let km = kms
                .iter()
                .find(|km| {
                    peri_middlewares::plugin::MarketplaceManager::extract_name(&km.source) == name
                })
                .ok_or_else(|| AcpError::new(-32602, format!("marketplace not found: {name}")))?;

            match peri_middlewares::plugin::marketplace::refresh_marketplace(&km.source, name).await
            {
                Ok((manifest, _install_location)) => {
                    let plugin_count = manifest.plugins.len();
                    Ok(serde_json::json!({ "success": true, "pluginCount": plugin_count }))
                }
                Err(e) => Err(AcpError::new(-32603, e.to_string())),
            }
        }

        _ => Err(AcpError::new(-32601, format!("Method not found: {method}"))),
    }
}

/// Adapts `&dyn AcpTransport` into a `ReplaySender` for the TUI path.
struct TuiReplaySender<'a> {
    transport: &'a dyn peri_acp::transport::AcpTransport,
}

#[async_trait::async_trait]
impl ReplaySender for TuiReplaySender<'_> {
    async fn send(
        &self,
        notif: SessionNotification,
    ) -> Result<(), peri_acp::dispatch::ReplayError> {
        let payload = serde_json::to_value(&notif)
            .map_err(|e| peri_acp::dispatch::ReplayError::SendFailed(e.to_string()))?;
        self.transport
            .send_notification("session/update", payload)
            .await
            .map_err(|e| peri_acp::dispatch::ReplayError::SendFailed(e.to_string()))
    }
}

// ── Plugin event pushers ──────────────────────────────────────────────────

async fn push_plugin_action_result(
    transport: &dyn peri_acp::transport::AcpTransport,
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
        .send_notification("peri/unstable-event", envelope)
        .await
    {
        tracing::warn!(error = %e, "Failed to push plugin-action-result");
    }
}

async fn push_plugin_snapshot(
    transport: &dyn peri_acp::transport::AcpTransport,
    session_id: &str,
    claude_dir: &std::path::Path,
    caps: &PeriCaps,
) {
    if !caps.unstable_event {
        return;
    }
    let plugins = collect_plugin_snapshot(claude_dir);
    let payload = PluginSnapshot { plugins };
    let data = serde_json::to_value(&payload).unwrap_or_default();
    let envelope = serde_json::json!({
        "sessionId": session_id,
        "event": "plugin-snapshot",
        "data": data,
    });
    if let Err(e) = transport
        .send_notification("peri/unstable-event", envelope)
        .await
    {
        tracing::warn!(error = %e, "Failed to push plugin-snapshot");
    }
}

async fn push_plugin_search_result(
    transport: &dyn peri_acp::transport::AcpTransport,
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
        .send_notification("peri/unstable-event", envelope)
        .await
    {
        tracing::warn!(error = %e, "Failed to push plugin-search-result");
    }
}

fn collect_plugin_snapshot(claude_dir: &std::path::Path) -> Vec<PluginSnapshotEntry> {
    let loaded = peri_middlewares::plugin::load_enabled_plugins_aggregated(claude_dir, None);

    let plugins_path = claude_dir.join("plugins").join("installed_plugins.json");
    let installed = peri_middlewares::plugin::load_installed_plugins(Some(&plugins_path))
        .ok()
        .unwrap_or_default();

    loaded
        .plugins
        .iter()
        .map(|p| PluginSnapshotEntry {
            name: p.manifest.name.clone(),
            version: p.manifest.version.clone(),
            enabled: installed.plugins.iter().any(|ip| ip.name == p.name),
            root: p.install_path.to_string_lossy().to_string(),
            description: p.manifest.description.clone(),
            marketplace: p.marketplace.clone(),
            author: p.manifest.author.as_ref().map(|a| a.name.clone()),
            skills_count: p.skills_roots.len(),
            commands_count: p.commands.len(),
            agents_count: p.agents_dirs.len(),
            mcp_count: p.mcp_servers.len(),
            install_scope: installed
                .plugins
                .iter()
                .find(|ip| ip.name == p.name)
                .map(|ip| format!("{:?}", ip.scope).to_lowercase())
                .unwrap_or_default(),
            load_error: None,
        })
        .collect()
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
            if let Ok(content) = std::fs::read_to_string(&manifest_path)
                && let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content)
                && let Some(plugins) = manifest.get("plugins").and_then(|v| v.as_array())
            {
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
