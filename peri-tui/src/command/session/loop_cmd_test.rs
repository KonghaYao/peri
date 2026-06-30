    async fn headless_app() -> App {
        App::new_headless(80, 24).await.0
    }

    /// 从 `Vec<Effect>` 中提取首个 `PushSystemNote` 文本（若有）。
    fn first_system_note_text(effects: &[Effect]) -> Option<String> {
        for e in effects {
            if let Effect::PushSystemNote(t) = e {
                return Some(t.clone());
            }
        }
        None
    }

    #[tokio::test]
    async fn test_loop_cmd_empty_args_shows_usage() {
        let mut app = headless_app().await;
        let cmd = LoopCommand;
        let effects = cmd.execute(&mut app, "");
        let text = first_system_note_text(&effects)
            .expect("空参数应返回 PushSystemNote 用法提示");
        assert!(
            text.contains("用法") || text.contains("Usage"),
            "空参数应显示用法提示，实际: {}",
            text
        );
    }

    #[tokio::test]
    async fn test_loop_cmd_empty_whitespace_shows_usage() {
        let mut app = headless_app().await;
        let cmd = LoopCommand;
        let effects = cmd.execute(&mut app, "   ");
        let text = first_system_note_text(&effects)
            .expect("纯空格参数应返回 PushSystemNote 用法提示");
        assert!(
            text.contains("用法") || text.contains("Usage"),
            "纯空格参数应显示用法提示，实际: {}",
            text
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_loop_cmd_valid_args_submits_message() {
        // Cron #26 step 7e.7: submit_message 不再写 v1 view_messages。
        // UserBubble 通过 push_user_bubble 入队到 pending_v2_user_bubbles，
        // 由 main_loop 通过 Event::PushUserBubble 路由到 v2 state.view。
        let mut app = headless_app().await;
        let cmd = LoopCommand;
        cmd.execute(&mut app, "每隔5分钟提醒我喝水");
        // pending_v2_user_bubbles 应包含提交的 prompt
        let pending = &app.session_mgr.current().messages.pending_v2_user_bubbles;
        assert_eq!(
            pending.len(),
            1,
            "/loop 命令应入队 1 个 UserBubble 到 pending_v2_user_bubbles"
        );
        // 检查提交的消息包含 cron_register 指令
        assert!(
            pending[0].contains("cron_register"),
            "提交的消息应包含 cron_register 指令，实际: {}",
            pending[0]
        );
    }

    #[test]
    fn test_loop_cmd_name() {
        let cmd = LoopCommand;
        assert_eq!(cmd.name(), "loop");
    }

    #[test]
    fn test_loop_cmd_description_not_empty() {
        let cmd = LoopCommand;
        let lc = crate::i18n::LcRegistry::default();
        assert!(!cmd.description(&lc).is_empty());
    }
