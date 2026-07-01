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

        // Cron #41 (Phase 2.6 step 7e.4): retired apply_rebuild_all(0, view_msgs)
        // — it only wrote to v1 view_messages, but production render reads v2
        // state.view exclusively. ACP emits CompactCompleted then returns
        // (no follow-up ViewCommit), so:
        //   - state.view kept showing pre-compact (stale) messages
        //   - the "✻ Compact Done" label was invisible (only in v1 view_messages)
        //
        // Fix mirrors Cron #29 P1 (handle_rewind_completed) + Cron #28 (handle_error):
        //   (a) pending_view_rewind_to = Some(0) → main_loop truncates state.view
        //       to 0 (clearing stale pre-compact content)
        //   (b) push_system_note routes compact_label through pending_v2_notes
        //       → SM Event::PushSystemNote → state.view
        //
        // After this fix: user sees cleared message area + "✻ Compact Done" note.
        // The compacted transcript re-appears on the next ViewCommit triggered by
        // user's next input. Strictly better than showing stale pre-compact content
        // (which made compact look broken).
        self.global_ui.pending_view_rewind_to = Some(0);
        self.push_system_note(compact_label);

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

        // Cron #42 (Phase 2.6 step 7e.5): retired apply_rebuild_all(0, view_msgs)
        // — it only wrote to v1 view_messages, but production render reads v2
        // state.view exclusively. Cron #29 P1 fix below (pending_view_rewind_to +
        // push_system_note) is the sole load-bearing v2 route.
        //
        // Audit confirmed no headless test asserts view_messages after rewind
        // (unlike compact which had 2 headless tests migrated in Cron #41),
        // so this retirement needs no test migration.
        let label = format!("↩ {summary}");

        // Cron #29 P1 fix: route rewind summary + state.view truncation to v2.
        //
        // Bug (workflow weo7g6w2n P1): historically apply_rebuild_all only
        // updated v1 view_messages. ACP layer emits ONLY RewindCompleted —
        // no subsequent ViewCommit/TurnCommitted (peri-acp/src/session/
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
        // Symmetric with Cron #28 handle_error fix (lifecycle.rs:240) +
        // Cron #41 handle_compact_completed fix (agent_compact.rs:43).
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

    /// Cron #41 P1 fix：handle_compact_completed 必须设置 pending_view_rewind_to
    /// = Some(0)，让 main_loop 截断 v2 state.view（清除 stale pre-compact 内容）。
    ///
    /// 历史 bug：apply_rebuild_all 只更新 v1 view_messages。ACP emit 完
    /// CompactCompleted 后直接返回——没有后续 ViewCommit。生产渲染独占
    /// 读 v2 state.view，导致 /compact 后：
    ///   - 用户仍看到被压缩前的旧消息（state.view 未清空）
    ///   - "✻ Compact Done" 反馈不可见（label 只在 v1 view_messages）
    ///
    /// 修复镜像 Cron #29 P1（handle_rewind_completed）+ Cron #28（handle_error）。
    #[tokio::test]
    async fn test_handle_compact_completed_sets_pending_view_rewind() {
        let mut app = make_app().await;

        let files = vec![];
        let skills = vec![];
        let messages = vec![];

        let _ = app.handle_compact_completed(
            "auto-compact summary".to_string(),
            files,
            skills,
            0, // micro_cleared = 0 → full compact path
            messages,
        );

        // 核心断言 1：pending_view_rewind_to 已设置为 Some(0)
        assert_eq!(
            app.global_ui.pending_view_rewind_to,
            Some(0),
            "handle_compact_completed 必须设置 pending_view_rewind_to = Some(0)，\
             否则 main_loop 不会截断 v2 state.view，stale pre-compact 消息会持续显示"
        );
    }

    /// Cron #41 P1 fix：handle_compact_completed 必须把 "✻ Compact Done" label
    /// 通过 push_system_note 路由到 v2 state.view（经由 pending_v2_notes 队列）。
    #[tokio::test]
    async fn test_handle_compact_completed_routes_label_to_v2() {
        let mut app = make_app().await;

        let files = vec![peri_acp::event::CompactFileInfoDto {
            path: "src/main.rs".to_string(),
            lines: 100,
        }];
        let skills = vec!["blog-writer".to_string()];
        let messages = vec![];

        let _ =
            app.handle_compact_completed("auto-compact".to_string(), files, skills, 0, messages);

        // 核心断言 2：label 入队到 pending_v2_notes
        let pending = &app.session_mgr.current().messages.pending_v2_notes;
        assert_eq!(
            pending.len(),
            1,
            "handle_compact_completed must enqueue the label to pending_v2_notes"
        );
        // 必须包含 ✻ 标志 + files + skills
        assert!(
            pending[0].contains("✻"),
            "enqueued note must contain the ✻ Compact Done marker, got: {}",
            pending[0]
        );
        assert!(
            pending[0].contains("src/main.rs"),
            "enqueued note must contain the file path, got: {}",
            pending[0]
        );
        assert!(
            pending[0].contains("blog-writer"),
            "enqueued note must contain the skill name, got: {}",
            pending[0]
        );
    }

    /// Cron #41：handle_compact_completed 的 micro_cleared > 0 早返回路径
    /// 不应设置 pending_view_rewind_to（不触发 state.view 截断）。
    /// 那条路径通过 push_system_note 单独路由 "auto-cleared N entries" note，
    /// 已在 line 25 调用 push_system_note，是正确的 v2 路径。
    #[tokio::test]
    async fn test_handle_compact_completed_micro_cleared_does_not_set_rewind_flag() {
        let mut app = make_app().await;

        let messages = vec![BaseMessage::human("placeholder".to_string())];
        let _ = app.handle_compact_completed(
            "micro-compact".to_string(),
            vec![],
            vec![],
            5, // micro_cleared > 0 → early-return path
            messages,
        );

        // 早返回路径不设置 flag
        assert_eq!(
            app.global_ui.pending_view_rewind_to, None,
            "micro_cleared > 0 early-return path must NOT set pending_view_rewind_to"
        );
    }
}
