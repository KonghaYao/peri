use super::*;
use peri_acp_types::plugin::PluginManagerPort;

fn seed_identity_marketplace(cache: &Path, version: &str) {
    let root = cache.join("identity-market");
    let plugin = root.join("plugins/shared/.claude-plugin");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(root.join("marketplace.json"), serde_json::json!({
        "name": "identity-market", "plugins": [{"name": "shared", "source": "plugins/shared", "version": version}]
    }).to_string()).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        serde_json::json!({"name": "shared", "version": version}).to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn production_port_explicit_identity_never_mutates_other_scope_root() {
    let manager = crate::host_ports::PluginManager;
    for selected in [
        InstallScope::User,
        InstallScope::Project,
        InstallScope::Local,
    ] {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let scoped = if selected == InstallScope::Local {
            InstallScope::Local
        } else {
            InstallScope::Project
        };
        seed_identity_marketplace(cache.path(), "1.0.0");
        let user = manager
            .install(
                "shared",
                "identity-market",
                InstallScope::User,
                cache.path(),
                home.path(),
                None,
            )
            .await
            .unwrap();
        seed_identity_marketplace(cache.path(), "2.0.0");
        let project_record = manager
            .install(
                "shared",
                "identity-market",
                scoped,
                cache.path(),
                home.path(),
                Some(project.path()),
            )
            .await
            .unwrap();
        assert_ne!(user.install_path, project_record.install_path);
        let untouched = if selected == InstallScope::User {
            &project_record
        } else {
            &user
        };
        let untouched_record = serde_json::to_value(untouched).unwrap();
        let manifest = untouched.install_path.join(".claude-plugin/plugin.json");
        let original_manifest = std::fs::read(&manifest).unwrap();
        let settings =
            enabled_plugins_settings_path(untouched.scope, home.path(), Some(project.path()))
                .unwrap();
        let original_settings = std::fs::read(&settings).unwrap();
        seed_identity_marketplace(cache.path(), "3.0.0");
        let updated = manager
            .update(
                &user.id,
                selected,
                cache.path(),
                home.path(),
                Some(project.path()),
            )
            .await
            .unwrap();
        assert_eq!(updated.scope, selected);
        assert_eq!(updated.version, "3.0.0");
        let records_path = home.path().join("plugins/installed_plugins.json");
        let records = load_installed_plugins(Some(&records_path)).unwrap();
        assert_eq!(records.plugins.len(), 2);
        assert_eq!(
            serde_json::to_value(
                records
                    .plugins
                    .iter()
                    .find(|record| record.scope == untouched.scope)
                    .unwrap()
            )
            .unwrap(),
            untouched_record
        );
        assert_eq!(std::fs::read(&manifest).unwrap(), original_manifest);
        assert_eq!(std::fs::read(&settings).unwrap(), original_settings);
        manager
            .uninstall(&user.id, selected, home.path(), Some(project.path()))
            .await
            .unwrap();
        let records = load_installed_plugins(Some(&records_path)).unwrap();
        assert_eq!(records.plugins.len(), 1);
        assert_eq!(
            serde_json::to_value(&records.plugins[0]).unwrap(),
            untouched_record
        );
        assert_eq!(std::fs::read(&manifest).unwrap(), original_manifest);
        assert_eq!(std::fs::read(&settings).unwrap(), original_settings);
        assert!(!untouched.install_path.join(".orphaned_at").exists());
    }
}

#[tokio::test]
async fn production_port_missing_identity_never_falls_back_or_writes() {
    let manager = crate::host_ports::PluginManager;
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let other_project = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    seed_identity_marketplace(cache.path(), "1.0.0");
    let record = manager
        .install(
            "shared",
            "identity-market",
            InstallScope::Project,
            cache.path(),
            home.path(),
            Some(project.path()),
        )
        .await
        .unwrap();
    let path = home.path().join("plugins/installed_plugins.json");
    let original = std::fs::read(&path).unwrap();
    seed_identity_marketplace(cache.path(), "3.0.0");
    for (scope, directory) in [
        (InstallScope::User, Some(project.path())),
        (InstallScope::Local, Some(project.path())),
        (InstallScope::Project, Some(other_project.path())),
        (InstallScope::Project, None),
        (InstallScope::Project, Some(Path::new("relative"))),
    ] {
        assert!(manager
            .update(&record.id, scope, cache.path(), home.path(), directory)
            .await
            .is_err());
        assert!(manager
            .uninstall(&record.id, scope, home.path(), directory)
            .await
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(!home
            .path()
            .join("plugins/cache/identity-market/shared/3.0.0")
            .exists());
        assert!(record
            .install_path
            .join(".claude-plugin/plugin.json")
            .exists());
        assert!(!record.install_path.join(".orphaned_at").exists());
        assert!(!home.path().join("settings.json").exists());
        assert!(!other_project.path().join(".claude").exists());
    }
}
