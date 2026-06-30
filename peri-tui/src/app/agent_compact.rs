use peri_agent::messages::BaseMessage; // P4b: type-dependency

use super::*;

impl App {
    pub(crate) fn handle_compact_started(&mut self) -> (bool, bool, bool) {
        self.session_mgr.current_mut().focused_instance_id = None;
        self.session_mgr.current_mut().ui.bg_bar_cursor = None;
        self.session_mgr.current_mut().ui.text_selection.clear();
        self.set_loading(true);
        self.push_system_note(self.services.lc.tr("app-compact-started"));
        (true, false, false)
    }

    pub(crate) fn handle_compact_completed(
        &mut self,
        _summary: String,
        files: Vec<peri_acp::event::CompactFileInfoDto>,
        skills: Vec<String>,
        micro_cleared: usize,
        messages: Vec<BaseMessage>,
    ) -> (bool, bool, bool) {
        if micro_cleared > 0 {
            self.session_mgr.current_mut().agent.origin_messages = messages;
            self.push_system_note(self.services.lc.tr_args(
                "app-compact-auto-cleared",
                &[("count".into(), (micro_cleared as i64).into())],
            ));
            return (true, false, false);
        }

        self.session_mgr.current_mut().ui.text_selection.clear();

        let mut label_lines = vec![format!("✻ {}", self.services.lc.tr("app-compact-done"))];
        for f in &files {
            label_lines.push(format!("  ⎿  Read {} ({} lines)", f.path, f.lines));
        }
        if !skills.is_empty() {
            label_lines.push(format!("  ⎿  Skill: {}", skills.join(", ")));
        }
        let compact_label = label_lines.join("\n");

        self.session_mgr.current_mut().agent.origin_messages = messages.clone();

        // P5: replace view_messages (SystemNote anchor tracking retired in Phase 2.5)
        let view_msgs = vec![MessageViewModel::system(compact_label)];
        self.session_mgr.current_mut().messages.round_start_vm_idx = 0;
        self.apply_rebuild_all(0, view_msgs);

        (true, false, false)
    }

    pub(crate) fn handle_compact_error(&mut self, msg: String) -> (bool, bool, bool) {
        self.set_loading(false);
        self.push_system_note(
            self.services
                .lc
                .tr_args("app-compact-failed", &[("error".into(), msg.into())]),
        );

        (true, false, false)
    }

    pub(crate) fn handle_rewind_completed(
        &mut self,
        summary: String,
        messages: Vec<BaseMessage>,
    ) -> (bool, bool, bool) {
        self.session_mgr.current_mut().agent.origin_messages = messages.clone();

        // P5: replace view_messages (SystemNote anchor tracking retired in Phase 2.5)
        let cwd = self.services.cwd.clone();
        let mut view_msgs = super::messages_to_view_models(&messages, &cwd);
        let label = format!("↩ {summary}");
        view_msgs.push(MessageViewModel::system(label.clone()));
        self.session_mgr.current_mut().messages.round_start_vm_idx = 0;
        self.apply_rebuild_all(0, view_msgs);

        // Cron #29 P1 fix: route rewind summary + state.view truncation to v2.
        //
        // Bug (workflow weo7g6w2n P1): apply_rebuild_all only updates v1
        // view_messages. ACP layer emits ONLY RewindCompleted — no
        // subsequent ViewCommit/TurnCommitted (peri-acp/src/session/
        // command/rewind.rs emits RewindCompleted then returns). Production
        // render reads v2 state.view exclusively, so:
        //   - state.view still shows the pre-rewind (removed) messages
        //   - the "↩ {summary}" SystemNote is invisible
        // User perceives rewind as broken — old messages stay, no feedback.
        //
        // Fix: mirror Cron #28 handle_error pattern. Two parts:
        //   (a) pending_view_rewind_to = Some(0) → main_loop truncates
        //       state.view to 0 (clearing stale pre-rewind content)
        //   (b) push_system_note routes the label through pending_v2_notes
        //       → SM Event::PushSystemNote → state.view
        //
        // After this fix: user sees an empty message area + "↩ Rewound N
        // messages" note. The rewound transcript (the messages still valid
        // after rewind) re-appears on the next ViewCommit triggered by
        // user's next input. This is strictly better than showing stale
        // pre-rewind content which makes rewind look broken.
        //
        // Symmetric with Cron #28 handle_error fix (lifecycle.rs:240).
        self.global_ui.pending_view_rewind_to = Some(0);
        self.push_system_note(label);

        if let Some(text) = self.session_mgr.current_mut().ui.pending_rewind_text.take() {
            self.session_mgr.current_mut().ui.textarea.insert_str(&text);
        }

        (true, false, false)
    }
}

// ── Cron #29 regression tests ─────────────────────────────────────────────
//
// 验证 handle_rewind_completed 的 v2 路由修复（P1 bug）：
//   1. pending_view_rewind_to 被设置为 Some(0)，让 main_loop 截断 state.view
//   2. "↩ {summary}" label 通过 push_system_note 入队到 pending_v2_notes
//
// 历史 bug：apply_rebuild_all 只更新 v1 view_messages。ACP 层的 rewind 命令
// emit 完 RewindCompleted 后直接返回——没有后续 ViewCommit。生产渲染独占
// 读 v2 state.view，导致 /rewind N 后：
//   - 用户仍看到被回滚的消息（state.view 未清空）
//   - "↩ N messages" 反馈不可见（label 只在 v1 view_messages）
//
// 修复镜像 Cron #28 handle_error 模式（lifecycle.rs:240）。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::message_view::MessageViewModel;

    /// 构造一个 headless App，模拟 "用户在会话中" 的状态。
    async fn make_app() -> App {
        let (app, _handle) = App::new_headless(80, 24).await;
        app
    }

    /// Cron #29 P1 fix：handle_rewind_completed 必须设置 pending_view_rewind_to
    /// = Some(0)，让 main_loop 截断 v2 state.view（清除 stale pre-rewind 内容）。
    #[tokio::test]
    async fn test_handle_rewind_completed_sets_pending_view_rewind() {
        let mut app = make_app().await;

        // 模拟 rewind 完成：messages 是回滚后的新 transcript（空表示回滚到初始）
        let summary = "Rewound 3 messages".to_string();
        let messages: Vec<BaseMessage> = Vec::new();

        let _ = app.handle_rewind_completed(summary, messages);

        // 核心断言 1：pending_view_rewind_to 已设置为 Some(0)
        assert_eq!(
            app.global_ui.pending_view_rewind_to,
            Some(0),
            "handle_rewind_completed 必须设置 pending_view_rewind_to = Some(0)，\
             否则 main_loop 不会截断 v2 state.view，stale pre-rewind 消息会持续显示"
        );
    }

    /// Cron #29 P1 fix：handle_rewind_completed 必须把 "↩ {summary}" label
    /// 通过 push_system_note 路由到 v2 state.view（经由 pending_v2_notes 队列）。
    #[tokio::test]
    async fn test_handle_rewind_completed_routes_label_to_v2() {
        let mut app = make_app().await;

        let summary = "Rewound 5 messages".to_string();
        let messages: Vec<BaseMessage> = Vec::new();

        let _ = app.handle_rewind_completed(summary.clone(), messages);

        // 核心断言 2：label 入队到 pending_v2_notes（由 main_loop drain 并 push 到 state.view）
        let pending = &app.session_mgr.current().messages.pending_v2_notes;
        assert_eq!(
            pending.len(),
            1,
            "handle_rewind_completed must enqueue the label to pending_v2_notes"
        );
        assert!(
            pending[0].contains(&summary),
            "enqueued note must contain the original summary, got: {}",
            pending[0]
        );
        assert!(
            pending[0].contains("↩"),
            "enqueued note must contain the ↩ prefix, got: {}",
            pending[0]
        );
    }

    /// Cron #29 P1 fix：即使 messages 参数为空（回滚到初始），也必须设置
    /// pending_view_rewind_to（部分实现可能 guard on messages.is_empty()）。
    #[tokio::test]
    async fn test_handle_rewind_completed_with_empty_messages_still_sets_flag() {
        let mut app = make_app().await;

        let _ = app.handle_rewind_completed("Rewound everything".to_string(), Vec::new());

        // 即使 messages 为空，flag 仍必须设置（state.view 仍需截断清除旧内容）
        assert_eq!(
            app.global_ui.pending_view_rewind_to,
            Some(0),
            "empty messages must not skip the pending_view_rewind_to flag"
        );
    }

    /// Cron #29 P1 fix：rewound transcript 即使有内容，pending_view_rewind_to
    /// 仍是 Some(0)——main_loop 截断到 0，然后 label 通过 PushSystemNote 加入。
    /// 这是有意的权衡：rewound messages 在下次 ViewCommit 才进入 state.view，
    /// 而当前窗口用户看到的是 "空 + 反馈 note"，比看到 stale pre-rewind 内容更好。
    #[tokio::test]
    async fn test_handle_rewind_completed_with_messages_still_truncates_to_zero() {
        let mut app = make_app().await;

        // 模拟 rewind 后仍有 2 条消息（部分回滚）
        let messages = vec![
            BaseMessage::human("first message".to_string()),
            BaseMessage::ai("first response".to_string()),
        ];

        let _ = app.handle_rewind_completed("Rewound 1 message".to_string(), messages);

        // flag 仍为 Some(0)——我们截断 state.view 到 0，让 rewound messages
        // 通过下次 ViewCommit 进入（保持简单一致的语义）
        assert_eq!(
            app.global_ui.pending_view_rewind_to,
            Some(0),
            "pending_view_rewind_to is Some(0) regardless of messages count — \
             consistent truncation semantics"
        );
    }
}
