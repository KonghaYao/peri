    async fn render_headless_hitl_single() -> (App, crate::ui::headless::HeadlessHandle) {
        let (mut app, mut handle) = App::new_headless(120, 30).await;
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let items = vec![BatchItem {
            tool_name: "Bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
        }];
        let prompt = HitlBatchPrompt::new(items, tx);
        app.session_mgr.current_mut()
            .agent
            .interaction_prompt = Some(InteractionPrompt::Approval(prompt));
        handle
            .terminal
            .draw(|f| crate::ui::main_ui::render(f, &mut app, None, None))
            .unwrap();
        (app, handle)
    }

    async fn render_headless_hitl_multi() -> (App, crate::ui::headless::HeadlessHandle) {
        let (mut app, mut handle) = App::new_headless(120, 30).await;
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let items = vec![
            BatchItem {
                tool_name: "Bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
            },
            BatchItem {
                tool_name: "Write".to_string(),
                input: serde_json::json!({"path": "test.rs"}),
            },
        ];
        let prompt = HitlBatchPrompt::new(items, tx);
        app.session_mgr.current_mut()
            .agent
            .interaction_prompt = Some(InteractionPrompt::Approval(prompt));
        // 通过 main_ui::render 渲染完整布局，确保面板高度正确
        handle
            .terminal
            .draw(|f| crate::ui::main_ui::render(f, &mut app, None, None))
            .unwrap();
        (app, handle)
    }

    #[tokio::test]
    async fn test_hitl_single_no_single_letter_hints() {
        let (_, handle) = render_headless_hitl_single().await;
        let snap = handle.snapshot().join("\n");
        // 不应出现单字母快捷键 y 或 n（作为独立快捷键提示）
        assert!(
            !snap.contains(":批准") || !snap.contains("y:"),
            "不应显示 y:批准 单字母快捷键"
        );
        assert!(
            !snap.contains(":拒绝") || !snap.contains("n:"),
            "不应显示 n:拒绝 单字母快捷键"
        );
        // 应显示合规快捷键
        assert!(handle.contains("Space"), "应显示 Space 快捷键");
        assert!(handle.contains("Enter"), "应显示 Enter 快捷键");
    }

    #[tokio::test]
    async fn test_hitl_multi_shows_enter_hint() {
        let (_, handle) = render_headless_hitl_multi().await;
        let snap = handle.snapshot().join("\n");
        // 多项应显示 Enter 确认
        assert!(
            snap.contains("Enter"),
            "多项应显示 Enter 快捷键，实际:\n{}",
            snap
        );
    }
