//! Agent event dispatch — the main `handle_agent_event` dispatcher routes
//! individual AgentEvent variants to specialized handlers.
//! Extracted sub-modules:
//!   acp_bridge.rs — ACP notification bridge
//!   lifecycle.rs — cleanup, done, interrupted, error
//!   subagent.rs  — token usage, subagent start
//!   polling.rs   — poll_agent, poll_background_events, poll_cron_triggers

use super::{agent_events_bg::BackgroundTaskResult, *};
mod acp_bridge;
mod lifecycle;
mod polling;
mod rewind;
mod subagent;

impl App {
    pub(crate) fn handle_agent_event(&mut self, event: AgentEvent) -> (bool, bool, bool) {
        match event {
            AgentEvent::SubAgentStart {
                agent_id,
                instance_id,
                task_preview,
                is_background,
            } => self.handle_subagent_start(agent_id, instance_id, task_preview, is_background),
            AgentEvent::SubagentLifecycle {
                agent_name,
                started,
            } => {
                if started {
                    // SubAgent 实际开始执行：更新 spinner 为工具使用模式
                    let verb = format!("Agent: {}", agent_name);
                    self.session_mgr
                        .current_mut()
                        .spinner_state
                        .set_mode(peri_widgets::SpinnerMode::ToolUse);
                    self.session_mgr
                        .current_mut()
                        .spinner_state
                        .set_verb(Some(&verb));
                } else {
                    // SubAgent 执行结束：恢复 spinner 为响应模式
                    self.session_mgr
                        .current_mut()
                        .spinner_state
                        .set_mode(peri_widgets::SpinnerMode::Responding);
                    self.session_mgr
                        .current_mut()
                        .spinner_state
                        .set_verb(Some("思考中…"));
                }
                // 触发 rebuild 刷新 SubAgentGroup 卡片显示
                self.request_rebuild();
                (true, false, false)
            }
            AgentEvent::SubAgentEnd {
                result,
                is_error,
                agent_id,
                instance_id,
            } => {
                self.session_mgr.current_mut().agent.subagent_depth = self
                    .session_mgr
                    .current_mut()
                    .agent
                    .subagent_depth
                    .saturating_sub(1);
                // 如果所有 SubAgent 已完成，恢复 spinner 到思考模式
                if self.session_mgr.current_mut().agent.subagent_depth == 0 {
                    self.session_mgr
                        .current_mut()
                        .spinner_state
                        .set_mode(peri_widgets::SpinnerMode::Responding);
                    self.session_mgr
                        .current_mut()
                        .spinner_state
                        .set_verb(Some("思考中…"));
                }
                // Pipeline：更新 SubAgentGroup（is_running=false, final_result）
                let actions = self
                    .session_mgr
                    .current_mut()
                    .messages
                    .pipeline
                    .handle_event(AgentEvent::SubAgentEnd {
                        result,
                        is_error,
                        agent_id,
                        instance_id,
                    });
                for action in actions {
                    self.apply_pipeline_action(action);
                }
                self.request_rebuild();
                (true, false, false)
            }
            AgentEvent::ContextWarning {
                used_tokens: _,
                total_tokens,
                percentage: _,
            } => {
                // 子 Agent 的 ContextWarning 不应触发父 Agent 的 auto-compact
                if self.session_mgr.current_mut().agent.subagent_depth > 0 {
                    return (true, false, false);
                }
                // 从核心层同步 context_window（核心层通过 model.context_window() 获取正确值）
                let cw = total_tokens as u32;
                if cw > 0 && self.session_mgr.current_mut().agent.context_window != cw {
                    tracing::debug!(
                        old = self.session_mgr.current_mut().agent.context_window,
                        new = cw,
                        "context_window updated from core layer"
                    );
                    self.session_mgr.current_mut().agent.context_window = cw;
                }
                (true, false, false)
            }
            AgentEvent::OAuthAuthorizationNeeded {
                server_name,
                authorization_url,
                callback_tx,
            } => self.handle_oauth_needed(server_name, authorization_url, callback_tx),
            AgentEvent::OAuthAuthorizationCompleted { server_name } => {
                self.handle_oauth_completed(server_name)
            }
            AgentEvent::OAuthAuthorizationFailed { server_name, error } => {
                self.handle_oauth_failed(server_name, error)
            }
            AgentEvent::McpActionCompleted {
                server_name,
                action,
                success,
            } => self.handle_mcp_action_completed(server_name, action, success),
            AgentEvent::PluginActionCompleted {
                plugin_id,
                action,
                success,
                message,
            } => self.handle_plugin_action_completed(plugin_id, action, success, message),
            AgentEvent::TokenUsageUpdate {
                usage,
                model: _model,
                stop_reason: _,
            } => self.handle_token_usage_update(usage),
            AgentEvent::ToolStart {
                tool_call_id,
                name,
                display,
                args,
                input,
                source_agent_id,
            } => {
                self.session_mgr.current_mut().agent.retry_status = None;
                self.session_mgr.current_mut().agent.agent_replied = true;
                self.session_mgr.current_mut().agent.tool_call_count += 1;
                // 跨切面：spinner
                self.session_mgr
                    .current_mut()
                    .spinner_state
                    .set_mode(peri_widgets::SpinnerMode::ToolUse);
                let verb_text = if !args.is_empty() {
                    let summary: String = args.chars().take(40).collect();
                    format!("{} {}", display, summary)
                } else {
                    format!("{}…", display)
                };
                self.session_mgr
                    .current_mut()
                    .spinner_state
                    .set_verb(Some(&verb_text));
                // Pipeline：创建 ToolBlock / 路由进 SubAgentGroup
                let actions = self
                    .session_mgr
                    .current_mut()
                    .messages
                    .pipeline
                    .handle_event(AgentEvent::ToolStart {
                        tool_call_id,
                        name,
                        display,
                        args,
                        input,
                        source_agent_id,
                    });
                for action in actions {
                    self.apply_pipeline_action(action);
                }
                self.request_rebuild();
                (true, false, false)
            }
            AgentEvent::ToolEnd {
                tool_call_id,
                name,
                output,
                is_error,
                source_agent_id,
            } => {
                let actions = self
                    .session_mgr
                    .current_mut()
                    .messages
                    .pipeline
                    .handle_event(AgentEvent::ToolEnd {
                        tool_call_id,
                        name,
                        output,
                        is_error,
                        source_agent_id,
                    });
                for action in actions {
                    self.apply_pipeline_action(action);
                }
                self.request_rebuild();
                (true, false, false)
            }
            AgentEvent::AssistantChunk {
                chunk,
                source_agent_id,
            } => {
                self.session_mgr.current_mut().agent.retry_status = None;
                self.session_mgr.current_mut().agent.agent_replied = true;
                // 跨切面：spinner
                self.session_mgr
                    .current_mut()
                    .spinner_state
                    .set_mode(peri_widgets::SpinnerMode::Responding);
                // Pipeline：路由到 SubAgentGroup 或父 Agent AssistantBubble
                let actions = self
                    .session_mgr
                    .current_mut()
                    .messages
                    .pipeline
                    .handle_event(AgentEvent::AssistantChunk {
                        chunk,
                        source_agent_id,
                    });
                for action in actions {
                    self.apply_pipeline_action(action);
                }
                (true, false, false)
            }
            AgentEvent::Done => self.handle_done(),
            AgentEvent::Interrupted => self.handle_interrupted(),
            AgentEvent::Error(ref e) => self.handle_error(e),
            AgentEvent::InteractionRequest { ctx, response_tx } => {
                self.handle_interaction_request(ctx, response_tx)
            }
            AgentEvent::TodoUpdate(todos) => {
                self.session_mgr.current_mut().todo_items = todos;
                (true, false, false)
            }
            AgentEvent::StateSnapshot(msgs) => {
                tracing::debug!(
                    snapshot_msgs = msgs.len(),
                    origin_msgs_before = self.session_mgr.current().agent.origin_messages.len(),
                    "StateSnapshot received in TUI"
                );
                // 子 Agent 的 StateSnapshot 不应污染父 Agent 的 origin_messages，
                // 否则子 Agent 的全部内部消息会混入父 Agent 的对话历史和持久化。
                if self.session_mgr.current_mut().agent.subagent_depth > 0 {
                    return (true, false, false);
                }
                self.session_mgr
                    .current_mut()
                    .agent
                    .origin_messages
                    .extend(msgs.clone());
                let actions = self
                    .session_mgr
                    .current_mut()
                    .messages
                    .pipeline
                    .handle_event(AgentEvent::StateSnapshot(msgs));
                for action in actions {
                    self.apply_pipeline_action(action);
                }
                self.request_rebuild();
                (true, false, false)
            }
            AgentEvent::CompactCompleted {
                summary,
                files,
                skills,
                micro_cleared,
                messages,
            } => self.handle_compact_completed(summary, files, skills, micro_cleared, messages),
            AgentEvent::CompactStarted => self.handle_compact_started(),
            AgentEvent::CompactError(msg) => self.handle_compact_error(msg),
            AgentEvent::RewindCompleted { summary, messages } => {
                self.handle_rewind_completed(summary, messages)
            }
            AgentEvent::LlmRetrying {
                attempt,
                max_attempts,
                delay_ms,
                error,
            } => {
                // 子 Agent 的 LlmRetrying 不应覆盖父 Agent 的 retry_status 显示
                if self.session_mgr.current_mut().agent.subagent_depth > 0 {
                    return (true, false, false);
                }
                self.session_mgr.current_mut().agent.retry_status =
                    Some(super::agent_comm::RetryStatus {
                        attempt,
                        max_attempts,
                        delay_ms,
                        error,
                    });
                (true, false, false)
            }
            AgentEvent::AiReasoning(text) => {
                let actions = self
                    .session_mgr
                    .current_mut()
                    .messages
                    .pipeline
                    .handle_event(AgentEvent::AiReasoning(text));
                for action in actions {
                    self.apply_pipeline_action(action);
                }
                (true, false, false)
            }
            AgentEvent::BackgroundTaskCompleted {
                task_id,
                agent_name,
                success,
                output,
                tool_calls_count,
                duration_ms,
                child_thread_id,
            } => self.handle_background_task_completed(BackgroundTaskResult {
                task_id,
                agent_name,
                success,
                output,
                tool_calls_count,
                duration_ms,
                child_thread_id,
            }),
            AgentEvent::LspDiagnostics {
                errors,
                warnings,
                files_with_errors,
            } => {
                self.session_mgr.current_mut().agent.lsp_diagnostics.errors = errors;
                self.session_mgr
                    .current_mut()
                    .agent
                    .lsp_diagnostics
                    .warnings = warnings;
                self.session_mgr
                    .current_mut()
                    .agent
                    .lsp_diagnostics
                    .files_with_errors = files_with_errors;
                (true, false, false)
            }
            AgentEvent::BgToolStep { child_thread_id } => {
                self.handle_bg_tool_step(&child_thread_id);
                (true, false, false)
            }
        }
    }

    // poll_agent/poll_background_events/poll_cron_triggers are in polling.rs
}

#[cfg(test)]
#[path = "../agent_ops_test.rs"]
mod tests;
