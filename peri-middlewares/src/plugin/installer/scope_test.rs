use super::*;

#[test]
fn scope_writes_are_isolated_and_preserve_sibling_fields() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let cases = [
        (InstallScope::User, home.path().join("settings.json")),
        (
            InstallScope::Project,
            project.path().join(".claude/settings.json"),
        ),
        (
            InstallScope::Local,
            project.path().join(".claude/settings.local.json"),
        ),
    ];
    for (scope, settings_path) in &cases {
        std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        std::fs::write(settings_path, r#"{"theme":"keep","enabledPlugins":{}}"#).unwrap();
        let plugin_id = format!("scope-{scope:?}");
        update_enabled_plugins(&plugin_id, *scope, home.path(), Some(project.path())).unwrap();
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(settings_path).unwrap()).unwrap();
        assert_eq!(settings["theme"], "keep");
        assert_eq!(settings["enabledPlugins"][&plugin_id], true);
        assert_eq!(settings["enabledPlugins"].as_object().unwrap().len(), 1);
        remove_from_enabled_plugins(&plugin_id, scope, home.path(), Some(project.path())).unwrap();
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(settings_path).unwrap()).unwrap();
        assert_eq!(settings["theme"], "keep");
        assert_eq!(settings["enabledPlugins"].as_object().unwrap().len(), 0);
    }
}

#[test]
fn scoped_toggle_without_session_never_falls_back_to_user_settings() {
    let home = tempfile::tempdir().unwrap();
    for scope in [InstallScope::Project, InstallScope::Local] {
        assert!(update_enabled_plugins("plugin", scope, home.path(), None).is_err());
        assert!(remove_from_enabled_plugins("plugin", &scope, home.path(), None).is_err());
    }
    assert!(!home.path().join("settings.json").exists());
}

#[test]
fn malformed_settings_are_not_overwritten_on_enable() {
    let home = tempfile::tempdir().unwrap();
    let settings_path = home.path().join("settings.json");
    for content in ["{malformed", "[]", "null"] {
        std::fs::write(&settings_path, content).unwrap();
        assert!(update_enabled_plugins("plugin", InstallScope::User, home.path(), None).is_err());
        assert_eq!(std::fs::read_to_string(&settings_path).unwrap(), content);
    }
}

fn seed_marketplace(cache: &Path, version: &str) {
    let root = cache.join("scope-market");
    let plugin = root.join("plugins/scoped/.claude-plugin");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(root.join("marketplace.json"), serde_json::json!({
        "name": "scope-market", "plugins": [{"name": "scoped", "source": "plugins/scoped", "version": version}]
    }).to_string()).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        serde_json::json!({
            "name": "scoped", "version": version
        })
        .to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn production_port_scoped_install_update_uninstall_preserves_user_settings() {
    use peri_acp_types::plugin::PluginManagerPort;
    let manager = crate::host_ports::PluginManager;
    for scope in [InstallScope::Project, InstallScope::Local] {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        seed_marketplace(cache.path(), "1.0.0");
        let settings = home.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{"theme":"keep","enabledPlugins":{"other":true},"pluginConfigs":{"secret":"keep"}}"#,
        )
        .unwrap();
        let original = std::fs::read(&settings).unwrap();
        let installed = manager
            .install(
                "scoped",
                "scope-market",
                scope,
                cache.path(),
                home.path(),
                Some(project.path()),
            )
            .await
            .unwrap();
        assert_eq!(installed.scope, scope);
        assert_eq!(installed.project_path.as_deref(), project.path().to_str());
        let scoped_settings =
            enabled_plugins_settings_path(scope, home.path(), Some(project.path())).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&scoped_settings).unwrap()).unwrap();
        assert_eq!(value["enabledPlugins"]["scoped@scope-market"], true);
        seed_marketplace(cache.path(), "2.0.0");
        let updated = manager
            .update(
                &installed.id,
                scope,
                cache.path(),
                home.path(),
                Some(project.path()),
            )
            .await
            .unwrap();
        assert_eq!(updated.version, "2.0.0");
        assert_eq!(updated.scope, scope);
        manager
            .uninstall(&installed.id, scope, home.path(), Some(project.path()))
            .await
            .unwrap();
        let records =
            load_installed_plugins(Some(&home.path().join("plugins/installed_plugins.json")))
                .unwrap();
        assert!(records.plugins.is_empty());
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&scoped_settings).unwrap()).unwrap();
        assert!(value["enabledPlugins"].as_object().unwrap().is_empty());
        assert_eq!(std::fs::read(&settings).unwrap(), original);
    }
}

#[tokio::test]
async fn production_port_scope_validation_precedes_install_copy_and_records() {
    use peri_acp_types::plugin::PluginManagerPort;
    let manager = crate::host_ports::PluginManager;
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    seed_marketplace(cache.path(), "1.0.0");
    let missing = home.path().join("missing-project");
    for scope in [InstallScope::Project, InstallScope::Local] {
        for project in [
            None,
            Some(Path::new("relative-project")),
            Some(missing.as_path()),
        ] {
            assert!(manager
                .install(
                    "scoped",
                    "scope-market",
                    scope,
                    cache.path(),
                    home.path(),
                    project
                )
                .await
                .is_err());
            assert!(!home.path().join("plugins").exists());
            assert!(!home.path().join("settings.json").exists());
            assert!(!missing.exists());
        }
    }
}

#[tokio::test]
async fn production_port_user_install_uninstall_needs_no_session() {
    use peri_acp_types::plugin::PluginManagerPort;
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    seed_marketplace(cache.path(), "1.0.0");
    let manager = crate::host_ports::PluginManager;
    let installed = manager
        .install(
            "scoped",
            "scope-market",
            InstallScope::User,
            cache.path(),
            home.path(),
            None,
        )
        .await
        .unwrap();
    assert!(installed.project_path.is_none());
    manager
        .uninstall(&installed.id, InstallScope::User, home.path(), None)
        .await
        .unwrap();
    assert!(
        load_installed_plugins(Some(&home.path().join("plugins/installed_plugins.json")))
            .unwrap()
            .plugins
            .is_empty()
    );
}

#[tokio::test]
async fn production_port_invalid_mutation_directory_preserves_records_and_files() {
    use peri_acp_types::plugin::PluginManagerPort;
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    seed_marketplace(cache.path(), "1.0.0");
    let manager = crate::host_ports::PluginManager;
    let installed = manager
        .install(
            "scoped",
            "scope-market",
            InstallScope::Project,
            cache.path(),
            home.path(),
            Some(project.path()),
        )
        .await
        .unwrap();
    let records = home.path().join("plugins/installed_plugins.json");
    let original = std::fs::read(&records).unwrap();
    seed_marketplace(cache.path(), "2.0.0");
    for directory in [None, Some(Path::new("relative-project"))] {
        assert!(manager
            .uninstall(&installed.id, InstallScope::Project, home.path(), directory)
            .await
            .is_err());
        assert!(manager
            .update(
                &installed.id,
                InstallScope::Project,
                cache.path(),
                home.path(),
                directory
            )
            .await
            .is_err());
        assert_eq!(std::fs::read(&records).unwrap(), original);
        assert!(installed.install_path.exists());
        assert!(!home
            .path()
            .join("plugins/cache/scope-market/scoped/2.0.0")
            .exists());
    }
}

#[tokio::test]
async fn production_port_user_scope_does_not_acquire_session_identity() {
    use peri_acp_types::plugin::PluginManagerPort;
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    seed_marketplace(cache.path(), "1.0.0");
    let manager = crate::host_ports::PluginManager;
    let installed = manager
        .install(
            "scoped",
            "scope-market",
            InstallScope::User,
            cache.path(),
            home.path(),
            Some(project.path()),
        )
        .await
        .unwrap();
    assert!(installed.project_path.is_none());
    assert!(!project.path().join(".claude").exists());
    manager
        .uninstall(
            &installed.id,
            InstallScope::User,
            home.path(),
            Some(project.path()),
        )
        .await
        .unwrap();
    assert!(
        load_installed_plugins(Some(&home.path().join("plugins/installed_plugins.json")))
            .unwrap()
            .plugins
            .is_empty()
    );
}

#[tokio::test]
async fn production_port_ambiguous_scopes_fail_without_mutation() {
    use peri_acp_types::plugin::PluginManagerPort;
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    seed_marketplace(cache.path(), "1.0.0");
    let manager = crate::host_ports::PluginManager;
    for scope in [InstallScope::Project, InstallScope::Local] {
        manager
            .install(
                "scoped",
                "scope-market",
                scope,
                cache.path(),
                home.path(),
                Some(project.path()),
            )
            .await
            .unwrap();
    }
    let path = home.path().join("plugins/installed_plugins.json");
    let mut records = load_installed_plugins(Some(&path)).unwrap();
    let duplicate = records
        .plugins
        .iter()
        .find(|record| record.scope == InstallScope::Project)
        .unwrap()
        .clone();
    records.plugins.push(duplicate);
    save_installed_plugins(&records, Some(&path)).unwrap();
    let original = std::fs::read(&path).unwrap();
    let loaded = crate::plugin::load_enabled_plugins(home.path(), Some(project.path())).unwrap();
    assert!(manager
        .installation_scope(home.path(), &loaded[0])
        .unwrap_err()
        .contains("ambiguous"));
    assert!(manager
        .uninstall(
            "scoped@scope-market",
            InstallScope::Project,
            home.path(),
            Some(project.path())
        )
        .await
        .unwrap_err()
        .contains("ambiguous"));
    assert!(manager
        .update(
            "scoped@scope-market",
            InstallScope::Project,
            cache.path(),
            home.path(),
            Some(project.path())
        )
        .await
        .unwrap_err()
        .contains("ambiguous"));
    assert_eq!(std::fs::read(path).unwrap(), original);
}
