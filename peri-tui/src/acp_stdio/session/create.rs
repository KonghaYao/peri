//! Session 创建：new / load / resume / fork。

use agent_client_protocol::{
    schema::{
        ForkSessionRequest, ForkSessionResponse, LoadSessionRequest, LoadSessionResponse,
        NewSessionRequest, NewSessionResponse, ResumeSessionRequest, ResumeSessionResponse,
        SessionId,
    },
    Client, ConnectionTo, Responder,
};
use peri_acp::{dispatch, session::state_builders::build_mode_state};

use super::super::{
    commands,
    context::{SessionInfo, StdioContext},
    freeze,
};

/// session/new 处理器：创建 ThreadStore 线程、冻结系统提示词、返回模式/模型/配置选项。
pub(crate) async fn handle_new(
    ctx: &StdioContext,
    req: NewSessionRequest,
    responder: Responder<NewSessionResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let cwd_str = req.cwd.to_string_lossy().to_string();
    let cwd_for_skills = cwd_str.clone();
    let meta = peri_agent::thread::ThreadMeta::new(&cwd_str);
    let thread_id = match ctx.thread_store.create_thread(meta).await {
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
    ctx.session_manager.ensure_session(&sid, &cwd_str);
    let frozen_data = freeze::build(ctx, &cwd_str);

    {
        let mut sessions = ctx.sessions.write();
        sessions.insert(
            sid.clone(),
            SessionInfo {
                session_id: sid.clone(),
                thread_id: thread_id.clone(),
                cwd: cwd_str,
                history: Vec::new(),
                cancel_token: None,
                frozen: Some(frozen_data),
                agent_pool: peri_acp::session::agent_pool::AgentPool::new(),
            },
        );
    }
    tracing::info!(session_id = %sid, "ACP session created with ThreadStore");
    let modes = build_mode_state(&ctx.permission_mode);
    let config_options = {
        let c = ctx.peri_config.read();
        let p = ctx.provider.read();
        dispatch::config_update::make_config_options(&c, &p, ctx.permission_mode.load())
    };
    let _ = responder.respond(
        NewSessionResponse::new(SessionId::new(&*sid))
            .modes(modes)
            .config_options(config_options),
    );
    // Push AvailableCommandsUpdate notification
    commands::send_available_commands(
        &cwd_for_skills,
        &ctx.plugin_skill_dirs,
        &SessionId::new(&*sid),
        &cx,
    );
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
    let cwd_for_skills = cwd.clone();

    // 登记到 SessionManager 以支撑 cascade cancel / goal_state
    ctx.session_manager.ensure_session(&sid, &cwd);
    // Build frozen data for session
    let frozen_data = freeze::build(ctx, &cwd);

    // Load history from ThreadStore via dispatch function
    let history = dispatch::load_session_messages(ctx.thread_store.as_ref(), &sid).await;

    // Insert into sessions if not already present
    {
        let mut sessions = ctx.sessions.write();
        if let Some(s) = sessions.get_mut(&sid) {
            if s.history.is_empty() {
                s.history = history;
            }
        } else {
            sessions.insert(
                sid.clone(),
                SessionInfo {
                    session_id: sid.clone(),
                    thread_id: sid.clone(),
                    cwd,
                    history,
                    cancel_token: None,
                    frozen: Some(frozen_data),
                    agent_pool: peri_acp::session::agent_pool::AgentPool::new(),
                },
            );
        }
    }

    let modes = build_mode_state(&ctx.permission_mode);
    let config_options = {
        let c = ctx.peri_config.read();
        let p = ctx.provider.read();
        dispatch::config_update::make_config_options(&c, &p, ctx.permission_mode.load())
    };
    let resp = LoadSessionResponse::new()
        .modes(modes)
        .config_options(config_options);
    let _ = responder.respond(resp);

    // Scan skills for AvailableCommands notification
    commands::send_available_commands(
        &cwd_for_skills,
        &ctx.plugin_skill_dirs,
        &SessionId::new(&*sid),
        &cx,
    );
    Ok(())
}

/// session/resume 处理器：按需注入新的冻结数据到已有或新会话。
pub(crate) async fn handle_resume(
    ctx: &StdioContext,
    req: ResumeSessionRequest,
    responder: Responder<ResumeSessionResponse>,
    _cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let sid = req.session_id.0.to_string();
    let cwd = req.cwd.to_string_lossy().to_string();
    // 登记到 SessionManager 以支撑 cascade cancel / goal_state
    ctx.session_manager.ensure_session(&sid, &cwd);
    // Build frozen data for session
    let frozen_data = freeze::build(ctx, &cwd);
    let mut sessions = ctx.sessions.write();
    if !sessions.contains_key(&sid) {
        sessions.insert(
            sid.clone(),
            SessionInfo {
                session_id: sid.clone(),
                thread_id: sid.clone(),
                cwd,
                history: Vec::new(),
                cancel_token: None,
                frozen: Some(frozen_data),
                agent_pool: peri_acp::session::agent_pool::AgentPool::new(),
            },
        );
        tracing::info!(session_id = %sid, "Session resumed (new)");
    } else {
        tracing::info!(session_id = %sid, "Session resumed (existing)");
    }
    let _ = responder.respond(ResumeSessionResponse::new());
    Ok(())
}

/// session/fork 处理器：从源会话复制历史到新 ThreadStore 线程。
pub(crate) async fn handle_fork(
    ctx: &StdioContext,
    req: ForkSessionRequest,
    responder: Responder<ForkSessionResponse>,
    _cx: ConnectionTo<Client>,
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

    // Fork via dispatch function
    let (new_thread_id, copied_history) = match dispatch::fork_session(
        ctx.thread_store.as_ref(),
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
    ctx.session_manager
        .ensure_session(&new_session_id, &cwd_str);
    // Build frozen data for forked session
    let frozen_data = freeze::build(ctx, &cwd_str);
    {
        let mut sessions = ctx.sessions.write();
        sessions.insert(
            new_session_id.clone(),
            SessionInfo {
                session_id: new_session_id.clone(),
                thread_id: new_thread_id.clone(),
                cwd: cwd_str,
                history: copied_history,
                cancel_token: None,
                frozen: Some(frozen_data),
                agent_pool: peri_acp::session::agent_pool::AgentPool::new(),
            },
        );
    }

    let resp = ForkSessionResponse::new(SessionId::new(new_session_id));
    let _ = responder.respond(resp);
    Ok(())
}
