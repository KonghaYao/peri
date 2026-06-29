    async fn render_headless_agent_empty() -> (App, crate::ui::headless::HeadlessHandle) {
        let (mut app, mut handle) = App::new_headless(120, 30).await;
        let panel = AgentPanel::new(vec![], None);
        app.session_mgr.current_mut()
            .session_panels
            .open(crate::app::panel_manager::PanelState::Agent(panel.clone()));
        app.session_mgr.current_mut()
            .session_panels
            .open(crate::app::panel_manager::PanelState::Agent(panel));
        handle
            .terminal
            .draw(|f| crate::ui::main_ui::render(f, &mut app, None))
            .unwrap();
        (app, handle)
    }

    #[tokio::test]
    async fn test_agent_empty_shows_guide() {
        let (_, handle) = render_headless_agent_empty().await;
        let snap = handle.snapshot().join("\n");
        // 空列表应显示引导提示（用 ASCII 子串避免 CJK 宽字符问题）
        assert!(
            snap.contains("agents/"),
            "空列表应显示 Agent 定义文件引导，实际:\n{}",
            snap
        );
    }

    #[tokio::test]
    async fn test_agent_panel_has_nav_hint() {
        let (_, handle) = render_headless_agent_empty().await;
        let snap = handle.snapshot().join("\n");
        // 面板内或状态栏应包含导航相关提示
        let has_nav = snap.contains("导航") || snap.contains("选择") || snap.contains("Enter");
        assert!(has_nav, "Agent 面板应包含操作提示，实际:\n{}", snap);
    }
