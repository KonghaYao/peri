    #[test]
    fn test_mask_api_key() {
        assert_eq!(mask_api_key(""), "");
        assert_eq!(mask_api_key("short"), "•••••");
        assert_eq!(mask_api_key("sk-ant-test-key-12345"), "sk-a••••2345");
    }

    async fn render_headless(
        wizard: SetupWizardPanel,
    ) -> (App, crate::ui::headless::HeadlessHandle) {
        let (mut app, mut handle) = App::new_headless(120, 30).await;
        app.global_ui.setup_wizard = Some(wizard);
        handle
            .terminal
            .draw(|f| crate::ui::main_ui::render(f, &mut app, None))
            .unwrap();
        (app, handle)
    }

    #[tokio::test]
    async fn test_render_step_choose() {
        let mut wizard = SetupWizardPanel::new();
        wizard.step = SetupStep::Choose;
        let (_, handle) = render_headless(wizard).await;
        assert!(handle.contains("Custom API"));
        assert!(handle.contains("Claude Code"));
    }

    #[tokio::test]
    async fn test_render_step_form() {
        let mut wizard = SetupWizardPanel::new();
        wizard.step = SetupStep::Form;
        let (_, handle) = render_headless(wizard).await;
        assert!(handle.contains("Configure"));
        assert!(handle.contains("Submit"));
    }

    #[tokio::test]
    async fn test_render_done_page() {
        let mut wizard = SetupWizardPanel::new();
        wizard.step = SetupStep::Done;
        wizard.providers[0].field_api_key.set_value("sk-ant-test1234xyz");
        let (_, handle) = render_headless(wizard).await;
        assert!(handle.contains("Complete"));
    }
