use super::*;

async fn identity_config(tmp: &tempfile::TempDir) -> AcpServerConfig {
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

fn seed_identity_marketplace(cache: &Path, version: &str) {
    let root = cache.join("identity-market");
    let plugin = root.join("plugins/shared/.claude-plugin");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(root.join("marketplace.json"), json!({
        "name": "identity-market", "plugins": [{"name": "shared", "source": "plugins/shared", "version": version}]
    }).to_string()).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        json!({"name": "shared", "version": version}).to_string(),
    )
    .unwrap();
}

#[tokio::test]
#[serial]
async fn plugin_rpc_selected_scope_preserves_other_root_with_same_id() {
    for requested_scope in [Some("user"), Some("project"), None] {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _home_guard = HomeDirGuard::set(&home);
        let mut cfg = identity_config(&tmp).await;
        let claude_dir = cfg.plugin_manager.claude_home();
        let cache = cfg.plugin_manager.cache_dir();
        let mut sessions = HashMap::new();
        let session_id =
            register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
        let cwd = PathBuf::from(&sessions[&session_id].cwd);
        let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
        for (scope, version) in [("user", "1.0.0"), ("project", "2.0.0")] {
            seed_identity_marketplace(&cache, version);
            handle_request("plugin/install", &json!({"name": "shared", "marketplace": "identity-market", "scope": scope, "sessionId": session_id}), &cfg, &mut sessions, &transport).await.unwrap();
        }
        cfg.plugin_loaded =
            peri_middlewares::plugin::load_enabled_plugins(&claude_dir, Some(&cwd)).unwrap();
        assert_eq!(cfg.plugin_loaded.len(), 2);
        assert_ne!(
            cfg.plugin_loaded[0].install_path,
            cfg.plugin_loaded[1].install_path
        );
        let listed = handle_request(
            "plugin/list",
            &json!({"sessionId": session_id}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
        let mut scopes = listed["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["install_scope"].as_str().unwrap())
            .collect::<Vec<_>>();
        scopes.sort();
        assert_eq!(scopes, ["project", "user"]);
        let selected = if requested_scope == Some("project") {
            InstallScope::Project
        } else {
            InstallScope::User
        };
        let records_path = claude_dir.join("plugins/installed_plugins.json");
        let records =
            peri_middlewares::plugin::load_installed_plugins(Some(&records_path)).unwrap();
        let untouched = records
            .plugins
            .iter()
            .find(|record| record.scope != selected)
            .unwrap();
        let untouched_record = serde_json::to_value(untouched).unwrap();
        let manifest_path = untouched.install_path.join(".claude-plugin/plugin.json");
        let original_manifest = std::fs::read(&manifest_path).unwrap();
        let settings_path = if selected == InstallScope::User {
            cwd.join(".claude/settings.json")
        } else {
            claude_dir.join("settings.json")
        };
        let original_settings = std::fs::read(&settings_path).unwrap();
        seed_identity_marketplace(&cache, "3.0.0");
        let mut params = json!({"pluginId": "shared@identity-market", "sessionId": session_id});
        if let Some(scope) = requested_scope {
            params["scope"] = json!(scope);
        }
        handle_request("plugin/update", &params, &cfg, &mut sessions, &transport)
            .await
            .unwrap();
        let after_update =
            peri_middlewares::plugin::load_installed_plugins(Some(&records_path)).unwrap();
        assert_eq!(after_update.plugins.len(), 2);
        let updated = after_update
            .plugins
            .iter()
            .find(|record| record.scope == selected)
            .unwrap();
        assert_eq!(updated.version, "3.0.0");
        assert_eq!(
            serde_json::to_value(
                after_update
                    .plugins
                    .iter()
                    .find(|record| record.scope != selected)
                    .unwrap()
            )
            .unwrap(),
            untouched_record
        );
        assert_eq!(std::fs::read(&manifest_path).unwrap(), original_manifest);
        assert_eq!(std::fs::read(&settings_path).unwrap(), original_settings);
        handle_request("plugin/uninstall", &params, &cfg, &mut sessions, &transport)
            .await
            .unwrap();
        let after_uninstall =
            peri_middlewares::plugin::load_installed_plugins(Some(&records_path)).unwrap();
        assert_eq!(after_uninstall.plugins.len(), 1);
        assert_eq!(
            serde_json::to_value(&after_uninstall.plugins[0]).unwrap(),
            untouched_record
        );
        assert_eq!(std::fs::read(&manifest_path).unwrap(), original_manifest);
        assert_eq!(std::fs::read(&settings_path).unwrap(), original_settings);
        assert!(!untouched.install_path.join(".orphaned_at").exists());
    }
}

#[tokio::test]
#[serial]
async fn plugin_rpc_missing_or_ambiguous_identity_is_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home_guard = HomeDirGuard::set(&home);
    let cfg = identity_config(&tmp).await;
    let claude_dir = cfg.plugin_manager.claude_home();
    let cache = cfg.plugin_manager.cache_dir();
    let mut sessions = HashMap::new();
    let session_id =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    seed_identity_marketplace(&cache, "1.0.0");
    handle_request("plugin/install", &json!({"name": "shared", "marketplace": "identity-market", "scope": "project", "sessionId": session_id}), &cfg, &mut sessions, &transport).await.unwrap();
    let records_path = claude_dir.join("plugins/installed_plugins.json");
    let original = std::fs::read(&records_path).unwrap();
    seed_identity_marketplace(&cache, "3.0.0");
    for params in [
        json!({"pluginId": "shared@identity-market", "sessionId": session_id}),
        json!({"pluginId": "shared@identity-market", "scope": "local", "sessionId": session_id}),
        json!({"pluginId": "shared@identity-market", "scope": "project"}),
        json!({"pluginId": "shared@identity-market", "scope": "managed", "sessionId": session_id}),
    ] {
        for method in ["plugin/update", "plugin/uninstall"] {
            assert!(
                handle_request(method, &params, &cfg, &mut sessions, &transport)
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::read(&records_path).unwrap(), original);
            assert!(!claude_dir
                .join("plugins/cache/identity-market/shared/3.0.0")
                .exists());
        }
    }
    let mut records =
        peri_middlewares::plugin::load_installed_plugins(Some(&records_path)).unwrap();
    records.plugins.push(records.plugins[0].clone());
    peri_middlewares::plugin::config::save_installed_plugins(&records, Some(&records_path))
        .unwrap();
    let duplicate = std::fs::read(&records_path).unwrap();
    let params =
        json!({"pluginId": "shared@identity-market", "scope": "project", "sessionId": session_id});
    for method in ["plugin/update", "plugin/uninstall", "plugin/toggle"] {
        let error = handle_request(method, &params, &cfg, &mut sessions, &transport)
            .await
            .unwrap_err();
        assert_eq!(error.code, -32603);
        assert_eq!(std::fs::read(&records_path).unwrap(), duplicate);
        assert!(!claude_dir
            .join("plugins/cache/identity-market/shared/3.0.0")
            .exists());
    }
}
