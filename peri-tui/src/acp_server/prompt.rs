//! ACP Prompt execution — builds and executes the agent via peri_acp::executor.
//! Extracted from original acp_server.rs (2026-05-20 split).

use std::{collections::HashMap, sync::Arc};

use agent_client_protocol::schema::{PromptResponse, StopReason};
use parking_lot::RwLock;
use peri_acp::{
    broker::AcpTransportBroker,
    langfuse::LangfuseSession,
    session::{event_sink::TransportEventSink, executor},
    transport::types::AcpError,
};
use peri_agent::{agent::AgentCancellationToken, interaction::ChannelState};
use peri_middlewares::prelude::*;
use serde_json::Value;
use tracing::info;

use super::SharedSessions;
use crate::{app::agent::LlmProvider, config::PeriConfig};

// ── Prompt execution (spawned into background task) ──────────────────────────

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_prompt(
    params: Value,
    sessions: &SharedSessions,
    provider: &Arc<RwLock<LlmProvider>>,
    peri_config: &Arc<RwLock<PeriConfig>>,
    permission_mode: &Arc<SharedPermissionMode>,
    cron_scheduler: Option<Arc<parking_lot::Mutex<CronScheduler>>>,
    plugin_skill_dirs: &[std::path::PathBuf],
    plugin_agent_dirs: &[std::path::PathBuf],
    hook_groups: &[Vec<peri_middlewares::hooks::RegisteredHook>],
    mcp_pool: Option<Arc<peri_middlewares::mcp::McpClientPool>>,
    channel_state: Option<Arc<ChannelState>>,
    tool_search_index: Arc<peri_middlewares::tool_search::ToolSearchIndex>,
    shared_tools: Arc<RwLock<HashMap<String, Arc<dyn peri_agent::tools::BaseTool>>>>,
    plugin_lsp_servers: &[peri_lsp::config::LspServerConfig],
    transport: &Arc<dyn peri_acp::transport::AcpTransport>,
    thread_store: &Arc<dyn peri_agent::thread::ThreadStore>,
    langfuse_session: Option<Arc<LangfuseSession>>,
    pool: Arc<parking_lot::Mutex<peri_acp::session::agent_pool::AgentPool>>,
    session_manager: peri_acp::session::SessionManager,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();
    let message = params
        .get("message")
        .ok_or_else(|| AcpError::new(-32602, "missing message"))?;
    let content: peri_agent::messages::MessageContent = message
        .get("content")
        .map(|v| serde_json::from_value(v.clone()).unwrap_or_default())
        .unwrap_or_default();

    // Parse optional background task results for synthetic tool_use + tool_result injection
    let bg_results: Vec<peri_agent::agent::events::BackgroundTaskResult> = params
        .get("bgResults")
        .map(|v| serde_json::from_value(v.clone()).unwrap_or_default())
        .unwrap_or_default();

    // Create cancel token and register in sessions.
    let cancel = AgentCancellationToken::new();
    {
        let mut sessions = sessions.lock().await;
        let state = sessions
            .get_mut(&session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        state.cancel_token = Some(cancel.clone());
    }

    // Read session data under lock, then release immediately.
    let (cwd, history, is_empty, thread_id, frozen, incoming_recalls) = {
        let mut sessions = sessions.lock().await;
        let state = sessions
            .get_mut(&session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        (
            state.cwd.clone(),
            state.history.clone(),
            state.history.is_empty(),
            state.thread_id.clone(),
            state.frozen.clone(),
            std::mem::take(&mut state.recall_items),
        )
    };
    let history_len = history.len();
    // Save message IDs for compact persistence path (history is moved into execute_prompt below).
    let history_ids: Vec<peri_agent::messages::MessageId> =
        history.iter().map(|m| m.id()).collect();

    let broker: Arc<dyn peri_agent::interaction::UserInteractionBroker> = Arc::new(
        AcpTransportBroker::new(Arc::clone(transport), session_id.clone().into()),
    );
    let event_sink = Arc::new(TransportEventSink::new(Arc::clone(transport)));

    let provider_snapshot = provider.read().clone();
    let peri_config_snapshot = Arc::new(peri_config.read().clone());

    // Track first history message ID for cancel-with-progress path (history is moved below)
    // Uses Option<MessageId> (16 bytes) instead of cloning the entire history.
    let first_history_id = history.first().map(|m| m.id());
    let result = executor::execute_prompt(executor::PromptExecutionContext {
        provider: provider_snapshot,
        peri_config: peri_config_snapshot,
        cwd,
        session_id: session_id.clone(),
        cancel,
        event_sink,
        broker,
        permission_mode: permission_mode.clone(),
        content,
        frozen,
        history,
        incoming_recalls,
        session_start_source: if is_empty {
            Some("startup".to_string())
        } else {
            None
        },
        bg_results,
        plugin_skill_dirs: plugin_skill_dirs.to_vec(),
        plugin_agent_dirs: plugin_agent_dirs.to_vec(),
        hook_groups: hook_groups.to_vec(),
        cron_scheduler,
        mcp_pool,
        channel_state,
        tool_search_index,
        shared_tools,
        lsp_servers: plugin_lsp_servers.to_vec(),
        langfuse_session,
        pool,
        thread_store: Some(Arc::clone(thread_store)),
        thread_id: Some(thread_id.clone()),
        session_manager: Some(session_manager),
    })
    .await;

    // Persist new messages to ThreadStore and update in-memory state.
    {
        let mut sessions = sessions.lock().await;
        if let Some(state) = sessions.get_mut(&session_id) {
            if result.ok {
                info!(session_id = %session_id, messages = result.messages.len(), "Agent execution completed");
                // Persist only the newly added messages.
                if history_len < result.messages.len() {
                    let new_msgs = &result.messages[history_len..];
                    if let Err(e) = thread_store.append_messages(&thread_id, new_msgs).await {
                        tracing::warn!(error = %e, "Failed to persist messages to ThreadStore");
                    }
                } else if result.messages.len() < history_len {
                    // Compact replaced own messages with a condensed summary.
                    // Delete old messages from ThreadStore and persist compacted state,
                    // otherwise session restore loads old + new messages causing duplication.
                    info!(
                        session_id = %session_id,
                        old_count = history_len,
                        new_count = result.messages.len(),
                        "Compact detected: updating ThreadStore"
                    );
                    if let Err(e) = thread_store.delete_messages(&thread_id, &history_ids).await {
                        tracing::warn!(
                            error = %e,
                            "Failed to delete pre-compact messages from ThreadStore"
                        );
                    }
                    if let Err(e) = thread_store
                        .append_messages(&thread_id, &result.messages)
                        .await
                    {
                        tracing::warn!(
                            error = %e,
                            "Failed to persist compacted messages to ThreadStore"
                        );
                    }
                }
                state.history = result.messages;
            } else if result.messages.len() > history_len + 1 {
                // Error/cancel but agent made progress (user msg + AI/tool messages beyond
                // just the user message). Preserve history so the agent remembers the
                // interrupted round's context on the next prompt. Covers all error paths:
                // LLM stream errors, HTTP errors, tool failures, middleware errors,
                // MaxIterationsExceeded, and Ctrl+C cancel.
                //
                // NOTE: execute() skips cleanup_prepended on error paths (? propagation),
                // so result.messages may contain leaked system prepends at the beginning.
                // Strip them by locating where the original history starts (ID matching).
                let cleaned = strip_leaked_prepends(&result.messages, first_history_id);
                let new_count = cleaned.len().saturating_sub(history_len);
                // Persist newly added messages to ThreadStore
                if new_count > 0 && history_len < cleaned.len() {
                    let new_msgs = &cleaned[history_len..];
                    if let Err(e) = thread_store.append_messages(&thread_id, new_msgs).await {
                        tracing::warn!(error = %e, "Failed to persist cancelled-round messages");
                    }
                }
                state.history = cleaned;
                info!(
                    session_id = %session_id,
                    history_len,
                    new_count,
                    "Agent cancelled with progress, preserving history"
                );
            } else {
                // Execution failed, cancelled early (no tool calls), or MaxIterationsExceeded.
                // Roll back to pre-submit state — the TUI's handle_interrupted will also
                // truncate view_messages and restore text to input for the no-tool-call case.
                state.history.truncate(history_len);
                info!(session_id = %session_id, history_len, "Agent execution failed/cancelled, rolled back history");
            }
            state.recall_items = result.recall_items;
            state.cancel_token = None;
        }
    }

    let acp_stop_reason = match result.stop_reason {
        executor::PromptStopReason::Cancelled => StopReason::Cancelled,
        executor::PromptStopReason::MaxTurnRequests => StopReason::MaxTurnRequests,
        executor::PromptStopReason::EndTurn => StopReason::EndTurn,
    };
    let resp = PromptResponse::new(acp_stop_reason);
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

/// Strip leaked system prepend messages from the agent's result messages.
///
/// When `execute()` returns via `?` propagation (cancel/error), `cleanup_prepended` is
/// not called, leaving system messages (from `before_agent` + `with_system_prompt`) at
/// the head of the message list. This function finds where the original history begins
/// (by matching the first message ID) and returns messages from that point onward.
fn strip_leaked_prepends(
    result_messages: &[peri_agent::messages::BaseMessage],
    first_history_id: Option<peri_agent::messages::MessageId>,
) -> Vec<peri_agent::messages::BaseMessage> {
    match first_history_id {
        Some(first_id) => {
            // Find where original history starts in result (skip leaked prepends)
            match result_messages.iter().position(|m| m.id() == first_id) {
                Some(start) => result_messages[start..].to_vec(),
                None => {
                    // Original history not found — compact may have replaced messages.
                    // Return as-is (no stripping).
                    result_messages.to_vec()
                }
            }
        }
        None => {
            // Original history was empty — strip leading system messages (all prepends)
            result_messages
                .iter()
                .skip_while(|m| m.is_system())
                .cloned()
                .collect()
        }
    }
}

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;
