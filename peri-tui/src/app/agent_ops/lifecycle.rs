//! Agent lifecycle handlers — cleanup, done, interrupted, error.
//! Extracted from original agent_ops.rs (2026-05-20 split).

use tracing::debug;

use super::super::*;
use crate::app::{message_pipeline::PipelineAction, App};

impl App {
    /// Shared agent state teardown for Done, Error, and Disconnected paths.
    /// Ends the Langfuse trace, sets loading=false, clears interaction state,
    /// and records task duration. Callers handle bg task channel logic separately.
    pub(super) fn cleanup_agent_state(&mut self, langfuse_error: Option<&str>) {
        {
            let s = &mut self.session_mgr.current_mut();

            // End Langfuse trace
            let tracer = s.langfuse.langfuse_tracer.take();
            if let Some(ref t) = tracer {
                s.langfuse.langfuse_flush_handle = Some(t.lock().on_trace_end(langfuse_error));
            }
            s.langfuse.langfuse_tracer = None;

            // Clear interaction state
            s.agent.interaction_prompt = None;
            s.agent.pending_hitl_items = None;
            s.agent.pending_ask_user = None;

            // Record task duration
            if let Some(start) = s.agent.task_start_time {
                s.agent.last_task_duration = Some(start.elapsed());
            }
        }
        self.set_loading(false);
    }

    pub(super) fn handle_done(&mut self) -> (bool, bool, bool) {
        self.session_mgr.current_mut().agent.cancel_sent_at = None;
        // Child agent Done during tool execution — ignore
        let in_sub = self
            .session_mgr
            .current_mut()
            .messages
            .pipeline
            .in_subagent();
        debug!(
            in_subagent = in_sub,
            "AgentEvent::Done — checking in_subagent"
        );
        if in_sub {
            return (false, false, false);
        }
        self.session_mgr.current_mut().agent.retry_status = None;
        // Pipeline：finalize 当前 AI 消息
        let actions = self
            .session_mgr
            .current_mut()
            .messages
            .pipeline
            .handle_event(AgentEvent::Done);
        for action in actions {
            self.apply_pipeline_action(action);
        }
        // 跳过已由 Interrupted/Error 处理器完成的 reconcile
        if !self.session_mgr.current_mut().agent.reconcile_already_done {
            let prefix_len = self.session_mgr.current_mut().messages.round_start_vm_idx;
            let has_snapshot = self
                .session_mgr
                .current_mut()
                .messages
                .pipeline
                .has_snapshot_this_round();
            // 防御：compact 后 round_start_vm_idx 被设为 0，如果 compact 后
            // 没有新的 StateSnapshot 到达（agent 在 compact 后立即失败），
            // build_tail_vms 会返回空 tail，导致 prefix_len=0 的 drain 清空所有视图。
            if prefix_len == 0 && !has_snapshot {
                tracing::warn!(
                    "handle_done: prefix_len=0 with no snapshot, skipping rebuild to preserve view"
                );
            } else {
                self.request_rebuild();
            }
        } else {
            if let Some(vm) = self
                .session_mgr
                .current_mut()
                .messages
                .view_messages
                .last_mut()
            {
                if let MessageViewModel::AssistantBubble { is_streaming, .. } = vm {
                    *is_streaming = false;
                }
                vm.recompute_hash();
            }
            self.render_rebuild();
        }
        // 后台任务：保持通道存活
        if !self.session_mgr.current_mut().background_agents.is_empty() {
            self.session_mgr
                .current_mut()
                .agent
                .bg_task_state
                .agent_done_pending = true;
            tracing::info!(
                count = self.session_mgr.current_mut().background_agents.len(),
                "agent done but background tasks still running, keeping channel alive"
            );
        } else {
            // 竞态修复：处理暂存的后台任务完成通知
            if !self
                .session_mgr
                .current_mut()
                .agent
                .bg_task_state
                .pre_done_results
                .is_empty()
            {
                let results: Vec<_> = self
                    .session_mgr
                    .current_mut()
                    .agent
                    .bg_task_state
                    .pre_done_results
                    .drain(..)
                    .collect();
                tracing::info!(
                    count = results.len(),
                    "Done: processing pre-done background task completions, setting continuation"
                );
                self.session_mgr
                    .current_mut()
                    .agent
                    .bg_task_state
                    .pending_continuation = Some(results);
            }
            // 清理显示文本缓存
            self.session_mgr
                .current_mut()
                .agent
                .bg_task_state
                .pre_done_completions
                .clear();
        }
        self.cleanup_agent_state(None);
        // 检查缓冲消息，合并发送
        if !self
            .session_mgr
            .current_mut()
            .messages
            .pending_messages
            .is_empty()
        {
            self.flush_pending_messages();
        }
        (true, false, true)
    }

    pub(super) fn handle_interrupted(&mut self) -> (bool, bool, bool) {
        self.session_mgr.current_mut().agent.cancel_sent_at = None;
        // When parent agent is interrupted while executing a sync SubAgent,
        // pipeline.in_subagent() returns true because the SubAgent UI state is active.
        // Previously this was silently ignored, leaving the UI stuck in loading forever
        // (only rescued by 5s cancel_sent_at timeout). Now we proceed with normal
        // interrupt cleanup — the SubAgent's execute() was already dropped by the
        // parent's tool_dispatch select! cancellation, so SubAgent state is irrelevant.
        if self
            .session_mgr
            .current_mut()
            .messages
            .pipeline
            .in_subagent()
        {
            // Fall through to cleanup instead of returning early.
            // The in_subagent() guard was designed to ignore *child agent* interruptions
            // (e.g. a background agent being cancelled), but it also catches *parent agent*
            // interruptions during sync SubAgent execution — which is the user's Ctrl+C intent.
            tracing::info!(
                "Parent agent interrupted during sync SubAgent — proceeding with cleanup"
            );
        }
        // Pipeline：finalize 当前状态
        let actions = self
            .session_mgr
            .current_mut()
            .messages
            .pipeline
            .handle_event(AgentEvent::Interrupted);
        for action in actions {
            self.apply_pipeline_action(action);
        }

        // 在 view_messages 中定位最后一个 UserBubble 的索引，
        // 而非依赖 round_start_vm_idx（Pipeline rebuild 会使 VM 索引偏移）。
        let user_msg_idx = self
            .session_mgr
            .current_mut()
            .messages
            .view_messages
            .iter()
            .rposition(|vm| matches!(vm, MessageViewModel::UserBubble { .. }))
            .unwrap_or(0);
        let view_len = self.session_mgr.current_mut().messages.view_messages.len();
        tracing::info!(
            user_msg_idx,
            view_len,
            has_tool_calls = false,
            "handle_interrupted: about to check for tool calls"
        );
        let has_tool_calls = self
            .session_mgr
            .current_mut()
            .messages
            .view_messages
            .iter()
            .skip(user_msg_idx + 1) // UserBubble 之后的消息
            .any(|vm| {
                matches!(
                    vm,
                    MessageViewModel::ToolCallGroup { .. } | MessageViewModel::ToolBlock { .. }
                )
            });

        if has_tool_calls {
            // 已有工具调用：只中断，保留对话历史
            let vm = MessageViewModel::system(self.services.lc.tr("app-interrupt-done"));
            self.apply_pipeline_action(PipelineAction::AddMessage(vm));
            // 标记 reconcile 已完成，防止后续 Done 事件覆盖通知消息
            self.session_mgr.current_mut().agent.reconcile_already_done = true;
            peri_agent::metrics::emit(
                "trap.cancel_interrupt",
                serde_json::json!({
                    "subagent_depth": self.session_mgr.current().agent.subagent_depth,
                    "messages_in_state": self.session_mgr.current().messages.view_messages.len(),
                    "had_progress": has_tool_calls,
                }),
                Some(&self.session_mgr.current().metadata.session_id.to_string()),
                None,
            );
            return (true, false, false);
        }

        // 无工具调用：撤回用户消息，恢复文本到输入框
        if let Some(text) = self
            .session_mgr
            .current_mut()
            .messages
            .last_submitted_text
            .take()
        {
            // 截断 view_messages（移除 UserBubble + 本轮所有 Agent 响应）
            tracing::info!(
                user_msg_idx,
                pre_drain_len = view_len,
                "handle_interrupted: RebuildAll with prefix_len"
            );
            self.apply_pipeline_action(PipelineAction::RebuildAll {
                prefix_len: user_msg_idx,
                tail_vms: vec![],
            });
            let view_len_after = self.session_mgr.current_mut().messages.view_messages.len();
            tracing::info!(view_len_after, "handle_interrupted: after RebuildAll");
            // 截断 origin_messages（回滚 StateSnapshot 扩展的内容）
            let pre_len = self.session_mgr.current_mut().metadata.pre_submit_state_len;
            self.session_mgr
                .current_mut()
                .agent
                .origin_messages
                .truncate(pre_len);
            // 恢复文本到输入框
            let mut ta = crate::app::build_textarea(false);
            ta.insert_str(text.clone());
            self.session_mgr.current_mut().ui.textarea = ta;
            // 清除 pending 缓冲
            self.session_mgr
                .current_mut()
                .messages
                .pending_messages
                .clear();
            // 清除 sticky header
            self.session_mgr.current_mut().metadata.last_human_message = None;
            // 清除 pipeline 状态
            self.session_mgr.current_mut().messages.pipeline.done();
            let restored = self.session_mgr.current_mut().agent.origin_messages.clone();
            self.session_mgr
                .current_mut()
                .messages
                .pipeline
                .restore_completed(restored);
            let vm = MessageViewModel::system(self.services.lc.tr("app-interrupted-resumed"));
            self.apply_pipeline_action(PipelineAction::AddMessage(vm));
        } else {
            let vm = MessageViewModel::system(self.services.lc.tr("app-interrupt-done"));
            self.apply_pipeline_action(PipelineAction::AddMessage(vm));
        }
        // 标记 reconcile 已完成，防止后续 Done 事件重复 RebuildAll 覆盖通知消息
        self.session_mgr.current_mut().agent.reconcile_already_done = true;
        (true, false, false)
    }

    pub(super) fn handle_error(&mut self, error_msg: &str) -> (bool, bool, bool) {
        self.session_mgr.current_mut().agent.cancel_sent_at = None;
        // Child agent error during tool execution — ignore
        if self
            .session_mgr
            .current_mut()
            .messages
            .pipeline
            .in_subagent()
        {
            return (false, false, false);
        }
        self.session_mgr.current_mut().agent.retry_status = None;
        // 清理 pipeline 状态（残留 SubAgent 栈等），防止下一个任务 UI 损坏
        self.session_mgr.current_mut().messages.pipeline.done();

        let mut vm = MessageViewModel::tool_block(
            "error".to_string(),
            "Agent Error".to_string(),
            None,
            true,
        );
        if let MessageViewModel::ToolBlock {
            content, collapsed, ..
        } = &mut vm
        {
            *content = error_msg.to_string();
            *collapsed = false;
            vm.recompute_hash();
        }
        self.apply_pipeline_action(PipelineAction::AddMessage(vm));
        // 标记 reconcile 已完成，防止后续 Done 事件重复 RebuildAll 覆盖错误消息
        self.session_mgr.current_mut().agent.reconcile_already_done = true;
        // 后台任务：保持通道存活
        if !self.session_mgr.current_mut().background_agents.is_empty() {
            self.session_mgr
                .current_mut()
                .agent
                .bg_task_state
                .agent_done_pending = true;
        } else {
            if !self
                .session_mgr
                .current_mut()
                .agent
                .bg_task_state
                .pre_done_results
                .is_empty()
            {
                let results: Vec<_> = self
                    .session_mgr
                    .current_mut()
                    .agent
                    .bg_task_state
                    .pre_done_results
                    .drain(..)
                    .collect();
                tracing::info!(
                    count = results.len(),
                    "Error: processing pre-done background task completions, setting continuation"
                );
                self.session_mgr
                    .current_mut()
                    .agent
                    .bg_task_state
                    .pending_continuation = Some(results);
            }
            // 清理显示文本缓存
            self.session_mgr
                .current_mut()
                .agent
                .bg_task_state
                .pre_done_completions
                .clear();
        }
        let err_label = format!("ERROR: {}", error_msg);
        self.cleanup_agent_state(Some(&err_label));
        // 检查缓冲消息，合并发送
        if !self
            .session_mgr
            .current_mut()
            .messages
            .pending_messages
            .is_empty()
        {
            self.flush_pending_messages();
        }
        (true, false, true)
    }

    // handle_agent_event is in mod.rs
}
