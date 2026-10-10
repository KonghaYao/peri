use super::*;

async fn scope_config(tmp: &tempfile::TempDir) -> AcpServerConfig {
    let config = make_peri_config_with_provider(make_provider_config(
        "fixture",
        "openai",
        "fixture-unused",
        "fixture-model",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, tmp).await;
    cfg.plugin_manager = Arc::new(peri_middlewares::host_ports::PluginManager);
    cfg
}

fn seed_scope_marketplace(cache: &Path) {
    let root = cache.join("scope-market");
    let plugin = root.join("plugins/scoped/.claude-plugin");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(root.join("marketplace.json"), json!({
        "name": "scope-market", "plugins": [{"name": "scoped", "source": "plugins/scoped", "version": "1.0.0"}]
    }).to_string()).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        json!({"name": "scoped", "version": "1.0.0"}).to_string(),
    )
    .unwrap();
}

#[tokio::test]
#[serial]
async fn plugin_list_scope_round_trips_through_real_toggle_request() {
    for scope in [
        InstallScope::User,
        InstallScope::Project,
        InstallScope::Local,
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _home_guard = HomeDirGuard::set(&home);
        let mut cfg = scope_config(&tmp).await;
        let claude_dir = cfg.plugin_manager.claude_home();
        let cache = cfg.plugin_manager.cache_dir();
        seed_scope_marketplace(&cache);
        let mut sessions = HashMap::new();
        let session_id =
            register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
        let cwd = PathBuf::from(&sessions[&session_id].cwd);
        let project_dir = if scope == InstallScope::User {
            None
        } else {
            Some(cwd.as_path())
        };
        let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
        handle_request("plugin/install", &json!({"name": "scoped", "marketplace": "scope-market", "scope": format!("{scope:?}").to_lowercase(), "sessionId": session_id}), &cfg, &mut sessions, &transport).await.unwrap();
        let records = peri_middlewares::plugin::load_installed_plugins(Some(
            &claude_dir.join("plugins/installed_plugins.json"),
        ))
        .unwrap();
        let installed = &records.plugins[0];
        assert_eq!(installed.scope, scope);
        assert_eq!(
            installed.project_path.as_deref(),
            project_dir.and_then(Path::to_str)
        );
        cfg.plugin_loaded =
            peri_middlewares::plugin::load_enabled_plugins(&claude_dir, project_dir).unwrap();
        assert_eq!(cfg.plugin_loaded.len(), 1);
        let response = handle_request(
            "plugin/list",
            &json!({"sessionId": session_id}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
        let entry = &response["plugins"][0];
        assert_eq!(entry["install_scope"], format!("{scope:?}").to_lowercase());
        assert_eq!(entry["source"], "session");
        assert_eq!(entry["toggle_supported"], true);
        assert!(entry["load_error"].is_null());
        let settings_path = match scope {
            InstallScope::User => claude_dir.join("settings.json"),
            InstallScope::Project => cwd.join(".claude/settings.json"),
            InstallScope::Local => cwd.join(".claude/settings.local.json"),
        };
        let sibling = claude_dir.join("settings.json");
        if scope != InstallScope::User {
            std::fs::write(&sibling, r#"{"pluginConfigs":{"private":"keep"}}"#).unwrap();
        }
        let params = json!({"sessionId": session_id, "pluginId": installed.id, "scope": entry["install_scope"], "enable": false});
        assert_eq!(
            handle_request("plugin/toggle", &params, &cfg, &mut sessions, &transport)
                .await
                .unwrap()["success"],
            true
        );
        let settings: Value =
            serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
        assert!(settings["enabledPlugins"].get(&installed.id).is_none());
        let mut params = params;
        params["enable"] = json!(true);
        handle_request("plugin/toggle", &params, &cfg, &mut sessions, &transport)
            .await
            .unwrap();
        let settings: Value =
            serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
        assert_eq!(settings["enabledPlugins"][&installed.id], true);
        if scope != InstallScope::User {
            assert_eq!(
                std::fs::read_to_string(sibling).unwrap(),
                r#"{"pluginConfigs":{"private":"keep"}}"#
            );
        }
        handle_request(
            "plugin/uninstall",
            &json!({"pluginId": installed.id, "scope": format!("{scope:?}").to_lowercase(), "sessionId": session_id}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
        assert!(peri_middlewares::plugin::load_installed_plugins(Some(
            &claude_dir.join("plugins/installed_plugins.json")
        ))
        .unwrap()
        .plugins
        .is_empty());
    }
}

#[tokio::test]
#[serial]
async fn plugin_scope_request_rejection_precedes_production_port_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let _home_guard = HomeDirGuard::set(tmp.path());
    let cfg = scope_config(&tmp).await;
    seed_scope_marketplace(&cfg.plugin_manager.cache_dir());
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    for scope in [
        json!("project"),
        json!("local"),
        json!("session"),
        json!("managed"),
        json!("typo"),
        Value::Null,
        json!(42),
    ] {
        let error = handle_request(
            "plugin/install",
            &json!({"name": "scoped", "marketplace": "scope-market", "scope": scope}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, -32602);
        assert!(!cfg
            .plugin_manager
            .claude_home()
            .join("plugins/cache")
            .exists());
        assert!(!cfg
            .plugin_manager
            .claude_home()
            .join("plugins/installed_plugins.json")
            .exists());
    }
    let session_id =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    sessions.get_mut(&session_id).unwrap().closing = true;
    for (method, params) in [
        (
            "plugin/install",
            json!({"name": "scoped", "marketplace": "scope-market", "scope": "local", "sessionId": session_id}),
        ),
        (
            "plugin/uninstall",
            json!({"pluginId": "scoped@scope-market", "sessionId": session_id}),
        ),
        (
            "plugin/update",
            json!({"pluginId": "scoped@scope-market", "sessionId": session_id}),
        ),
    ] {
        assert_eq!(
            handle_request(method, &params, &cfg, &mut sessions, &transport)
                .await
                .unwrap_err()
                .code,
            -32010
        );
    }
    sessions.get_mut(&session_id).unwrap().closing = false;
    sessions.get_mut(&session_id).unwrap().cwd = "untrusted-relative".into();
    let params = json!({"name": "scoped", "marketplace": "scope-market", "scope": "project", "sessionId": session_id});
    assert_eq!(
        handle_request("plugin/install", &params, &cfg, &mut sessions, &transport)
            .await
            .unwrap_err()
            .code,
        -32602
    );
    assert!(!cfg
        .plugin_manager
        .claude_home()
        .join("plugins/installed_plugins.json")
        .exists());
}

#[tokio::test]
#[serial]
async fn plugin_user_install_uninstall_requests_work_without_session() {
    let tmp = tempfile::tempdir().unwrap();
    let _home_guard = HomeDirGuard::set(tmp.path());
    let cfg = scope_config(&tmp).await;
    seed_scope_marketplace(&cfg.plugin_manager.cache_dir());
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let installed = handle_request(
        "plugin/install",
        &json!({"name": "scoped", "marketplace": "scope-market"}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(installed["plugin"], "scoped@scope-market");
    let response = handle_request(
        "plugin/uninstall",
        &json!({"pluginId": installed["plugin"]}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(response["success"], true);
    let records = peri_middlewares::plugin::load_installed_plugins(Some(
        &cfg.plugin_manager
            .claude_home()
            .join("plugins/installed_plugins.json"),
    ))
    .unwrap();
    assert!(records.plugins.is_empty());
    assert!(sessions.is_empty());
}

#[tokio::test]
#[serial]
async fn plugin_list_unrecorded_source_is_read_only_and_preserves_load_error() {
    let tmp = tempfile::tempdir().unwrap();
    let _home_guard = HomeDirGuard::set(tmp.path());
    let mut cfg = scope_config(&tmp).await;
    let claude_dir = cfg.plugin_manager.claude_home();
    seed_scope_marketplace(&cfg.plugin_manager.cache_dir());
    let installed = cfg
        .plugin_manager
        .install(
            "scoped",
            "scope-market",
            InstallScope::User,
            &cfg.plugin_manager.cache_dir(),
            &claude_dir,
            None,
        )
        .await
        .unwrap();
    cfg.plugin_loaded = peri_middlewares::plugin::load_enabled_plugins(&claude_dir, None).unwrap();
    let mut entries = cfg.plugin_manager.snapshot(&claude_dir, None);
    assert_eq!(entries.len(), 1);
    entries[0].load_error = Some("fixture manifest load failure".into());
    std::fs::remove_file(claude_dir.join("plugins/installed_plugins.json")).unwrap();
    let mut manager = MockPluginManager::install_ok("unused");
    manager.snapshot_entries = entries;
    cfg.plugin_manager = Arc::new(manager);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let response = handle_request("plugin/list", &json!({}), &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    let entry = &response["plugins"][0];
    assert!(entry["install_scope"].is_null());
    assert_eq!(entry["toggle_supported"], false);
    assert_eq!(entry["load_error"], "fixture manifest load failure");
    assert!(entry["management_error"]
        .as_str()
        .unwrap()
        .contains("no writable"));
    cfg.plugin_manager = Arc::new(peri_middlewares::host_ports::PluginManager);
    let before = std::fs::read(claude_dir.join("settings.json")).unwrap();
    let error = handle_request(
        "plugin/toggle",
        &json!({"pluginId": installed.id, "scope": "user", "enable": false}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32603);
    assert_eq!(
        std::fs::read(claude_dir.join("settings.json")).unwrap(),
        before
    );
}
