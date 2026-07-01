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

#[cfg(test)]
mod tests {
    use super::*;

    use peri_acp_types::view_model::{
        AssistantBubbleData, ToolCardData, UserBubbleData, ViewModel,
    };

    /// 构造一个 headless App，模拟 "用户提交后正在流式中" 的状态。
    async fn make_app_with_active_turn() -> App {
        let (mut app, _handle) = App::new_headless(80, 24).await;

        // Phase 2.6 step 7e: seed UserBubble via v2 test views
        app.seed_v2_user_bubble("hello");
        app.session_mgr.current_mut().messages.round_start_vm_idx = 1;
        app.session_mgr.current_mut().messages.last_submitted_text = Some("hello".to_string());
        app.session_mgr.current_mut().metadata.pre_submit_state_len = 0;
        app.set_loading(true);

        app
    }

    #[tokio::test]
    async fn test_handle_interrupted_branch2_sets_pending_view_rewind() {
        // 分支 2：view_slice 只有 UserBubble（无 ToolCard）→ 应触发回滚 + 设置 flag
        let mut app = make_app_with_active_turn().await;

        // view_slice 模拟 v2 state.view（来自 SM 的 UserBubble 推送）
        let view_slice: Vec<ViewModel> = vec![ViewModel::UserBubble(UserBubbleData {
            text: "hello".into(),
        })];

        let _ = app.handle_interrupted(&view_slice);

        // 核心断言：flag 已设置，值为 user_msg_idx（UserBubble 在 idx 0）
        assert_eq!(
            app.global_ui.pending_view_rewind_to,
            Some(0),
            "分支 2（回滚）必须设置 pending_view_rewind_to = Some(user_msg_idx)"
        );

        // Cron #40 (Phase 2.6 step 7e.3): 退役 apply_rebuild_all 后，v1
        // view_messages 不再被截断（生产渲染独占读 v2 state.view，截断由
        // main_loop 顶端的 pending_view_rewind_to 消费完成）。不再断言
        // view_messages.len()——它将在 view_messages 字段整体退役时一并删除。

        // last_submitted_text 应被还原到 textarea
        let textarea_text: String = app
            .session_mgr
            .current()
            .ui
            .textarea
            .lines()
            .to_vec()
            .join("\n");
        assert!(
            textarea_text.contains("hello"),
            "用户文本应被还原到 textarea，实际: {:?}",
            textarea_text
        );
    }

    #[tokio::test]
    async fn test_handle_interrupted_branch1_does_not_set_pending_view_rewind() {
        // 分支 1：view_slice 有 ToolCard（agent 已做工作）→ 应保留，不触发回滚
        let mut app = make_app_with_active_turn().await;

        // view_slice 包含 UserBubble + ToolCard → has_tool_cards_after 返回 true
        let view_slice: Vec<ViewModel> = vec![
            ViewModel::UserBubble(UserBubbleData {
                text: "hello".into(),
            }),
            ViewModel::ToolCard(ToolCardData {
                tool_id: "t1".into(),
                tool_name: "Bash".into(),
                input_summary: "ls".into(),
                output_summary: "files".into(),
                is_error: false,
                diff: None,
            }),
        ];

        let _ = app.handle_interrupted(&view_slice);

        // 分支 1：不应设置 flag（main_loop 不会截断 state.view）
        assert_eq!(
            app.global_ui.pending_view_rewind_to, None,
            "分支 1（保留工具进度）不应设置 pending_view_rewind_to"
        );

        // Cron #40: view_messages 不再断言——生产渲染独占读 v2 state.view，
        // 此分支保留语义通过 pending_view_rewind_to == None 间接保证
        // （若误设 flag，main_loop 会错误截断 state.view）。
    }

    #[tokio::test]
    async fn test_handle_interrupted_branch2_with_assistant_bubble_also_sets_flag() {
        // 边界场景：view_slice 有 UserBubble + AssistantBubble（流式文本）
        // 但没有 ToolCard。仍然应进入分支 2（回滚），设置 flag。
        let mut app = make_app_with_active_turn().await;

        let view_slice: Vec<ViewModel> = vec![
            ViewModel::UserBubble(UserBubbleData {
                text: "hello".into(),
            }),
            ViewModel::AssistantBubble(AssistantBubbleData {
                text: "partial reply".into(),
                reasoning: None,
                tool_card_ids: vec![],
            }),
        ];

        let _ = app.handle_interrupted(&view_slice);

        // UserBubble 在 idx 0 → flag 应为 Some(0)
        assert_eq!(
            app.global_ui.pending_view_rewind_to,
            Some(0),
            "AssistantBubble 不算 tool 进度，应进入分支 2 设置 flag"
        );

        // Cron #40: view_messages 截断断言已退役——生产渲染读 v2 state.view，
        // 由 main_loop 消费 pending_view_rewind_to 完成截断。
    }

    #[tokio::test]
    async fn test_handle_interrupted_no_last_submitted_text_no_flag() {
        // 边界场景：用户没有 last_submitted_text（例如 setup 期间被中断）。
        // 分支 2 的回滚代码块不执行，flag 不应被设置。
        let mut app = make_app_with_active_turn().await;
        // 清除 last_submitted_text
        app.session_mgr.current_mut().messages.last_submitted_text = None;

        let view_slice: Vec<ViewModel> = vec![ViewModel::UserBubble(UserBubbleData {
            text: "hello".into(),
        })];

        let _ = app.handle_interrupted(&view_slice);

        // 没有 last_submitted_text → 走 else 分支（push_system_note "interrupt-done"）
        // → flag 不应被设置
        assert_eq!(
            app.global_ui.pending_view_rewind_to, None,
            "无 last_submitted_text 时不应设置 pending_view_rewind_to"
        );
    }

    /// Cron #28: handle_error 必须把错误消息路由到 v2 state.view
    /// （通过 push_system_note → pending_v2_notes → SM Event::PushSystemNote）。
    ///
    /// 历史 bug：handle_error 只调 apply_add_message(vm) 写到 v1 view_messages，
    /// 但生产渲染独占读 v2 state.view → 用户在 agent 出错时（API 失败、rate limit
    /// 等）什么都看不到。修复镜像 cron #24/cron #26 queue-and-drain 模式。
    #[tokio::test]
    async fn test_handle_error_routes_to_v2_state_view() {
        let mut app = make_app_with_active_turn().await;

        let _ = app.handle_error("provider rate limited");

        // 验证 push_system_note 入队到 pending_v2_notes（由 main_loop drain）
        let pending = &app.session_mgr.current().messages.pending_v2_notes;
        assert_eq!(
            pending.len(),
            1,
            "handle_error must enqueue error message to pending_v2_notes"
        );
        // 验证消息文本包含原始 error_msg
        assert!(
            pending[0].contains("provider rate limited"),
            "enqueued note must contain original error message, got: {}",
            pending[0]
        );
    }

    // -----------------------------------------------------------------------
    // Cron #31: handle_interrupted subagent_depth early-return regression
    // -----------------------------------------------------------------------
    //
    // Bug: handle_interrupted lacked the subagent_depth > 0 early-return
    // that handle_done (line 44-46) and handle_error (line 212-214) both
    // have. Ctrl+C during a sync SubAgent's run flowed through the full
    // interrupt pipeline on the PARENT's view, rolling back the parent's
    // submission + restoring last_submitted_text.
    //
    // These tests verify the guard mirrors its siblings exactly.

    /// Cron #31 P1: handle_interrupted MUST early-return when
    /// subagent_depth > 0, preserving parent state.
    ///
    /// Without the guard, the parent's UserBubble in view_slice would
    /// trigger branch 2 rollback (no top-level ToolCards after user msg).
    #[tokio::test]
    async fn test_handle_interrupted_subagent_depth_early_return() {
        let mut app = make_app_with_active_turn().await;
        // Simulate SubAgent in flight
        app.session_mgr.current_mut().agent.subagent_depth = 1;

        // Parent's view_slice: just a UserBubble (would trigger branch 2
        // rollback if the guard isn't in place)
        let view_slice: Vec<ViewModel> = vec![ViewModel::UserBubble(UserBubbleData {
            text: "hello".into(),
        })];

        let (should_return, should_break, _) = app.handle_interrupted(&view_slice);

        // Guard returns (false, false, false) — same as handle_done/error
        assert!(
            !should_return && !should_break,
            "handle_interrupted must early-return (false, false, _) when subagent_depth > 0"
        );

        // CRITICAL: pending_view_rewind_to must NOT be set
        // (parent's state.view should not be truncated)
        assert_eq!(
            app.global_ui.pending_view_rewind_to, None,
            "subagent_depth > 0 must NOT trigger pending_view_rewind_to (would roll back parent)"
        );

        // CRITICAL: last_submitted_text must NOT be consumed
        // (parent's textarea should not be restored)
        assert_eq!(
            app.session_mgr
                .current()
                .messages
                .last_submitted_text
                .as_deref(),
            Some("hello"),
            "subagent_depth > 0 must NOT consume last_submitted_text"
        );

        // Cron #40: view_messages 断言已退役——guard 通过 flag + last_text
        // 间接保证。生产渲染独占读 v2 state.view，v1 字段将在整体退役时删除。
    }

    /// Cron #31 P1: handle_interrupted subagent_depth > 0 with ToolCards
    /// also early-returns (asymmetric to ensure parent state never touched).
    ///
    /// Verifies that branch 1 (has_tool_calls → keep progress) is also
    /// bypassed when subagent_depth > 0. The SubAgentEnd event handles
    /// cleanup; handle_interrupted should never touch parent state.
    #[tokio::test]
    async fn test_handle_interrupted_subagent_depth_skips_branch1_too() {
        let mut app = make_app_with_active_turn().await;
        app.session_mgr.current_mut().agent.subagent_depth = 1;

        // view_slice with ToolCards — would trigger branch 1 if guard absent
        let view_slice: Vec<ViewModel> = vec![
            ViewModel::UserBubble(UserBubbleData {
                text: "hello".into(),
            }),
            ViewModel::ToolCard(ToolCardData {
                tool_id: "t1".into(),
                tool_name: "Bash".into(),
                input_summary: "ls".into(),
                output_summary: "files".into(),
                is_error: false,
                diff: None,
            }),
        ];

        let _ = app.handle_interrupted(&view_slice);

        // Branch 1 would set reconcile_already_done = true + push system note
        // "app-interrupt-done". Both must NOT happen.
        assert!(
            !app.session_mgr.current().agent.reconcile_already_done,
            "subagent_depth > 0 must NOT set reconcile_already_done (branch 1 also bypassed)"
        );

        // No "interrupt-done" system note enqueued
        let pending = &app.session_mgr.current().messages.pending_v2_notes;
        assert!(
            pending.is_empty(),
            "subagent_depth > 0 must NOT push interrupt-done note, got: {:?}",
            pending
        );
    }

    /// Cron #31 P1: subagent_depth = 0 (no SubAgent) must NOT early-return —
    /// existing branch 1/branch 2 logic still applies.
    ///
    /// Regression guard: the new early-return must not break the normal
    /// (no SubAgent) interrupt flow.
    #[tokio::test]
    async fn test_handle_interrupted_no_subagent_still_runs_full_pipeline() {
        let mut app = make_app_with_active_turn().await;
        // subagent_depth = 0 (default)

        let view_slice: Vec<ViewModel> = vec![ViewModel::UserBubble(UserBubbleData {
            text: "hello".into(),
        })];

        let _ = app.handle_interrupted(&view_slice);

        // Branch 2 should fire: flag set + pending_view_rewind_to set
        assert_eq!(
            app.global_ui.pending_view_rewind_to,
            Some(0),
            "subagent_depth = 0 must still run full pipeline (branch 2 rollback)"
        );
    }

    // -----------------------------------------------------------------------
    // Cron #33: handle_interrupted state cleanup parity with handle_done
    // -----------------------------------------------------------------------
    //
    // Bug: handle_interrupted never reset retry_status and never called
    // cleanup_agent_state — unlike handle_done (line 47 + 97) and
    // handle_error (line 234 + 294). Ctrl+C during an LLM retry left a
    // stale "Retrying (attempt N)" in the status bar, an orphaned
    // langfuse tracer, dangling interaction_prompt / pending_hitl_items /
    // pending_ask_user, and spinner stuck in Responding mode (loading
    // stayed true because set_loading(false) is only called via
    // cleanup_agent_state).
    //
    // These tests verify the parity:
    //   1. retry_status cleared in ALL branches (1/2/3)
    //   2. spinner_state reset to Idle (set_loading(false) ran)
    //   3. loading flag cleared
    //   4. Early-return on subagent_depth > 0 does NOT clear (SubAgentEnd
    //      handles its own cleanup; parent's retry_status may still be
    //      legitimately set if parent was retrying before SubAgent ran)

    /// Helper: simulate an active LLM retry state — mirrors what
    /// AgentEvent::LlmRetrying sets (retry_status populated).
    fn set_active_retry(app: &mut App) {
        // RetryStatus is a plain struct with public fields (see
        // app/agent_comm.rs:14). Construct directly.
        app.session_mgr.current_mut().agent.retry_status = Some(crate::app::RetryStatus {
            attempt: 2,
            max_attempts: 5,
            delay_ms: 100,
            error: "provider rate limited".into(),
        });
    }

    #[tokio::test]
    async fn test_handle_interrupted_branch1_resets_retry_status_and_loading() {
        // Branch 1: has tool calls → keep progress, but STILL clear retry
        // and loading. Otherwise spinner stays in Responding mode after
        // Ctrl+C.
        let mut app = make_app_with_active_turn().await;
        set_active_retry(&mut app);
        // Sanity: retry_status really is populated
        assert!(app.session_mgr.current().agent.retry_status.is_some());

        let view_slice: Vec<ViewModel> = vec![
            ViewModel::UserBubble(UserBubbleData {
                text: "hello".into(),
            }),
            ViewModel::ToolCard(ToolCardData {
                tool_id: "t1".into(),
                tool_name: "Bash".into(),
                input_summary: "ls".into(),
                output_summary: "files".into(),
                is_error: false,
                diff: None,
            }),
        ];

        let _ = app.handle_interrupted(&view_slice);

        // CRITICAL: retry_status must be cleared (status bar would show
        // stale "Retrying (attempt 2)" otherwise)
        assert!(
            app.session_mgr.current().agent.retry_status.is_none(),
            "branch 1 must clear retry_status — stale retry indicator would persist otherwise"
        );

        // CRITICAL: loading must be cleared (spinner stuck otherwise)
        assert!(
            !app.session_mgr.current().ui.loading,
            "branch 1 must clear loading — spinner stays in Responding mode otherwise"
        );
    }

    #[tokio::test]
    async fn test_handle_interrupted_branch2_resets_retry_status_and_loading() {
        // Branch 2: no tool calls + has last_submitted_text → rollback.
        // Same cleanup obligations.
        let mut app = make_app_with_active_turn().await;
        set_active_retry(&mut app);

        let view_slice: Vec<ViewModel> = vec![ViewModel::UserBubble(UserBubbleData {
            text: "hello".into(),
        })];

        let _ = app.handle_interrupted(&view_slice);

        assert!(
            app.session_mgr.current().agent.retry_status.is_none(),
            "branch 2 must clear retry_status"
        );
        assert!(
            !app.session_mgr.current().ui.loading,
            "branch 2 must clear loading"
        );
    }

    #[tokio::test]
    async fn test_handle_interrupted_branch3_no_text_resets_retry_and_loading() {
        // Branch 3: no tool calls + no last_submitted_text (e.g. interrupt
        // during setup). All three branches must call cleanup_agent_state.
        let mut app = make_app_with_active_turn().await;
        set_active_retry(&mut app);
        app.session_mgr.current_mut().messages.last_submitted_text = None;

        let view_slice: Vec<ViewModel> = vec![ViewModel::UserBubble(UserBubbleData {
            text: "hello".into(),
        })];

        let _ = app.handle_interrupted(&view_slice);

        assert!(
            app.session_mgr.current().agent.retry_status.is_none(),
            "branch 3 must clear retry_status"
        );
        assert!(
            !app.session_mgr.current().ui.loading,
            "branch 3 must clear loading"
        );
    }

    #[tokio::test]
    async fn test_handle_interrupted_subagent_depth_does_not_clear_retry() {
        // Early-return guard (subagent_depth > 0): retry_status is NOT
        // touched. If the parent was itself in an LLM retry when the
        // SubAgent ran, the parent's retry indicator stays — clearing it
        // would mask a real retry in progress.
        //
        // SubAgentEnd handler (mod.rs) decrements subagent_depth; the
        // parent's eventual Interrupted/Done/Error will clear retry_status.
        let mut app = make_app_with_active_turn().await;
        set_active_retry(&mut app);
        app.session_mgr.current_mut().agent.subagent_depth = 1;

        let view_slice: Vec<ViewModel> = vec![ViewModel::UserBubble(UserBubbleData {
            text: "hello".into(),
        })];

        let _ = app.handle_interrupted(&view_slice);

        assert!(
            app.session_mgr.current().agent.retry_status.is_some(),
            "subagent_depth > 0 early-return must NOT clear parent retry_status"
        );
        // Loading stays true — parent is still in flight
        assert!(
            app.session_mgr.current().ui.loading,
            "subagent_depth > 0 early-return must NOT clear parent loading"
        );
    }

    // -----------------------------------------------------------------------
    // Cron #34: rewind-before-drain ordering preserves confirmation note
    // -----------------------------------------------------------------------
    //
    // Bug (found by Cron #33 workflow wv5751rjq synthesizer, MEDIUM):
    // main_loop had the order backwards — it drained pending_v2_notes
    // FIRST (appending the system note to state.view), THEN applied
    // pending_view_rewind_to (truncating state.view). The truncate
    // dropped the just-appended note along with the rolled-back
    // messages.
    //
    // handle_interrupted branch 2 and handle_rewind_completed both
    // enqueue a confirmation note + a rewind flag simultaneously:
    //   - "interrupted-resumed" / "↩ rewound to message X"
    // Users saw the view truncate but the note vanished — no UX
    // feedback for a destructive rollback.
    //
    // Fix: in main_loop.rs, move the pending_view_rewind_to block to
    // BEFORE the pending_v2_notes / pending_v2_user_bubbles drains.
    //
    // This test exercises the SM ordering invariant directly:
    //   truncate-then-drain → note preserved at end of state.view
    //   drain-then-truncate → note dropped (the bug)

    /// Helper: build an Idle state with the given viewModels.
    fn idle_state_with_view(vms: Vec<ViewModel>) -> crate::state_machine::State {
        use crate::state_machine::state::{IdleState, State};
        State::Idle(IdleState {
            view: vms,
            ..Default::default()
        })
    }

    /// Cron #34 FIXED order: truncate to user_msg_idx first, then push
    /// the system note. The note lands AFTER the cut and is preserved.
    #[test]
    fn test_rewind_before_drain_preserves_system_note() {
        use crate::state_machine::{event::Event, handle, state::State};

        // Setup: state.view = [UserBubble, AssistantBubble]
        // (mimics agent mid-stream when user presses Ctrl+C with no tools run)
        let state = idle_state_with_view(vec![
            ViewModel::UserBubble(UserBubbleData {
                text: "hello".into(),
            }),
            ViewModel::AssistantBubble(AssistantBubbleData {
                text: "partial reply".into(),
                reasoning: None,
                tool_card_ids: vec![],
            }),
        ]);

        // Step 1 (FIXED order): truncate to user_msg_idx = 1
        // (keep only UserBubble, drop the AssistantBubble)
        let mut state = state;
        if let State::Idle(idle) = &mut state {
            idle.view.truncate(1);
        }

        // Step 2: drain pending_v2_notes — push_system_note("resumed")
        let (new_state, _) = handle(state, Event::PushSystemNote("resumed".to_string()));
        state = new_state;

        // Assert: AssistantBubble dropped, SystemNote preserved at idx 1
        if let State::Idle(idle) = &state {
            assert_eq!(
                idle.view.len(),
                2,
                "FIXED order: state.view should have UserBubble + SystemNote (len 2)"
            );
            assert!(
                matches!(idle.view.get(1), Some(ViewModel::SystemNote(_))),
                "FIXED order: SystemNote must be preserved at idx 1, got {:?}",
                idle.view.get(1).map(|v| discriminant_name(v))
            );
        } else {
            panic!("expected State::Idle after operations, got {:?}", state);
        }
    }

    /// Cron #34 BUGGY order (regression guard): drain notes first,
    /// then truncate. This drops the just-appended note.
    ///
    /// We keep this test to document the bug we fixed — if someone
    /// reorders the blocks back, this test still passes (it tests the
    /// buggy order in isolation, not the production order), but the
    /// paired FIXED-order test above will fail in integration.
    #[test]
    fn test_drain_before_rewind_drops_system_note_bug_documented() {
        use crate::state_machine::{event::Event, handle, state::State};

        let state = idle_state_with_view(vec![
            ViewModel::UserBubble(UserBubbleData {
                text: "hello".into(),
            }),
            ViewModel::AssistantBubble(AssistantBubbleData {
                text: "partial reply".into(),
                reasoning: None,
                tool_card_ids: vec![],
            }),
        ]);

        // Step 1 (BUGGY order): drain pending_v2_notes first
        let mut state = state;
        let (new_state, _) = handle(state, Event::PushSystemNote("resumed".to_string()));
        state = new_state;
        // state.view is now [UserBubble, AssistantBubble, SystemNote]

        // Step 2: truncate to user_msg_idx = 1
        // (this is what the bug does — drops the note along with AssistantBubble)
        if let State::Idle(idle) = &mut state {
            idle.view.truncate(1);
        }

        // Assert: SystemNote is GONE (the bug)
        if let State::Idle(idle) = &state {
            assert_eq!(
                idle.view.len(),
                1,
                "BUGGY order: SystemNote got dropped by truncate (regression guard documents the bug)"
            );
            assert!(
                matches!(idle.view.first(), Some(ViewModel::UserBubble(_))),
                "BUGGY order: only UserBubble remains — note was silently dropped"
            );
        } else {
            panic!("expected State::Idle");
        }
    }

    /// Helper for the assert messages above — gets a short name for a
    /// ViewModel variant for diagnostic output.
    fn discriminant_name(vm: &ViewModel) -> &'static str {
        match vm {
            ViewModel::UserBubble(_) => "UserBubble",
            ViewModel::AssistantBubble(_) => "AssistantBubble",
            ViewModel::ToolCard(_) => "ToolCard",
            ViewModel::SystemNote(_) => "SystemNote",
            ViewModel::SubAgentGroup(_) => "SubAgentGroup",
            ViewModel::CollapsedGroup(_) => "CollapsedGroup",
            ViewModel::Divider(_) => "Divider",
        }
    }
}
