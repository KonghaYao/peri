//! Prompt 执行管线：executor → 持久化 → 响应。

use std::sync::Arc;

use agent_client_protocol::{
    schema::{PromptResponse, SessionId, SessionInfoUpdate, SessionUpdate, StopReason},
    Client, ConnectionTo, Responder,
};
use peri_acp::session::{event_sink::StdioEventSink, executor};
use peri_agent::{agent::AgentCancellationToken, messages::MessageContent};

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
    pub history: Vec<peri_agent::messages::BaseMessage>,
    pub is_empty_history: bool,
    pub history_len: usize,
    pub cancel: AgentCancellationToken,
    pub pool: Arc<parking_lot::Mutex<peri_acp::session::agent_pool::AgentPool>>,
    pub thread_id: String,
    pub responder: Responder<PromptResponse>,
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
        is_empty_history,
        history_len,
        cancel,
        pool,
        thread_id,
        responder,
    } = params;

    let broker: Arc<dyn peri_agent::interaction::UserInteractionBroker> =
        Arc::new(super::super::context::StdioBroker::new());

    let event_sink = Arc::new(StdioEventSink::new(cx.clone(), session_id.clone()));
    let event_sink_for_notif = Arc::clone(&event_sink);

    // Snapshot provider / config (release guards before await).
    let provider_snapshot = ctx.provider.read().clone();
    let peri_config_snapshot = Arc::new(ctx.peri_config.read().clone());

    let result = executor::execute_prompt(executor::PromptExecutionContext {
        provider: provider_snapshot,
        peri_config: peri_config_snapshot,
        cwd: agent_cwd,
        session_id: sid.clone(),
        cancel,
        event_sink,
        broker,
        permission_mode: ctx.permission_mode.clone(),
        content,
        frozen,
        history,
        incoming_recalls: vec![],
        is_empty_history,
        bg_results: vec![], // stdio 无后台任务
        plugin_skill_dirs: ctx.plugin_skill_dirs.clone(),
        plugin_agent_dirs: ctx.plugin_agent_dirs.clone(),
        hook_groups: ctx.hook_groups.clone(),
        cron_scheduler: Some(ctx.cron_scheduler.clone()),
        mcp_pool: ctx.mcp_pool.clone(),
        channel_state: ctx.channel_state.clone(),
        tool_search_index: ctx.tool_search_index.clone(),
        shared_tools: ctx.shared_tools.clone(),
        lsp_servers: ctx.plugin_lsp_servers.clone(),
        langfuse_session: ctx.langfuse_session.clone(),
        pool: pool.clone(),
        thread_store: Some(Arc::clone(&ctx.thread_store)),
        thread_id: Some(thread_id.clone()),
        session_manager: Some(ctx.session_manager.clone()),
    })
    .await;

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
