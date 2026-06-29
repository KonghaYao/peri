    async fn render_headless_hooks_empty() -> (App, crate::ui::headless::HeadlessHandle) {
        let (mut app, mut handle) = App::new_headless(120, 30).await;
        let panel = HooksPanel::new(vec![]);
        app.session_mgr.current_mut()
            .session_panels
            .open(crate::app::panel_manager::PanelState::Hooks(panel.clone()));
        app.session_mgr.current_mut()
            .session_panels
            .open(crate::app::panel_manager::PanelState::Hooks(panel));
        handle
            .terminal
            .draw(|f| crate::ui::main_ui::render(f, &mut app, None))
            .unwrap();
        (app, handle)
    }

    #[tokio::test]
    async fn test_hooks_empty_shows_guide() {
        let (_, handle) = render_headless_hooks_empty().await;
        let snap = handle.snapshot().join("\n");
        assert!(
            snap.contains("none configured") || snap.contains("No hooks"),
            "empty panel should show guide, actual:\n{}",
            snap
        );
    }

    #[tokio::test]
    async fn test_hooks_empty_has_panel_title() {
        let (_, handle) = render_headless_hooks_empty().await;
        let snap = handle.snapshot().join("\n");
        assert!(
            snap.contains("Hooks"),
            "panel should have Hooks title, actual:\n{}",
            snap
        );
    }

    #[tokio::test]
    async fn test_hooks_panel_with_data() {
        let (mut app, mut handle) = App::new_headless(120, 30).await;

        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "echo hello"
        }))
        .unwrap();

        let registered = RegisteredHook {
            hook,
            event: HookEvent::PreToolUse,
            matcher: Some("Bash".to_string()),
            plugin_name: "test-plugin".to_string(),
            plugin_id: "test-plugin".to_string(),
            plugin_root: PathBuf::from("/tmp/test"),
            plugin_data_dir: PathBuf::from("/tmp/test-data"),
            plugin_options: HashMap::new(),
        };

        app.session_mgr.current_mut()
            .session_panels
            .open(crate::app::panel_manager::PanelState::Hooks(
                HooksPanel::new(vec![registered.clone()]),
            ));
        app.session_mgr.current_mut()
            .session_panels
            .open(crate::app::panel_manager::PanelState::Hooks(
                HooksPanel::new(vec![registered]),
            ));
        handle
            .terminal
            .draw(|f| crate::ui::main_ui::render(f, &mut app, None))
            .unwrap();

        let snap = handle.snapshot().join("\n");
        assert!(
            snap.contains("PreToolUse"),
            "panel should show PreToolUse event, actual:\n{}",
            snap
        );
        assert!(
            snap.contains("1 hooks"),
            "panel should show hook count, actual:\n{}",
            snap
        );
    }
