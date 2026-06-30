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
            // Cron #23 P1 fix: 请求 main_loop 截断 v2 state.view 到 user_msg_idx，
            // 与 v1 view_messages 的截断保持一致。否则 stale UserBubble + 部分
            // AssistantBubble 会持续存在直到下一个 view-commit（用户感知为
            // "按 Esc 回滚后消息还在"）。main_loop 在 handle_acp_event 返回后
            // 消费此 flag，仅对 Idle/Streaming 生效（Modal/Switching 跳过）。
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
        // Cron #28: route error message through v2 state.view via
        // push_system_note (mirrors cron #24/cron #26 queue-and-drain pattern).
        // Without this, production render (which reads v2 state.view
        // exclusively) shows NOTHING when the agent errors out — the
        // error ToolBlock above only reaches v1 view_messages, which is
        // not on the production render path. Phase 2.6 will retire the
        // v1 push above; this v2 routing is the load-bearing path.
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
    use crate::ui::message_view::MessageViewModel;
    use peri_acp_types::view_model::{
        AssistantBubbleData, ToolCardData, UserBubbleData, ViewModel,
    };

    /// 构造一个 headless App，模拟 "用户提交后正在流式中" 的状态。
    async fn make_app_with_active_turn() -> App {
        let (mut app, _handle) = App::new_headless(80, 24).await;

        // 模拟用户提交 "hello" —— 复刻 agent_submit.rs 的关键字段
        let user_vm = MessageViewModel::user("hello".to_string());
        app.apply_add_message(user_vm);
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

        // v1 view_messages 也应被截断
        assert_eq!(
            app.session_mgr.current().messages.view_messages.len(),
            0,
            "v1 view_messages 应被 apply_rebuild_all(0, []) 截断为 0"
        );

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

        // v1 view_messages 不应被截断（保留 UserBubble + 后续工作）
        assert_eq!(
            app.session_mgr.current().messages.view_messages.len(),
            1,
            "分支 1 应保留 UserBubble 在 view_messages 中"
        );
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

        // v1 view_messages 也应被截断
        assert_eq!(
            app.session_mgr.current().messages.view_messages.len(),
            0,
            "v1 view_messages 应被截断（包括 AssistantBubble 也被移除）"
        );
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
}
