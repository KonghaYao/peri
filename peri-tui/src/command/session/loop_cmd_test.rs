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
        let mut app = headless_app().await;
        let initial_len = app.session_mgr.current_mut()
            .messages
            .view_messages
            .len();
        let cmd = LoopCommand;
        cmd.execute(&mut app, "每隔5分钟提醒我喝水");
        // submit_message 会添加一条 user 消息到 view_messages
        assert!(
            app.session_mgr.current_mut()
                .messages
                .view_messages
                .len()
                > initial_len,
            "有参数时应提交消息给 Agent"
        );
        // 检查提交的消息包含 cron_register 指令
        let text = format!(
            "{:?}",
            app.session_mgr.current_mut()
                .messages
                .view_messages
        );
        assert!(
            text.contains("cron_register"),
            "提交的消息应包含 cron_register 指令，实际: {}",
            text
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
