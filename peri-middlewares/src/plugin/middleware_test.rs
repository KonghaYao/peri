    fn make_loaded_plugin(name: &str) -> LoadedPlugin {
        LoadedPlugin {
            name: name.into(),
            version: "1.0.0".into(),
            install_path: PathBuf::new(),
            manifest: make_manifest_with_commands(vec![]),
            commands: vec![],
            skills_roots: vec![],
            agents_dirs: vec![],
            mcp_servers: HashMap::new(),
            data_path: PathBuf::new(),
            hooks_config: None,
            marketplace: String::new(),
        }
    }

    #[test]
    fn test_middleware_name() {
        let mw = PluginMiddleware::new(vec![]);
        assert_eq!(Middleware::name(&mw), "PluginMiddleware");
    }

    #[tokio::test]
    async fn test_middleware_before_agent_noop() {
        let mw = PluginMiddleware::new(vec![]);
        let mut state = AgentState::new("/tmp");
        let result = mw.before_agent(&mut state).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_middleware_plugins_accessor() {
        let mw = PluginMiddleware::new(vec![make_loaded_plugin("test")]);
        assert_eq!(mw.plugins().len(), 1);
    }
