//! ACP Prompt execution — builds and executes the agent via crate::executor.
//! Extracted from original acp_server.rs (2026-05-20 split).

use std::{collections::BTreeMap, sync::Arc};

use crate::{
    broker::AcpTransportBroker,
    session::{event_sink::TransportEventSink, executor},
    transport::types::AcpError,
};
use agent_client_protocol::schema::v1::{PromptResponse, StopReason};
use parking_lot::RwLock;
use peri_agent::{agent::AgentCancellationToken, interaction::ChannelState};
use peri_controller::langfuse::LangfuseSession;
use peri_middlewares::prelude::*;
use serde_json::Value;
use tracing::info;

use super::SharedSessions;
use crate::provider::{LlmProvider, PeriConfig};

// ── Prompt execution (spawned into background task) ──────────────────────────

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_prompt(
    params: Value,
    sessions: &SharedSessions,
    provider: &Arc<RwLock<LlmProvider>>,
    peri_config: &Arc<RwLock<PeriConfig>>,
    permission_mode: &Arc<SharedPermissionMode>,
    cron_scheduler: Option<Arc<parking_lot::Mutex<CronScheduler>>>,
    plugin_skill_roots: &[peri_middlewares::skills::SkillRoot],
    plugin_agent_dirs: &[std::path::PathBuf],
    plugin_loaded: &[peri_middlewares::plugin::LoadedPlugin],
    hook_groups: &[Vec<peri_middlewares::hooks::RegisteredHook>],
    mcp_pool: Option<Arc<peri_middlewares::mcp::McpClientPool>>,
    channel_state: Option<Arc<ChannelState>>,
    tool_search_index: Arc<peri_middlewares::tool_search::ToolSearchIndex>,
    shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn peri_agent::tools::BaseTool>>>>,
    plugin_lsp_servers: &[peri_lsp::config::LspServerConfig],
    transport: &Arc<dyn crate::transport::AcpTransport>,
    thread_store: &Arc<dyn peri_agent::thread::ThreadStore>,
    langfuse_session: Option<Arc<LangfuseSession>>,
    pool: Arc<parking_lot::Mutex<crate::session::agent_pool::AgentPool>>,
    session_manager: crate::session::SessionManager,
    // 内部 continuation 通知通道（注入 SessionContext，供 on_bg_complete
    // 闭包通知 server 的 continuation scheduler）。stdio 等无 scheduler 场景为 None。
    cont_tx: Option<
        tokio::sync::mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>,
    >,
    // 内部 AsyncContinuation（bg 完成唤醒被取消的 turn）：不 push 空 user
    // prompt、不触发 keepgoing 语义。仅由 continuation scheduler 调用。
    continuation: bool,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();
    // v2 路径下 MessageQueue 由 run_session_loop 从 session_manager.v2_message_queue
    // 解析（executor.rs:368），不再作为 PromptExecutionContext 字段传入。
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

    // Issue 2026-08-05 返工：requestId 透传——TUI 提交时生成、随 prompt RPC 到达，
    // 服务器随 turn 结束事件（peri/agent_event_done）原样回带，供 TUI 侧 stale
    // TurnInterrupted 的 request_id 配对判定。缺失路径（stdio / continuation 等）为 None。
    let request_id = params
        .get("requestId")
        .and_then(|v| v.as_str())
        .map(String::from);

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
    let (cwd, history, is_empty, thread_id, frozen, incoming_recalls, workflow_middleware) = {
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
            // [AsyncContinuation] 续跑不 take recall：上一轮留给用户 prompt 的
            // recall 必须保留在 SessionState（后续用户 prompt 注入），续跑自身
            // 也不注入（见 executor::run_session_loop 的 continuation 分支）。
            take_recall_for_turn(&mut state.recall_items, continuation),
            state.workflow_middleware.clone(),
        )
    };
    let history_len = history.len();
    // Save message IDs for compact persistence path (history is moved into run_session_loop below).
    let history_ids: Vec<peri_agent::messages::MessageId> =
        history.iter().map(|m| m.id()).collect();

    let broker: Arc<dyn peri_agent::interaction::UserInteractionBroker> = Arc::new(
        AcpTransportBroker::new(Arc::clone(transport), session_id.clone().into()),
    );
    let event_sink = Arc::new(TransportEventSink::new(
        Arc::clone(transport),
        session_manager.caps_registry(),
    ));

    let provider_snapshot = provider.read().clone();
    let peri_config_snapshot = Arc::new(peri_config.read().clone());

    // Create workflow executor (enables Workflow tool for multi-agent orchestration)
    // GAP-05: inject frozen data so workflow agents reuse SubAgent infra
    let workflow_executor = crate::agent::workflow_agent::create_executor(
        crate::agent::workflow_agent::WorkflowAgentContext {
            provider: Arc::clone(provider),
            cwd: cwd.clone(),
            frozen_claude_md: frozen
                .as_ref()
                .and_then(|f| f.claude_md().map(|s| s.to_string())),
            frozen_claude_local_md: frozen
                .as_ref()
                .and_then(|f| f.claude_local_md().map(|s| s.to_string())),
            frozen_skill_summary: frozen
                .as_ref()
                .and_then(|f| f.skill_summary().map(|s| s.to_string())),
            session_id: Some(session_id.clone()),
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
        },
    );

    // Track first history message ID for cancel-with-progress path (history is moved below)
    // Uses Option<MessageId> (16 bytes) instead of cloning the entire history.
    let first_history_id = history.first().map(|m| m.id());
    let ctx = executor::SessionContext {
        provider: provider_snapshot,
        peri_config: peri_config_snapshot,
        cwd,
        session_id: session_id.clone(),
        cancel,
        broker,
        permission_mode: permission_mode.clone(),
        plugin_skill_roots: plugin_skill_roots.to_vec(),
        plugin_agent_dirs: plugin_agent_dirs.to_vec(),
        plugin_loaded: plugin_loaded.to_vec(),
        hook_groups: hook_groups.to_vec(),
        cron_scheduler,
        mcp_pool,
        channel_state,
        tool_search_index,
        shared_tools,
        lsp_servers: plugin_lsp_servers.to_vec(),
        pool,
        thread_store: Some(Arc::clone(thread_store)),
        thread_id: Some(thread_id.clone()),
        session_manager: Some(session_manager),
        workflow_executor: Some(workflow_executor),
        workflow_middleware,
        session_start_source: if !continuation && is_empty {
            Some("startup".to_string())
        } else {
            None
        },
        request_id,
        allow_await_wake: true,
        continuation_notify: cont_tx,
    };
    let turn = executor::TurnInput {
        event_sink,
        content,
        continuation,
        frozen,
        history,
        incoming_recalls,
        bg_results,
        langfuse_session,
    };
    let result = executor::run_session_loop(ctx, turn).await;

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
            } else if result.history_replaced_by_compaction
                || result.messages.len() > history_len + 1
            {
                // Error/cancel but agent made progress (user msg + AI/tool messages beyond
                // just the user message). Preserve history so the agent remembers the
                // interrupted round's context on the next prompt. Covers all error paths:
                // LLM stream errors, HTTP errors, tool failures, middleware errors,
                // MaxIterationsExceeded, and Ctrl+C cancel.
                //
                // NOTE: execute() skips cleanup_prepended on error paths (? propagation),
                // so result.messages may contain leaked system prepends at the beginning.
                // A committed Full Compact intentionally removes prior visible IDs and appends
                // its summary to ThreadStore. Only that explicit executor signal may replace
                // history without its original first message; other partial results are rejected.
                if let Some(cleaned) = strip_leaked_prepends(
                    &result.messages,
                    first_history_id,
                    result.history_replaced_by_compaction,
                ) {
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
                    tracing::warn!(
                        session_id = %session_id,
                        history_len,
                        result_messages = result.messages.len(),
                        "Cancelled result omitted existing history; preserving prior in-memory history"
                    );
                }
            } else {
                // Execution failed, cancelled early (no AI output), or MaxIterationsExceeded.
                // Roll back LLM-side history to pre-submit state.
                // The TUI's TurnInterrupted handler detects zero AI output (current_turn empty)
                // and performs the corresponding UI rollback: removes the user bubble from
                // committed + restores text to the input area via INPUT_RESTORE_TEXT storage
                // + RENDER_HEARTBEAT trigger.
                state.history.truncate(history_len);
                info!(session_id = %session_id, history_len, "Agent execution failed/cancelled, rolled back history");
            }
            // [AsyncContinuation] 续跑结束不回写 recall：保留续跑开始前
            // SessionState 中的 recall（上一轮留给用户 prompt 的），续跑自身
            // 产生的 recall 不覆盖它；用户 prompt 正常回写。
            if recall_overwrite_allowed(continuation) {
                state.recall_items = result.recall_items;
            }
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

/// [AsyncContinuation] 读取本轮 recall 的策略：
///
/// - 用户 prompt：`mem::take`（消费上一轮 recall 注入本轮的 system-reminder，
///   结束时由 `recall_overwrite_allowed` 回写新 recall）；
/// - 内部续跑：**clone 而非 take**——上一轮留给用户 prompt 的 recall 必须
///   保留在 `SessionState`（续跑自身也不注入，见 executor 的 continuation
///   分支），避免续跑"吞掉"用户 prompt 应得的 recall。
fn take_recall_for_turn(recall_items: &mut Vec<String>, continuation: bool) -> Vec<String> {
    if continuation {
        recall_items.clone()
    } else {
        std::mem::take(recall_items)
    }
}

/// 本轮结束时是否允许用 `result.recall_items` 覆盖 `SessionState.recall_items`。
///
/// 续跑结束时**不改变** SessionState 中的 recall（保留续跑开始前的值给后续
/// 用户 prompt）；用户 prompt 正常回写本轮产生的 recall。
fn recall_overwrite_allowed(continuation: bool) -> bool {
    !continuation
}

/// Returns `None` when a partial result omits existing history. A committed Full Compact
/// explicitly replaces prior visible messages with its persisted summary, so it is accepted.
fn strip_leaked_prepends(
    result_messages: &[peri_agent::messages::BaseMessage],
    first_history_id: Option<peri_agent::messages::MessageId>,
    full_compaction_committed: bool,
) -> Option<Vec<peri_agent::messages::BaseMessage>> {
    match first_history_id {
        Some(first_id) => {
            // Find where original history starts in result (skip leaked prepends).
            if let Some(start) = result_messages.iter().position(|m| m.id() == first_id) {
                Some(result_messages[start..].to_vec())
            } else if full_compaction_committed {
                Some(
                    result_messages
                        .iter()
                        .skip_while(|m| m.is_system())
                        .cloned()
                        .collect(),
                )
            } else {
                None
            }
        }
        None => {
            // Original history was empty — strip leading system messages (all prepends).
            Some(
                result_messages
                    .iter()
                    .skip_while(|m| m.is_system())
                    .cloned()
                    .collect(),
            )
        }
    }
}

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;
