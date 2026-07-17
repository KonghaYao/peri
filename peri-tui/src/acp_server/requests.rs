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
use peri_acp_types::event_data::{
    PluginActionResult, PluginSearchResult, PluginSnapshot, PluginSnapshotEntry,
};
use peri_agent::thread::ThreadMeta;
use serde_json::Value;
use tracing::{debug, info, warn};

use super::{
    AcpServerConfig, SessionState, apply_thinking_effort, build_mode_state,
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

pub(crate) async fn handle_request(
    method: &str,
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &mut HashMap<String, SessionState>,
    transport: &dyn peri_acp::transport::AcpTransport,
) -> Result<Value, AcpError> {
    match method {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(|v| v.as_u64())
                .unwrap_or(1);
            info!(protocol_version = %version, "ACP initialize");
            let resp = dispatch::build_initialize_response();
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
            let workflow_middleware = {
                let mut compact_config = peri_agent::agent::compact::CompactConfig::default();
                compact_config.apply_env_overrides();
                let wf_executor = peri_acp::agent::workflow_agent::create_executor(
                    peri_acp::agent::workflow_agent::WorkflowAgentContext {
                        provider: Arc::clone(&cfg.provider),
                        cwd: cwd.clone(),
                        frozen_claude_md: frozen_data.claude_md().map(|s| s.to_string()),
                        frozen_claude_local_md: frozen_data
                            .claude_local_md()
                            .map(|s| s.to_string()),
                        frozen_skill_summary: frozen_data.skill_summary().map(|s| s.to_string()),
                        session_id: Some(session_id.clone()),
                        compact_config: Some(compact_config),
                        cancel: None,
                        system_prompt: Some(frozen_data.system_prompt().to_string()),
                        broker: None,
                        permission_mode: None,
                        frozen_date: Some(frozen_data.date().to_string()),
                        frozen_language: frozen_data.language().map(|s| s.to_string()),
                        agent_pool: None,
                        langfuse_session: None,
                        thread_store: None,
                        peri_config: Some(Arc::new(cfg.peri_config.read().clone())),
                    },
                );
                let (notification_tx, _) = tokio::sync::broadcast::channel(32);
                Some(Arc::new(
                    peri_middlewares::workflow::WorkflowMiddleware::new(
                        wf_executor,
                        &cwd,
                        notification_tx,
                    ),
                ))
            };

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
            send_available_commands_update(transport, &session_id, &skills).await;

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
            send_config_option_update(transport, session_id, cfg).await;
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
                    apply_thinking_effort(&cfg.peri_config, value);
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
                    {
                        let mut c = cfg.peri_config.write();
                        c.config.context_1m = Some(enabled);
                    }
                    persist_config(cfg);
                    info!(enabled = %enabled, "Context 1M changed via configOption (persisted)");
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
            send_config_option_update(transport, session_id, cfg).await;
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

            // Insert into sessions if not already present
            if let Some(state) = sessions.get_mut(req_session_id) {
                if state.history.is_empty() {
                    state.history = history;
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
                        frozen: None,
                        recall_items: Vec::new(),
                        agent_pool: peri_acp::session::agent_pool::AgentPool::new(),
                        workflow_middleware: None,
                    },
                );
            }

            // ── Freeze session data at load time ──
            cfg.session_manager.ensure_session(req_session_id, cwd);
            let frozen_data = cfg.session_manager.build_frozen_data(
                cwd,
                &cfg.plugin_skill_roots,
                &cfg.plugin_agent_dirs,
            );
            if let Some(s) = sessions.get_mut(req_session_id) {
                s.frozen = Some(frozen_data);
            }

            // ── ACP v1 spec: replay history via session/update BEFORE responding ──
            let history_for_replay: Vec<_> = sessions
                .get(req_session_id)
                .map(|s| s.history.clone())
                .unwrap_or_default();
            let replay_sender = TuiReplaySender { transport };
            if let Err(e) = dispatch::replay_session_history(
                req_session_id,
                &history_for_replay,
                &replay_sender,
            )
            .await
            {
                tracing::warn!(session_id = %req_session_id, error = %e, "session/load: history replay failed, continuing");
            }

            // modes/configOptions sent both via notification AND in response body
            // (notification for async update, response body for immediate availability)
            send_config_option_update(transport, req_session_id, cfg).await;

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
            send_available_commands_update(transport, req_session_id, &skills).await;
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
            let run_id = params
                .get("runId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing runId"))?;
            let agent_id = params
                .get("agentId")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| AcpError::new(-32602, "missing agentId"))?;

            let killed = if let Some(mw) = sessions
                .values()
                .find_map(|s| s.workflow_middleware.as_ref())
            {
                mw.runner().kill_agent(run_id, agent_id).await
            } else {
                false
            };

            if killed {
                info!(run_id, agent_id, "Workflow agent killed via ACP");
            } else {
                warn!(run_id, agent_id, "Workflow agent kill failed (not found)");
            }
            Ok(serde_json::json!({ "killed": killed }))
        }

        "workflow/kill_run" => {
            let run_id = params
                .get("runId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing runId"))?;

            let killed = if let Some(mw) = sessions
                .values()
                .find_map(|s| s.workflow_middleware.as_ref())
            {
                mw.registry().kill(run_id).is_ok()
            } else {
                false
            };

            if killed {
                info!(run_id, "Workflow run killed via ACP");
            } else {
                warn!(run_id, "Workflow run kill failed (not found)");
            }
            Ok(serde_json::json!({ "killed": killed }))
        }

        "workflow/resume" => {
            let run_id = params
                .get("runId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AcpError::new(-32602, "missing runId"))?;

            let mw = sessions
                .values()
                .find_map(|s| s.workflow_middleware.as_ref())
                .ok_or_else(|| AcpError::new(-32602, "no workflow middleware found"))?;

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

            if let Some(session) = cfg.session_manager.get_session(req_session_id) {
                session
                    .background_registry
                    .cancel(task_id)
                    .map_err(|e| AcpError::new(-32603, e.to_string()))?;
                info!(session_id = %req_session_id, task_id = %task_id, "Background task cancelled via ACP");
            }
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

            if !sessions.contains_key(req_session_id) {
                sessions.insert(
                    req_session_id.to_string(),
                    SessionState {
                        session_id: req_session_id.to_string(),
                        thread_id: req_session_id.to_string(),
                        cwd: cwd.to_string(),
                        history,
                        cancel_token: None,
                        frozen: None,
                        recall_items: Vec::new(),
                        agent_pool: peri_acp::session::agent_pool::AgentPool::new(),
                        workflow_middleware: None,
                    },
                );
                info!(session_id = %req_session_id, "Session resumed (new)");
            } else {
                // Existing session: if history still empty, populate from ThreadStore
                if let Some(s) = sessions.get_mut(req_session_id)
                    && s.history.is_empty()
                {
                    s.history = history;
                }
                info!(session_id = %req_session_id, "Session resumed (existing)");
            }

            // ── Freeze session data at resume time ──
            cfg.session_manager.ensure_session(req_session_id, cwd);
            let frozen_data = cfg.session_manager.build_frozen_data(
                cwd,
                &cfg.plugin_skill_roots,
                &cfg.plugin_agent_dirs,
            );
            if let Some(s) = sessions.get_mut(req_session_id) {
                s.frozen = Some(frozen_data);
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
            sessions.insert(
                new_session_id.clone(),
                SessionState {
                    session_id: new_session_id.clone(),
                    thread_id: new_thread_id.clone(),
                    cwd: cwd.to_string(),
                    history: copied_history,
                    cancel_token: None,
                    frozen: None,
                    recall_items: Vec::new(),
                    agent_pool: peri_acp::session::agent_pool::AgentPool::new(),
                    workflow_middleware: None,
                },
            );

            // ── Freeze session data at fork time ──
            cfg.session_manager.ensure_session(&new_session_id, cwd);
            let frozen_data = cfg.session_manager.build_frozen_data(
                cwd,
                &cfg.plugin_skill_roots,
                &cfg.plugin_agent_dirs,
            );
            if let Some(s) = sessions.get_mut(&new_session_id) {
                s.frozen = Some(frozen_data);
            }

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
            let active_pid = new_cfg.config.active_provider_id.as_str();
            if !active_pid.is_empty()
                && !new_cfg.config.providers.iter().any(|p| p.id == active_pid)
            {
                return Err(AcpError::new(
                    -32602,
                    format!("active_provider_id '{active_pid}' not found"),
                ));
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
                tracing::warn!(
                    active_provider = %new_cfg.config.active_provider_id,
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
            send_config_option_update(transport, session_id, cfg).await;
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
                        transport, session_id, "install", name, true, None,
                    )
                    .await;
                    let _ = push_plugin_snapshot(transport, session_id, &claude_dir).await;
                    Ok(serde_json::json!({ "success": true, "plugin": installed.id }))
                }
                Err(e) => {
                    let _ = push_plugin_action_result(
                        transport,
                        session_id,
                        "install",
                        name,
                        false,
                        Some(&e.to_string()),
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

            match peri_middlewares::plugin::uninstall_plugin(plugin_id, &claude_dir, None).await {
                Ok(()) => {
                    let _ = push_plugin_action_result(
                        transport,
                        session_id,
                        "uninstall",
                        plugin_id,
                        true,
                        None,
                    )
                    .await;
                    let _ = push_plugin_snapshot(transport, session_id, &claude_dir).await;
                    Ok(serde_json::json!({ "success": true }))
                }
                Err(e) => {
                    let _ = push_plugin_action_result(
                        transport,
                        session_id,
                        "uninstall",
                        plugin_id,
                        false,
                        Some(&e.to_string()),
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

            match result {
                Ok(()) => {
                    let action = if enable { "enable" } else { "disable" };
                    let _ = push_plugin_action_result(
                        transport, session_id, action, plugin_id, true, None,
                    )
                    .await;
                    let _ = push_plugin_snapshot(transport, session_id, &claude_dir).await;
                    Ok(serde_json::json!({ "success": true }))
                }
                Err(e) => {
                    let action = if enable { "enable" } else { "disable" };
                    let _ = push_plugin_action_result(
                        transport,
                        session_id,
                        action,
                        plugin_id,
                        false,
                        Some(&e.to_string()),
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

            let _ = push_plugin_search_result(transport, session_id, query, &results).await;
            Ok(serde_json::json!({ "results": results.iter().map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "version": r.version,
                    "description": r.description,
                    "marketplace": r.marketplace,
                })
            }).collect::<Vec<_>>() }))
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
) {
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
) {
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
) {
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
    let loaded = peri_middlewares::plugin::load_enabled_plugins_aggregated(claude_dir);

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
