//! Agent lifecycle handlers — cleanup, done, interrupted, error.
//! Extracted from original agent_ops.rs (2026-05-20 split).

use tracing::debug;

use super::super::*;
use crate::app::App;

impl App {
    /// Shared agent state teardown for Done, Error, and Disconnected paths.
    pub(super) fn cleanup_agent_state(&mut self, langfuse_error: Option<&str>) {
        {
            let s = &mut self.session_mgr.current_mut();

            let tracer = s.langfuse.langfuse_tracer.take();
            if let Some(ref t) = tracer {
                s.langfuse.langfuse_flush_handle = Some(t.lock().on_trace_end(langfuse_error));
            }
            s.langfuse.langfuse_tracer = None;

            s.agent.interaction_prompt = None;
            s.agent.pending_hitl_items = None;
            s.agent.pending_ask_user = None;

            if let Some(start) = s.agent.task_start_time {
                s.agent.last_task_duration = Some(start.elapsed());
            }

            // Phase 2.3: 清理过期的 SubAgentStatus entry（TTL 5 分钟）
            s.subagent_status.evict_expired();
        }
        self.set_loading(false);
    }

    pub(super) fn handle_done(&mut self) -> (bool, bool, bool) {
        self.session_mgr.current_mut().agent.cancel_sent_at = None;

        // P5: Check subagent_depth instead of pipeline.in_subagent()
        let in_sub = self.session_mgr.current_mut().agent.subagent_depth > 0;
        debug!(
            in_subagent = in_sub,
            "AgentEvent::Done — checking in_subagent"
        );
        if in_sub {
            return (false, false, false);
        }
        self.session_mgr.current_mut().agent.retry_status = None;

        // Phase 2.6 step 7e.1: Retired the v1 is_streaming=false mutation
        // on the last AssistantBubble. Confirmed dead in production:
        // vm_convert.rs:293 always emits is_streaming: false when bridging
        // v1 → v2, so v2 rendering never reads the v1 flag. The v1 fallback
        // render path (message_area.rs:155) is only triggered by tests;
        // production always passes v2_view_models=Some.

        if !self.session_mgr.current_mut().agent.reconcile_already_done {
            let prefix_len = self.session_mgr.current_mut().messages.round_start_vm_idx;
            // P5: No has_snapshot_this_round() check — simpler defense
            if prefix_len == 0 {
                tracing::warn!("handle_done: prefix_len=0, skipping rebuild to preserve view");
            } else {
                self.request_rebuild();
            }
        }

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
                let _results: Vec<_> = self
                    .session_mgr
                    .current_mut()
                    .agent
                    .bg_task_state
                    .pre_done_results
                    .drain(..)
                    .collect();
            }
            self.session_mgr
                .current_mut()
                .agent
                .bg_task_state
                .pre_done_completions
                .clear();
        }
        self.cleanup_agent_state(None);
        if !self
            .session_mgr
            .current()
            .messages
            .pending_messages
            .is_empty()
        {
            self.flush_pending_messages();
        }
        (true, false, true)
    }

    pub(super) fn handle_interrupted(
        &mut self,
        view_slice: &[peri_acp_types::view_model::ViewModel],
    ) -> (bool, bool, bool) {
        self.session_mgr.current_mut().agent.cancel_sent_at = None;

        if self.session_mgr.current_mut().agent.subagent_depth > 0 {
            tracing::info!(
                "Parent agent interrupted during sync SubAgent — proceeding with cleanup"
            );
        }

        // Phase 2.6 step 7c: scan v2 state.view (passed in as view_slice)
        // instead of v1 view_messages. The v2 helpers are pure functions
        // over &[ViewModel] defined in state_machine::view_store.
        //
        // v2 semantics:
        // - last_user_bubble_index scans for ViewModel::UserBubble (DTO type).
        //   The SM Enter transition (idle.rs step 7d) pushes UserBubble to
        //   state.view on submit, so this finds it without v1 view_messages.
        // - has_tool_cards_after scans top-level for ViewModel::ToolCard.
        //   The SM TurnInterrupted handler (streaming.rs step 7c) persists
        //   current_turn's ToolCards to state.view, so this detects progress
        //   correctly even if interrupt arrives before ViewCommit.
        let user_msg_idx =
            crate::state_machine::view_store::last_user_bubble_index(view_slice).unwrap_or(0);
        let view_len = view_slice.len();
        tracing::info!(
            user_msg_idx,
            view_len,
            "handle_interrupted: about to check for tool calls (v2)"
        );
        let has_tool_calls =
            crate::state_machine::view_store::has_tool_cards_after(view_slice, user_msg_idx);

        if has_tool_calls {
            self.push_system_note(self.services.lc.tr("app-interrupt-done"));
            self.session_mgr.current_mut().agent.reconcile_already_done = true;
            peri_agent::metrics::emit(
                "trap.cancel_interrupt",
                serde_json::json!({
                    "subagent_depth": self.session_mgr.current().agent.subagent_depth,
                    "view_vm_count": view_slice.len(),
                    "had_progress": has_tool_calls,
                }),
                Some(&self.session_mgr.current().metadata.session_id.to_string()),
                None,
            );
            return (true, false, false);
        }

        if let Some(text) = self
            .session_mgr
            .current_mut()
            .messages
            .last_submitted_text
            .take()
        {
            self.apply_rebuild_all(user_msg_idx, vec![]);
            let pre_len = self.session_mgr.current_mut().metadata.pre_submit_state_len;
            self.session_mgr
                .current_mut()
                .agent
                .origin_messages
                .truncate(pre_len);
            let mut ta = crate::app::build_textarea(false);
            ta.insert_str(text.clone());
            self.session_mgr.current_mut().ui.textarea = ta;
            self.session_mgr
                .current_mut()
                .messages
                .pending_messages
                .clear();
            self.session_mgr.current_mut().metadata.last_human_message = None;
            // P5: No pipeline.done()/restore_completed()
            self.push_system_note(self.services.lc.tr("app-interrupted-resumed"));
        } else {
            self.push_system_note(self.services.lc.tr("app-interrupt-done"));
        }
        self.session_mgr.current_mut().agent.reconcile_already_done = true;
        if !self
            .session_mgr
            .current()
            .messages
            .pending_messages
            .is_empty()
        {
            self.flush_pending_messages();
        }
        (true, false, false)
    }

    pub(super) fn handle_error(&mut self, error_msg: &str) -> (bool, bool, bool) {
        self.session_mgr.current_mut().agent.cancel_sent_at = None;

        // P5: Check subagent_depth instead of pipeline.in_subagent()
        if self.session_mgr.current_mut().agent.subagent_depth > 0 {
            return (false, false, false);
        }
        self.session_mgr.current_mut().agent.retry_status = None;
        // P5: No pipeline.done() needed

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
        self.apply_add_message(vm);
        self.session_mgr.current_mut().agent.reconcile_already_done = true;

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
                let _results: Vec<_> = self
                    .session_mgr
                    .current_mut()
                    .agent
                    .bg_task_state
                    .pre_done_results
                    .drain(..)
                    .collect();
            }
            self.session_mgr
                .current_mut()
                .agent
                .bg_task_state
                .pre_done_completions
                .clear();
        }
        let err_label = format!("ERROR: {}", error_msg);
        self.cleanup_agent_state(Some(&err_label));
        if !self
            .session_mgr
            .current()
            .messages
            .pending_messages
            .is_empty()
        {
            self.flush_pending_messages();
        }
        (true, false, true)
    }
}
