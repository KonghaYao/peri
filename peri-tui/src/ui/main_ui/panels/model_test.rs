    async fn render_headless_model_no_provider() -> (App, crate::ui::headless::HeadlessHandle) {
        let (mut app, mut handle) = App::new_headless(120, 30).await;
        let panel = ModelPanel {
            provider_name: String::new(),
            active_tab: AliasTab::Opus,
            buf_thinking_effort: "medium".to_string(),
            buf_max_tokens: 32000,
            buf_context_1m: false,
            cursor: ROW_OPUS,
        };
        app.session_mgr.current_mut()
            .session_panels
            .open(crate::app::panel_manager::PanelState::Model(panel.clone()));
        handle
            .terminal
            .draw(|f| crate::ui::main_ui::render(f, &mut app, None))
            .unwrap();
        (app, handle)
    }

    #[tokio::test]
    async fn test_model_panel_renders_select_model_title() {
        let (_, handle) = render_headless_model_no_provider().await;
        let snap = handle.snapshot().join("\n");
        assert!(
            snap.contains("Select model"),
            "Panel should show 'Select model' title, got:\n{}",
            snap
        );
    }

    #[tokio::test]
    async fn test_model_panel_shows_effort() {
        let (_, handle) = render_headless_model_no_provider().await;
        let snap = handle.snapshot().join("\n");
        assert!(
            snap.contains("Effort"),
            "Panel should show effort setting, got:\n{}",
            snap
        );
    }
