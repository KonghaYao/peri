//! Agent event dispatch — the main `handle_agent_event` dispatcher routes
//! individual AgentEvent variants to specialized handlers.
//! Extracted sub-modules:
//!   acp_bridge.rs — ACP notification bridge
//!   lifecycle.rs — cleanup, done, interrupted, error
//!   subagent.rs  — token usage, subagent start
//!   polling.rs   — poll_agent, poll_panic_notifications, poll_workflow_runs

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
                agent_id: _,
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
                // Phase 2.3: 同步完成状态到 SubAgentStatusMap（v2 渲染权威源）
                if let Some(inst) = instance_id.as_deref() {
                    self.session_mgr
                        .current_mut()
                        .subagent_status
                        .complete_foreground(inst, result.clone(), is_error);
                }
                // Phase 2.6 step 6 — 删除 view_messages.SubAgentGroup iter_mut 突变
                // + ToolBlock fallback。生产渲染完全通过 SubAgentStatusMap +
                // SessionSubAgentProbe 读取完成状态，不依赖 view_messages。
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
            } => {
                // v2: PluginPanel 暂未迁移，改为推送系统通知
                let note = if success {
                    format!("Plugin action completed: {} ({})", plugin_id, action)
                } else {
                    format!(
                        "Plugin action failed: {} ({}): {}",
                        plugin_id, action, message
                    )
                };
                self.session_mgr
                    .current_mut()
                    .messages
                    .push_system_note(note);
                (true, false, false)
            }
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
                input: _,
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

                // Phase 2.6: source_agent_id 路由 — 若匹配 SubAgent 则累积到
                // SubAgentStatus.child_messages（v2 权威源），不再追加到 view_messages。
                // 这是实现「子 Agent 内容嵌套显示」的关键路径。
                let tool_card = peri_acp_types::view_model::ViewModel::ToolCard(
                    peri_acp_types::view_model::ToolCardData {
                        tool_id: tool_call_id.clone(),
                        tool_name: name.clone(),
                        input_summary: args.clone(),
                        output_summary: String::new(),
                        is_error: false,
                        diff: None,
                    },
                );
                let routed_to_subagent = match source_agent_id.as_deref() {
                    Some(src) => self
                        .session_mgr
                        .current_mut()
                        .subagent_status
                        .append_child_message(src, tool_card),
                    None => false,
                };
                if routed_to_subagent {
                    self.request_rebuild();
                    return (true, false, false);
                }
                // 主 Agent 路径：保留 v1 view_messages 累积（向后兼容）。
                // P5: No pipeline — create ToolBlock directly
                let header = format!("{} {}", display, args);
                let tool_vm = MessageViewModel::tool_block(name.clone(), header, None, false);
                self.apply_add_message(tool_vm);
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
                // Phase 2.6: 优先在 SubAgentStatus.child_messages 中更新 ToolCard
                // （与 ToolStart 路由配对）。匹配成功则跳过 view_messages 查找。
                if let Some(src) = source_agent_id.as_deref() {
                    if self
                        .session_mgr
                        .current_mut()
                        .subagent_status
                        .update_child_tool_output(src, &tool_call_id, output.clone(), is_error)
                    {
                        self.request_rebuild();
                        return (true, false, false);
                    }
                }
                // 主 Agent 路径或 SubAgent 路由失败时的 fallback
                let session = self.session_mgr.current_mut();
                let mut found = false;
                for vm in &mut session.messages.view_messages {
                    if let MessageViewModel::ToolBlock {
                        tool_call_id: vm_tc_id,
                        content,
                        collapsed,
                        is_error: vm_is_error,
                        ..
                    } = vm
                    {
                        if vm_tc_id == &tool_call_id {
                            *content = output.clone();
                            *collapsed = false; // auto-expand on completion
                            *vm_is_error = is_error;
                            vm.recompute_hash();
                            found = true;
                            break;
                        }
                    }
                }
                if !found {
                    // 若 source_agent_id 匹配 SubAgent（但 ToolStart 未路由 / 已被 evict），
                    // 仍然累积到 child_messages 而非主消息流（保持子 Agent 内容隔离）。
                    if let Some(src) = source_agent_id.as_deref() {
                        let tool_card = peri_acp_types::view_model::ViewModel::ToolCard(
                            peri_acp_types::view_model::ToolCardData {
                                tool_id: tool_call_id.clone(),
                                tool_name: name.clone(),
                                input_summary: String::new(),
                                output_summary: output.clone(),
                                is_error,
                                diff: None,
                            },
                        );
                        if session.subagent_status.append_child_message(src, tool_card) {
                            session.messages.message_cache = None;
                            found = true;
                        }
                    }
                }
                if !found {
                    let mut vm =
                        MessageViewModel::tool_block(name.clone(), output.clone(), None, is_error);
                    vm.recompute_hash();
                    session.messages.view_messages.push(vm);
                    // Invalidate render cache — view_messages mutated.
                    session.messages.message_cache = None;
                }
                let _ = session;
                self.request_rebuild();
                (true, false, false)
            }
            AgentEvent::AssistantChunk { source_agent_id } => {
                // Phase 2.6 step 2：子 Agent 的 AssistantChunk 不应污染父 Agent 状态。
                // - 主 Agent（None）：保持原行为（清 retry、设 spinner、agent_replied）
                // - 子 Agent（Some）：仅触发 rebuild，跳过副作用
                if source_agent_id.is_some() {
                    self.request_rebuild();
                    return (true, false, false);
                }
                self.session_mgr.current_mut().agent.retry_status = None;
                self.session_mgr.current_mut().agent.agent_replied = true;
                self.session_mgr
                    .current_mut()
                    .spinner_state
                    .set_mode(peri_widgets::SpinnerMode::Responding);
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
                // P5: No pipeline — StateSnapshot updates origin_messages only
                // NOTE: extend semantics are correct here — StateSnapshot(msgs)
                // is a legacy v1 event carrying incremental messages, not a
                // full transcript snapshot.
                self.request_rebuild();
                (true, false, false)
            }
            AgentEvent::TurnCommitted { messages, steps } => {
                tracing::debug!(
                    committed_msgs = messages.len(),
                    steps,
                    origin_msgs_before = self.session_mgr.current().agent.origin_messages.len(),
                    "TurnCommitted received in TUI (v2 iteration boundary)"
                );
                if self.session_mgr.current_mut().agent.subagent_depth > 0 {
                    return (true, false, false);
                }
                // P5: TurnCommitted carries the FULL transcript snapshot from v2 stages.
                // Use replacement semantics (not extend) — finalized_messages is a
                // complete snapshot, not incremental. extend would double the history
                // on each turn (see CLAUDE.md TRAP re: commit_iteration).
                self.session_mgr.current_mut().agent.origin_messages = messages;
                // Convert origin_messages to view_messages for sync rendering.
                // prefix_len = 0: full rebuild from complete transcript snapshot —
                // origin_messages is the full history (replacement semantics), and
                // messages_to_view_models returns ALL VMs. Using round_start_vm_idx
                // would duplicate prefix VMs in the tail (same messages appear twice).
                let cwd = self.services.cwd.clone();
                let view_msgs = super::messages_to_view_models(
                    &self.session_mgr.current().agent.origin_messages,
                    &cwd,
                );
                self.apply_rebuild_all(0, view_msgs);
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
            AgentEvent::WorkflowProgress(payload) => {
                self.global_ui.workflow_tracker.apply(&payload);
                // v2: Workflow panel refresh handled by state machine via PanelReadContext
                (true, false, false)
            }
            AgentEvent::StateSnapshotMeta {
                message_count,
                current_step,
                budget_pct,
                ..
            } => {
                // v2 轻量级元数据快照：仅日志观测，不触发 UI 状态变更。
                // budget_pct / context_total_tokens 后续可用于状态栏上下文使用率刷新。
                tracing::debug!(
                    message_count,
                    current_step,
                    budget_pct = ?budget_pct,
                    "[v2] StateSnapshotMeta received"
                );
                (true, false, false)
            }
        }
    }

    // poll_agent/poll_panic_notifications/poll_workflow_runs are in polling.rs
}

#[cfg(test)]
#[path = "../agent_ops_test.rs"]
mod tests;
