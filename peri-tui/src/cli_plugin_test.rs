use super::*;
use peri_acp_types::plugin::InstallScope;

#[test]
fn user_scope_defaults_without_consulting_execution_directory() {
    for scope in [None, Some("user")] {
        let context =
            plugin_scope_context(scope, || panic!("user scope must not resolve cwd")).unwrap();
        assert_eq!(context.scope, InstallScope::User);
        assert!(context.project_dir.is_none());
    }
}

#[test]
fn scoped_context_keeps_explicit_scope_and_trusted_directory() {
    let project = tempfile::tempdir().unwrap();
    for (value, scope) in [
        ("project", InstallScope::Project),
        ("local", InstallScope::Local),
    ] {
        let context =
            plugin_scope_context(Some(value), || Ok(project.path().to_path_buf())).unwrap();
        assert_eq!(context.scope, scope);
        assert_eq!(context.project_dir.as_deref(), Some(project.path()));
    }
}

#[test]
fn unknown_scope_fails_before_resolving_directory() {
    for scope in ["session", "managed", "", "typo"] {
        let error =
            plugin_scope_context(Some(scope), || panic!("invalid scope must not resolve cwd"))
                .err()
                .unwrap();
        assert!(error.to_string().contains("未知的插件范围"));
    }
}

#[test]
fn scoped_directory_failure_is_not_replaced_by_user_scope() {
    for scope in ["project", "local"] {
        let error = plugin_scope_context(Some(scope), || {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "fixture cwd resolution failure",
            ))
        })
        .err()
        .unwrap();
        assert!(error.to_string().contains("fixture cwd resolution failure"));
    }
}

#[test]
fn scoped_directory_must_exist_and_be_absolute() {
    let root = tempfile::tempdir().unwrap();
    for scope in ["project", "local"] {
        for directory in [PathBuf::from("relative"), root.path().join("missing")] {
            assert!(plugin_scope_context(Some(scope), || Ok(directory)).is_err());
            assert!(!root.path().join("missing").exists());
        }
    }
}

#[test]
fn scoped_toggle_context_preserves_user_settings() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let user_settings = home.path().join("settings.json");
    let original =
        r#"{"enabledPlugins":{"shared@fixture":true},"pluginConfigs":{"private":"keep"}}"#;
    std::fs::write(&user_settings, original).unwrap();
    for (value, filename) in [
        ("project", "settings.json"),
        ("local", "settings.local.json"),
    ] {
        let context =
            plugin_scope_context(Some(value), || Ok(project.path().to_path_buf())).unwrap();
        peri_middlewares::plugin::update_enabled_plugins(
            "shared@fixture",
            context.scope,
            home.path(),
            context.project_dir.as_deref(),
        )
        .unwrap();
        let settings_path = project.path().join(".claude").join(filename);
        let settings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
        assert_eq!(settings["enabledPlugins"]["shared@fixture"], true);
        peri_middlewares::plugin::remove_from_enabled_plugins(
            "shared@fixture",
            &context.scope,
            home.path(),
            context.project_dir.as_deref(),
        )
        .unwrap();
        let settings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
        assert!(settings["enabledPlugins"].get("shared@fixture").is_none());
        assert_eq!(std::fs::read_to_string(&user_settings).unwrap(), original);
    }
}
