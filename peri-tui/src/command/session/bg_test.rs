    async fn headless_app() -> App {
        App::new_headless(80, 24).await.0
    }

    /// 从 `Vec<Effect>` 中提取首个 `PushSystemNote` 文本（若有）。
    fn first_system_note_text(effects: &[Effect]) -> Option<String> {
        for e in effects {
            if let Effect::ShowNotification(t) = e {
                return Some(t.clone());
            }
        }
        None
    }

    #[tokio::test]
    async fn test_bg_cmd_empty_args_shows_usage() {
        let mut app = headless_app().await;
        let cmd = BgCommand;
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
    async fn test_bg_cmd_empty_whitespace_shows_usage() {
        let mut app = headless_app().await;
        let cmd = BgCommand;
        let effects = cmd.execute(&mut app, "   ");
        let text = first_system_note_text(&effects)
            .expect("纯空格参数应返回 PushSystemNote 用法提示");
        assert!(
            text.contains("用法") || text.contains("Usage"),
            "纯空格参数应显示用法提示，实际: {}",
            text
        );
    }

    #[test]
    fn test_bg_cmd_name() {
        let cmd = BgCommand;
        assert_eq!(cmd.name(), "bg");
    }

    #[test]
    fn test_bg_cmd_aliases() {
        let cmd = BgCommand;
        assert!(cmd.aliases().contains(&"background"));
    }

    #[test]
    fn test_bg_cmd_description_not_empty() {
        let cmd = BgCommand;
        let lc = crate::i18n::LcRegistry::default();
        assert!(!cmd.description(&lc).is_empty());
    }
