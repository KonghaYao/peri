

    fn make_server(name: &str, status: ClientStatus) -> ServerInfo {
        ServerInfo {
            name: name.to_string(),
            transport_type: "stdio".to_string(),
            status,
            tool_count: 3,
            resource_count: 2,
            oauth_status: Default::default(),
            source: None,
            url: None,
            plugin_source: None,
        }
    }

    async fn render_mcp_panel(servers: Vec<ServerInfo>) -> crate::ui::headless::HeadlessHandle {
        let (mut app, mut handle) = App::new_headless(120, 30).await;
        let panel = McpPanel::new(servers);
        app.global_panels
            .open(crate::app::panel_manager::PanelState::Mcp(Box::new(panel)));
        handle
            .terminal
            .draw(|f| crate::ui::main_ui::render(f, &mut app, None))
            .unwrap();
        handle
    }

    #[tokio::test]
    async fn test_mcp_panel_empty_server_list() {
        let handle = render_mcp_panel(vec![]).await;
        let snap = handle.snapshot().join("\n");
        assert!(snap.contains("No MCP servers"), "空 MCP 面板应显示引导文字");
    }

    #[tokio::test]
    async fn test_mcp_panel_server_list_with_items() {
        let handle = render_mcp_panel(vec![
            make_server("test-connected", ClientStatus::Connected),
            make_server("test-failed", ClientStatus::Failed("timeout".into())),
        ])
        .await;
        let snap = handle.snapshot().join("\n");
        assert!(snap.contains("test-connected"), "MCP 面板应显示服务器名称");
        assert!(snap.contains("connected"), "MCP 面板应显示 connected 状态");
    }

    #[tokio::test]
    async fn test_mcp_panel_detail_action_menu() {
        let (mut app, mut handle) = App::new_headless(120, 30).await;
        let mut srv = make_server("test-srv", ClientStatus::Connected);
        srv.transport_type = "http".to_string();
        srv.url = Some("https://example.com/mcp".to_string());
        let panel = McpPanel::new(vec![srv]);
        app.global_panels
            .open(crate::app::panel_manager::PanelState::Mcp(Box::new(panel)));
        app.mcp_panel_enter();

        match &app.global_panels.get::<McpPanel>().unwrap().view {
            McpPanelView::ServerDetail { actions, .. } => {
                assert!(
                    actions
                        .iter()
                        .any(|a| matches!(a, crate::app::DetailAction::ReAuthenticate)),
                    "HTTP 服务器应有 ReAuthenticate action"
                );
            }
            _ => panic!("应进入 ServerDetail 视图"),
        }

        handle
            .terminal
            .draw(|f| crate::ui::main_ui::render(f, &mut app, None))
            .unwrap();
        let snap = handle.snapshot().join("\n");
        assert!(snap.contains("test-srv"), "详情页应显示服务器名");
    }

    #[tokio::test]
    async fn test_mcp_panel_grouped_by_source() {
        let mut project_srv = make_server("project-srv", ClientStatus::Connected);
        project_srv.source = Some(ConfigSource::Project(std::path::PathBuf::from(
            "/project/.mcp.json",
        )));
        let mut global_srv = make_server("global-srv", ClientStatus::Connected);
        global_srv.source = Some(ConfigSource::Global(std::path::PathBuf::from(
            "/home/.peri/settings.json",
        )));

        let handle = render_mcp_panel(vec![project_srv, global_srv]).await;
        let snap = handle.snapshot().join("\n");
        assert!(snap.contains("project-srv"), "应显示项目级服务器");
        assert!(snap.contains("global-srv"), "应显示全局服务器");
    }

    #[tokio::test]
    async fn test_plugin_mcp_panel_enter_detail() {
        let (mut app, mut handle) = App::new_headless(120, 30).await;

        let mut plugin_srv = make_server("plugin:context7:context7", ClientStatus::Connected);
        plugin_srv.source = Some(ConfigSource::Plugin);
        plugin_srv.plugin_source = Some("context7@alpha".to_string());

        let panel = McpPanel::new(vec![plugin_srv]);
        app.global_panels
            .open(crate::app::panel_manager::PanelState::Mcp(Box::new(panel)));

        // Enter detail view
        app.mcp_panel_enter();

        // Should be in ServerDetail view
        match &app.global_panels.get::<McpPanel>().unwrap().view {
            McpPanelView::ServerDetail {
                server_name,
                actions,
                ..
            } => {
                assert_eq!(
                    server_name, "plugin:context7:context7",
                    "Server name should match"
                );
                assert!(!actions.is_empty(), "Should have actions");
            }
            _ => panic!("Should be in ServerDetail view"),
        }

        // Render the detail view
        handle
            .terminal
            .draw(|f| crate::ui::main_ui::render(f, &mut app, None))
            .unwrap();
        let snap = handle.snapshot().join("\n");
        assert!(
            snap.contains("plugin:context7:context7"),
            "Detail view should show server name"
        );
        assert!(
            snap.contains("Plugin"),
            "Detail view should show Plugin source"
        );
    }

    /// 验证多 server 时，进入第二个 server 的详情页显示的是对应 server 的数据
    #[tokio::test]
    async fn test_mcp_panel_detail_shows_correct_server_on_multi() {
        let (mut app, mut handle) = App::new_headless(120, 30).await;

        let mut srv_a = make_server("server-a", ClientStatus::Connected);
        srv_a.url = Some("https://a.example.com/mcp".to_string());
        srv_a.transport_type = "http".to_string();

        let mut srv_b = make_server("server-b", ClientStatus::Failed("connect err".into()));
        srv_b.url = Some("https://b.example.com/mcp".to_string());
        srv_b.transport_type = "http".to_string();

        let panel = McpPanel::new(vec![srv_a, srv_b]);
        app.global_panels
            .open(crate::app::panel_manager::PanelState::Mcp(Box::new(panel.clone())));

        // 选择第二个 server 并进入详情
        app.mcp_panel_move_down();
        app.mcp_panel_enter();

        // 验证进入了 server-b 的详情
        match &app.global_panels.get::<McpPanel>().unwrap().view {
            McpPanelView::ServerDetail { server_name, .. } => {
                assert_eq!(server_name, "server-b", "应进入 server-b 的详情页");
            }
            _ => panic!("应在 ServerDetail 视图"),
        }

        // 渲染并验证显示的是 server-b 的数据
        handle
            .terminal
            .draw(|f| crate::ui::main_ui::render(f, &mut app, None))
            .unwrap();
        let snap = handle.snapshot().join("\n");
        assert!(snap.contains("server-b"), "详情页标题应显示 server-b");
        assert!(
            snap.contains("https://b.example.com/mcp"),
            "详情页应显示 server-b 的 URL"
        );
        assert!(!snap.contains("server-a"), "详情页不应显示 server-a");
    }

    /// 验证未初始化 server 的详情页只显示 Reconnect 且正确渲染
    #[tokio::test]
    async fn test_mcp_panel_uninitialized_detail() {
        let (mut app, mut handle) = App::new_headless(120, 30).await;

        let mut uninit_srv = make_server("new-server", ClientStatus::Uninitialized);
        uninit_srv.url = Some("https://new.example.com/mcp".to_string());
        uninit_srv.transport_type = "http".to_string();

        let mut connected_srv = make_server("old-server", ClientStatus::Connected);
        connected_srv.url = Some("https://old.example.com/mcp".to_string());
        connected_srv.transport_type = "http".to_string();

        let panel = McpPanel::new(vec![connected_srv, uninit_srv]);
        app.global_panels
            .open(crate::app::panel_manager::PanelState::Mcp(Box::new(panel.clone())));

        // 排序后 "new-server" < "old-server"（字母序），uninit 在位置 0（默认 cursor=0）
        app.mcp_panel_enter();

        // 验证操作菜单只有 Reconnect
        match &app.global_panels.get::<McpPanel>().unwrap().view {
            McpPanelView::ServerDetail {
                server_name,
                actions,
                ..
            } => {
                assert_eq!(server_name, "new-server");
                assert_eq!(actions.len(), 1, "Uninitialized 应只有一个 action");
                assert!(
                    matches!(actions[0], DetailAction::Reconnect),
                    "唯一 action 应为 Reconnect"
                );
            }
            _ => panic!("应在 ServerDetail 视图"),
        }

        // 渲染并验证
        handle
            .terminal
            .draw(|f| crate::ui::main_ui::render(f, &mut app, None))
            .unwrap();
        let snap = handle.snapshot().join("\n");
        assert!(snap.contains("new-server"), "详情页标题应显示 new-server");
        assert!(
            snap.contains("not initialized"),
            "详情页应显示 not initialized 状态"
        );
        assert!(snap.contains("Reconnect"), "详情页应显示 Reconnect 操作");
    }
