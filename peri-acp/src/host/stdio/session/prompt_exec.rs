//! Prompt 执行管线：executor → 持久化 → 响应。

use std::sync::Arc;

use crate::session::{event_sink::StdioEventSink, executor};
use agent_client_protocol::{
    schema::v1::{PromptResponse, SessionId, SessionInfoUpdate, SessionUpdate, StopReason},
    Client, ConnectionTo, Responder,
};
use peri_acp_types::messages::MessageContent;
use peri_acp_types::PeriCaps;
use tokio_util::sync::CancellationToken;

use super::super::context::StdioContext;

/// Prompt 执行的完整参数集合。
pub(crate) struct PromptExecParams {
    pub ctx: Arc<StdioContext>,
    pub cx: ConnectionTo<Client>,
    pub session_id: SessionId,
    pub sid: String,
    pub agent_cwd: String,
    pub content: MessageContent,
    pub frozen: Option<executor::FrozenSessionData>,
    pub history: Vec<peri_acp_types::messages::BaseMessage>,
    pub session_start_source: Option<String>,
    pub history_len: usize,
    pub cancel: CancellationToken,
    pub pool: Arc<parking_lot::Mutex<crate::session::agent_pool::AgentPool>>,
    pub thread_id: String,
    pub responder: Responder<PromptResponse>,
    pub peri_caps: PeriCaps,
}

/// 执行 agent 管线：executor → pool 恢复 → 持久化 → 内存更新 → 响应。
pub(crate) async fn run(params: PromptExecParams) {
    let PromptExecParams {
        ctx,
        cx,
        session_id,
        sid,
        agent_cwd,
        content,
        frozen,
        history,
        session_start_source,
        history_len,
        cancel,
        pool,
        thread_id,
        responder,
        peri_caps,
    } = params;

    let broker: Arc<dyn peri_acp_types::interaction::UserInteractionBroker> =
        Arc::new(super::super::context::StdioBroker::new());

    let event_sink = Arc::new(StdioEventSink::new(
        cx.clone(),
        session_id.clone(),
        peri_caps,
    ));
    let event_sink_for_notif = Arc::clone(&event_sink);

    // Snapshot provider / config (release guards before await).
    let provider_snapshot = ctx.provider.read().clone();
    let peri_config_snapshot = Arc::new(ctx.peri_config.read().clone());

    // Create workflow executor (enables Workflow tool for multi-agent orchestration)
    // GAP-05: inject frozen data so workflow agents reuse SubAgent infra
    let workflow_executor = crate::agent::workflow_agent::create_executor(
        crate::agent::workflow_agent::WorkflowAgentContext {
            provider: Arc::clone(&ctx.provider),
            cwd: agent_cwd.clone(),
            frozen_claude_md: frozen
                .as_ref()
                .and_then(|f| f.claude_md().map(|s| s.to_string())),
            frozen_claude_local_md: frozen
                .as_ref()
                .and_then(|f| f.claude_local_md().map(|s| s.to_string())),
            frozen_skill_summary: frozen
                .as_ref()
                .and_then(|f| f.skill_summary().map(|s| s.to_string())),
            session_id: Some(sid.clone()),
            compact_config: {
                let mut cc = peri_config_snapshot
                    .config
                    .compact
                    .clone()
                    .unwrap_or_default();
                cc.apply_env_overrides();
                Some(cc)
            },
            cancel: Some(cancel.clone()),
            // 无 16_workflow 版本（P2-2026-08-02）：workflow agent 链不
            // 注册 WorkflowTool，不得复用带 workflow 声明的主 prompt。
            system_prompt: frozen
                .as_ref()
                .map(|f| f.subagent_system_prompt().to_string()),
            broker: None,
            permission_mode: None,
            frozen_date: frozen.as_ref().map(|f| f.date().to_string()),
            frozen_language: frozen
                .as_ref()
                .and_then(|f| f.language().map(|s| s.to_string())),
            agent_pool: None,
            langfuse_session: None,
            thread_store: None,
            peri_config: Some(peri_config_snapshot.clone()),
            progress_tx: None,
            subagent_ctx_builder: None,
            controller: Some(Arc::clone(&ctx.controller)),
        },
    );

    // Read session-scoped workflow_middleware from SessionInfo
    let workflow_middleware = {
        let sessions = ctx.sessions.read();
        sessions
            .get(&sid)
            .and_then(|s| s.workflow_middleware.clone())
    };

    // v2 路径下 MessageQueue 由 run_session_loop 从 session_manager.v2_message_queue
    // 解析（executor.rs:368），不再作为 PromptExecutionContext 字段传入。

    let cx = executor::SessionContext {
        provider: provider_snapshot,
        peri_config: peri_config_snapshot,
        cwd: agent_cwd,
        session_id: sid.clone(),
        cancel,
        broker,
        permission_mode: ctx.permission_mode.clone(),
        plugin_skill_roots: ctx.plugin_skill_roots.clone(),
        plugin_agent_dirs: ctx.plugin_agent_dirs.clone(),
        plugin_loaded: ctx.plugin_loaded.clone(),
        hook_groups: ctx.hook_groups.clone(),
        cron_scheduler: Some(ctx.cron_scheduler.clone()),
        mcp_pool: ctx.mcp_pool.clone(),
        channel_state: ctx.channel_state.clone(),
        tool_search_index: ctx.tool_search_index.clone(),
        skills: ctx.skills.clone(),
        shared_tools: ctx.shared_tools.clone(),
        lsp_servers: ctx.plugin_lsp_servers.clone(),
        pool: pool.clone(),
        thread_store: Some(Arc::clone(&ctx.thread_store)),
        thread_id: Some(thread_id.clone()),
        session_manager: Some(ctx.session_manager.clone()),
        workflow_executor: Some(workflow_executor),
        workflow_middleware,
        controller: Arc::clone(&ctx.controller),
        session_start_source,
        request_id: None, // stdio 无 requestId 配对（TUI 专用）
        allow_await_wake: false,
        continuation_notify: None, // stdio 无 continuation scheduler
    };
    let turn = executor::TurnInput {
        event_sink,
        content,
        continuation: false,
        frozen,
        history,
        incoming_recalls: vec![],
        bg_results: vec![], // stdio 无后台任务
        langfuse_session: ctx.langfuse_session.clone(),
    };

    // 3.0 批 2：执行发起经 Controller（控制面第四步 run Session）。
    // 本轮执行句柄（PromptHandle）注册进 Runtime 映射 → run_session 发起 →
    // 返回时结果已就绪 → take_result。
    let handle = Arc::new(crate::host::exec::prompt_handle::PromptHandle::new(
        cx, turn,
    ));
    ctx.controller.register_session(&sid, Arc::clone(&handle));
    if let Err(e) = ctx.controller.run_session(&sid).await {
        tracing::error!(session_id = %sid, error = %e, "run_session failed");
        let _ = responder.respond(PromptResponse::new(StopReason::Cancelled));
        return;
    }
    let result = handle.take_result();

    // Restore AgentPool back into session
    if let Ok(mutex) = Arc::try_unwrap(pool) {
        let mut sessions = ctx.sessions.write();
        if let Some(s) = sessions.get_mut(&sid) {
            s.agent_pool = mutex.into_inner();
        }
    }

    // Persist new messages to ThreadStore.
    if result.ok && history_len < result.messages.len() {
        let new_msgs = &result.messages[history_len..];
        if let Err(e) = ctx.thread_store.append_messages(&thread_id, new_msgs).await {
            tracing::warn!(error = %e, "Failed to persist messages to ThreadStore");
        }
    }
    // Update in-memory state.
    {
        let mut sessions = ctx.sessions.write();
        if let Some(s) = sessions.get_mut(&sid) {
            s.history = result.messages;
            s.cancel_token = None;
        }
    }

    let acp_stop_reason = match result.stop_reason {
        executor::PromptStopReason::Cancelled => StopReason::Cancelled,
        executor::PromptStopReason::MaxTurnRequests => StopReason::MaxTurnRequests,
        executor::PromptStopReason::EndTurn => StopReason::EndTurn,
    };
    let _ = responder.respond(PromptResponse::new(acp_stop_reason));

    // Send SessionInfoUpdate after prompt completes.
    let info = SessionInfoUpdate::new().updated_at(chrono::Utc::now().to_rfc3339());
    event_sink_for_notif.send_update(SessionUpdate::SessionInfoUpdate(info));
}
