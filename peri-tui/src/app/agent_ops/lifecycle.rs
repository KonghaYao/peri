//! Agent lifecycle handlers — cleanup, done, interrupted, error.
//! Extracted from original agent_ops.rs (2026-05-20 split).

use tracing::debug;

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

        // v2 渲染不再读取 v1 is_streaming 标志

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

        // Cron #31 P1 fix: early-return on subagent_depth > 0, mirroring
        // handle_done (line 44-46) and handle_error (line 212-214).
        //
        // Bug: prior to this guard, Ctrl+C during a sync SubAgent's run
        // would flow through the FULL interrupt pipeline on the PARENT's
        // view_slice — `last_user_bubble_index` finds the parent's
        // UserBubble, `has_tool_cards_after` checks the parent's top-level
        // ToolCards (not the SubAgent's internal content). If the parent
        // had no top-level ToolCards after the user bubble (common during
        // early SubAgent execution), branch 2 triggers — apply_rebuild_all
        // wipes the parent's progress + pending_view_rewind_to truncates
        // state.view + last_submitted_text gets restored to textarea.
        // Result: cancelling a SubAgent accidentally rolled back the
        // parent's submission + input.
        //
        // The SubAgentEnd event (mod.rs:59-93) still fires independently
        // to decrement subagent_depth and record SubAgent completion
        // status, so this early-return doesn't strand any SubAgent state.
        if self.session_mgr.current_mut().agent.subagent_depth > 0 {
            tracing::info!(
                "Parent agent interrupted during sync SubAgent — bailing out to preserve parent state"
            );
            return (false, false, false);
        }

        // Cron #33: reset retry_status and defer to cleanup_agent_state for
        // the shared teardown. Mirrors handle_done (line 47 + 97) and
        // handle_error (line 234 + 294). Without this, Ctrl+C during an
        // LLM retry leaves:
        //   - stale retry_status in the status bar ("Retrying (attempt N)")
        //     — read by status_bar.rs:212
        //   - langfuse_tracer never flushed (orphaned trace)
        //   - interaction_prompt / pending_hitl_items / pending_ask_user
        //     left dangling (a subsequent Done/Error would have cleared
        //     them, but if the executor returns Interrupted as the
        //     terminal event, they stay set forever)
        //   - task_start_time consumed into last_task_duration only on
        //     Done/Error paths today — status bar shows "Elapsed: …"
        //     counting from this turn's start, but last_task_duration
        //     is never set, so a future agent submit resets it
        //   - spinner_state stays in Responding mode (set_loading(false)
        //     only happens via cleanup_agent_state)
        self.session_mgr.current_mut().agent.retry_status = None;

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
        let user_msg_idx = view_slice
            .iter()
            .enumerate()
            .rev()
            .find(|(_, vm)| matches!(vm, peri_acp_types::view_model::ViewModel::UserBubble(_)))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let view_len = view_slice.len();
        tracing::info!(
            user_msg_idx,
            view_len,
            "handle_interrupted: about to check for tool calls (v2)"
        );
        let has_tool_calls = view_slice[user_msg_idx..]
            .iter()
            .any(|vm| matches!(vm, peri_acp_types::view_model::ViewModel::ToolCard(_)));

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
            // Cron #33: branch 1 (keep tool progress) is also a terminal
            // agent state — clear langfuse tracer / interaction_prompt /
            // spinner / loading here too, mirroring branch 2/3 below.
            self.cleanup_agent_state(None);
            return (true, false, false);
        }

        if let Some(text) = self
            .session_mgr
            .current_mut()
            .messages
            .last_submitted_text
            .take()
        {
            // Cron #40 (Phase 2.6 step 7e.3): retired apply_rebuild_all(user_msg_idx, vec![])
            // — it only truncated v1 view_messages, but production render reads v2 state.view
            // exclusively. The v2 rewind is carried out by pending_view_rewind_to flag below
            // (consumed by main_loop at the top of the next iteration, truncating state.view).
            // Audit workflow wj0c3ppca auditor-0 confirmed apply_rebuild_all in handle_interrupted
            // was load-bearing ONLY for v1 test assertions; pending_view_rewind_to is the
            // production-critical path. Test assertions on view_messages.len() migrated to
            // rely solely on pending_view_rewind_to + state.view semantics.
            //
            // Cron #23 P1 fix: 请求 main_loop 截断 v2 state.view 到 user_msg_idx。
            // 否则 stale UserBubble + 部分 AssistantBubble 会持续存在直到下一个
            // view-commit（用户感知为 "按 Esc 回滚后消息还在"）。main_loop 在
            // handle_acp_event 返回后消费此 flag，仅对 Idle/Streaming 生效
            // （Modal/Switching 跳过）。
            self.global_ui.pending_view_rewind_to = Some(user_msg_idx);
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
        // Cron #33: terminal agent state — call shared teardown before
        // flush_pending_messages to mirror handle_done (line 97-106).
        // Without this, branch 2 (rollback) and branch 3 (no rollback
        // text) leave loading=true / spinner in Responding mode after
        // Ctrl+C.
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

        // Cron #39 (Phase 2.6 step 7e.2): retired the v1 apply_add_message(vm)
        // push — it wrote a ToolBlock to view_messages, but production render
        // reads v2 state.view exclusively (via state.view_models()). The v2
        // route below (push_system_note → pending_v2_notes → SM
        // Event::PushSystemNote → state.view) is the sole load-bearing path.
        // Confirmed safe by audit workflow wj0c3ppca auditor-0: only
        // test_handle_error_routes_to_v2_state_view covers this, and it
        // asserts pending_v2_notes (not view_messages).
        self.push_system_note(format!("⚠️ Agent Error: {}", error_msg));
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

// ── Cron #23 P1 fix regression tests ─────────────────────────────────────────
//
// 验证 handle_interrupted 分支 2（无工具调用，回滚路径）请求 main_loop 截断
// v2 state.view 到 user_msg_idx。完整的 end-to-end 测试在 main_loop 中较难
// 模拟（需要 channel + ApplyContext），所以这里只验证 flag 被正确设置——
// main_loop 端的应用逻辑通过纯函数 truncate(idx) 直接验证（Vec::truncate 是
// stdlib，无需测试）。
//
// 重点测试场景：
// 1. 分支 2（无工具，回滚）设置 flag = Some(user_msg_idx)
// 2. 分支 1（有工具，保留）不设置 flag（保持 None）
// 3. 没有 last_submitted_text 的回滚路径也设置 flag
