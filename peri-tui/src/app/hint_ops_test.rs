    mod slash_hint_detect_tests {
        use super::SlashHintState;

        #[test]
        fn test_detect_slash_at_line_start() {
            let (prefix, pos) = SlashHintState::detect("/code", 5).expect("行首 / 应被检测");
            assert_eq!(prefix, "code");
            assert_eq!(pos, 0);
        }

        #[test]
        fn test_detect_slash_after_space() {
            let (prefix, pos) = SlashHintState::detect("review /code", 12)
                .expect("空格后 / 应被检测");
            assert_eq!(prefix, "code");
            assert_eq!(pos, "review ".len());
        }

        #[test]
        fn test_detect_slash_only_no_prefix() {
            let (prefix, pos) = SlashHintState::detect("/", 1).expect("仅有 / 应被检测");
            assert_eq!(prefix, "");
            assert_eq!(pos, 0);
        }

        #[test]
        fn test_detect_slash_after_space_no_prefix() {
            let (prefix, pos) = SlashHintState::detect("hello /", 7)
                .expect("空格后 / 应被检测");
            assert_eq!(prefix, "");
            assert_eq!(pos, 6);
        }

        #[test]
        fn test_detect_slash_preceded_by_letter_is_none() {
            assert!(SlashHintState::detect("and/or", 5).is_none());
        }

        #[test]
        fn test_detect_slash_after_newline() {
            let text = "第一行\n/command";
            let (prefix, pos) = SlashHintState::detect(text, text.len())
                .expect("换行后 / 应被检测");
            assert_eq!(prefix, "command");
            assert_eq!(pos, "第一行\n".len());
        }

        #[test]
        fn test_detect_slash_cjk_space_before() {
            let text = "帮我 review /code";
            let (prefix, pos) = SlashHintState::detect(text, text.len())
                .expect("中文后空格 + / 应被检测");
            assert_eq!(prefix, "code");
            assert_eq!(pos, 14);
        }

        #[test]
        fn test_detect_cursor_before_slash_is_none() {
            assert!(SlashHintState::detect("abc /code", 3).is_none());
        }

        #[test]
        fn test_detect_cursor_at_zero_is_none() {
            assert!(SlashHintState::detect("/code", 0).is_none());
        }

        #[test]
        fn test_detect_invalid_chars_after_slash_is_none() {
            assert!(SlashHintState::detect("/co de", 5).is_none());
        }
    }

    fn make_skill(name: &str) -> SkillMetadataDto {
        SkillMetadataDto {
            name: name.to_string(),
            description: format!("{} skill", name),
            path: format!("/tmp/{}.md", name),
            source: SkillSourceDto::User,
            plugin_name: None,
            disabled: false,
        }
    }

    #[tokio::test]
    async fn test_candidates_count_slash_prefix_returns_cmd_plus_skills() {
        let (mut app, _handle) = crate::app::App::new_headless(80, 24).await;
        app.session_mgr.current_mut().ui.textarea = build_textarea(false);
        app.session_mgr.current_mut()
            .ui
            .textarea
            .insert_str("/");
        // 手动激活 slash hint（无用户键入事件时手动模拟）
        app.session_mgr.current_mut().ui.slash_hint.activate(String::new(), 0);
        app.session_mgr.current_mut()
            .commands
            .skills
            .push(make_skill("aaa-skill"));
        app.session_mgr.current_mut()
            .commands
            .skills
            .push(make_skill("zzz-skill"));

        let count = app.hint_candidates_count();
        let cmd_count = app.session_mgr.current_mut()
            .commands
            .command_registry
            .match_prefix("", &app.services.lc)
            .len();
        let expected = cmd_count + 2;
        assert_eq!(count, expected, "/ 前缀应返回命令数 + Skills 数");
    }

    #[tokio::test]
    async fn test_candidates_count_slash_prefix_filters_both() {
        let (mut app, _handle) = crate::app::App::new_headless(80, 24).await;
        app.session_mgr.current_mut().ui.textarea = build_textarea(false);
        app.session_mgr.current_mut()
            .ui
            .textarea
            .insert_str("/mo");
        app.session_mgr.current_mut().ui.slash_hint.activate("mo".to_string(), 0);
        app.session_mgr.current_mut()
            .commands
            .skills
            .push(make_skill("commit"));
        app.session_mgr.current_mut()
            .commands
            .skills
            .push(make_skill("model-skill"));

        let count = app.hint_candidates_count();
        assert!(
            count >= 2,
            "/mo 前缀应至少返回 model 命令 + model-skill 技能"
        );
    }

    #[tokio::test]
    async fn test_candidates_count_no_prefix_returns_zero() {
        let (mut app, _handle) = crate::app::App::new_headless(80, 24).await;
        app.session_mgr.current_mut().ui.textarea = build_textarea(false);
        app.session_mgr.current_mut()
            .ui
            .textarea
            .insert_str("hello");

        let count = app.hint_candidates_count();
        assert_eq!(count, 0, "无 slash hint 时应返回 0");
    }

    #[tokio::test]
    async fn test_hint_complete_command_at_cursor_0() {
        let (mut app, _handle) = crate::app::App::new_headless(80, 24).await;
        app.session_mgr.current_mut().ui.textarea = build_textarea(false);
        app.session_mgr.current_mut()
            .ui
            .textarea
            .insert_str("/m");
        app.session_mgr.current_mut().ui.slash_hint.activate("m".to_string(), 0);
        app.session_mgr.current_mut()
            .ui
            .hint_cursor = Some(0);

        app.hint_complete();
        let text: String = app.session_mgr.current_mut()
            .ui
            .textarea
            .lines()
            .iter()
            .map(|s| s.as_str())
            .collect();
        assert!(text.starts_with("/"), "补全后应以 / 开头，实际: {}", text);
        assert!(
            app.session_mgr.current_mut()
                .ui
                .hint_cursor
                .is_none(),
            "补全后 hint_cursor 应为 None"
        );
        assert!(
            !app.session_mgr.current_mut().ui.slash_hint.active,
            "补全后 slash_hint 应 inactive"
        );
    }

    #[tokio::test]
    async fn test_hint_complete_clears_hint_cursor() {
        let (mut app, _handle) = crate::app::App::new_headless(80, 24).await;
        app.session_mgr.current_mut().ui.textarea = build_textarea(false);
        app.session_mgr.current_mut()
            .ui
            .textarea
            .insert_str("/m");
        app.session_mgr.current_mut().ui.slash_hint.activate("m".to_string(), 0);
        app.session_mgr.current_mut()
            .ui
            .hint_cursor = Some(0);

        app.hint_complete();
        assert_eq!(
            app.session_mgr.current_mut()
                .ui
                .hint_cursor,
            None,
            "补全后 hint_cursor 应为 None"
        );
    }

    #[tokio::test]
    async fn test_hint_complete_skill_item() {
        let (mut app, _handle) = crate::app::App::new_headless(80, 24).await;
        app.session_mgr.current_mut().ui.textarea = build_textarea(false);
        app.session_mgr.current_mut()
            .ui
            .textarea
            .insert_str("/aaa");
        app.session_mgr.current_mut().ui.slash_hint.activate("aaa".to_string(), 0);
        app.session_mgr.current_mut()
            .commands
            .skills
            .push(make_skill("aaa-skill"));

        let items = app.build_hint_items();
        let idx = items
            .iter()
            .position(|it| it.name() == "aaa-skill")
            .expect("应有 aaa-skill 候选");
        app.session_mgr.current_mut()
            .ui
            .hint_cursor = Some(idx);

        app.hint_complete();
        let text: String = app.session_mgr.current_mut()
            .ui
            .textarea
            .lines()
            .iter()
            .map(|s| s.as_str())
            .collect();
        assert!(
            text.starts_with("/aaa-skill "),
            "应补全 Skill aaa-skill，实际: {}",
            text
        );
    }

    #[tokio::test]
    async fn test_hint_complete_inline_replaces_only_token() {
        let (mut app, _handle) = crate::app::App::new_headless(80, 24).await;
        // 用户输入: "帮我 review /code"
        app.session_mgr.current_mut().ui.textarea = build_textarea(false);
        app.session_mgr.current_mut()
            .ui
            .textarea
            .insert_str("帮我 review /code");
        // 帮我(6字节) + 空格 + review(7) + 空格 = 14, / 在字节 14
        app.session_mgr.current_mut().ui.slash_hint.activate("code".to_string(), 14);
        app.session_mgr.current_mut()
            .commands
            .skills
            .push(make_skill("code-review"));
        let items = app.build_hint_items();
        let idx = items.iter().position(|it| it.name() == "code-review")
            .expect("应有 code-review 候选");
        app.session_mgr.current_mut().ui.hint_cursor = Some(idx);

        app.hint_complete();
        let text: String = app.session_mgr.current_mut()
            .ui
            .textarea
            .lines()
            .iter()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(
            text,
            "帮我 review /code-review ",
            "应仅替换 /code token，保留前缀文本"
        );
        assert!(!app.session_mgr.current_mut().ui.slash_hint.active);
    }

    #[tokio::test]
    async fn test_hint_complete_inline_middle_of_text() {
        let (mut app, _handle) = crate::app::App::new_headless(80, 24).await;
        // 用户输入: "hello /code world"
        app.session_mgr.current_mut().ui.textarea = build_textarea(false);
        app.session_mgr.current_mut()
            .ui
            .textarea
            .insert_str("hello /code world");
        // hello(5) + 空格(1) = 6, / 在字节 6
        app.session_mgr.current_mut().ui.slash_hint.activate("code".to_string(), 6);
        app.session_mgr.current_mut()
            .commands
            .skills
            .push(make_skill("code-review"));
        let items = app.build_hint_items();
        let idx = items.iter().position(|it| it.name() == "code-review")
            .expect("应有 code-review 候选");
        app.session_mgr.current_mut().ui.hint_cursor = Some(idx);

        app.hint_complete();
        let text: String = app.session_mgr.current_mut()
            .ui
            .textarea
            .lines()
            .iter()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(
            text,
            "hello /code-review  world",
            "应仅替换中间的 /code token，保留前后文本"
        );
    }
