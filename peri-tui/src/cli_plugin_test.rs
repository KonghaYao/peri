use super::PathBuf;

use peri_acp_types::plugin::InstallScope;

#[test]
fn user_scope_defaults_without_consulting_execution_directory() {
    for scope in [None, Some("user")] {
        let context =
            super::plugin_scope_context(scope, || panic!("user scope must not resolve cwd")).expect("user scope defaults without consulting execution directory: the plugin_scope_context fixture operation uses valid inputs");
        assert_eq!(context.scope, InstallScope::User);
        assert!(context.project_dir.is_none());
    }
}

#[test]
fn scoped_context_keeps_explicit_scope_and_trusted_directory() {
    let project = tempfile::tempdir().expect("scoped context keeps explicit scope and trusted directory: isolated fixture workspace must remain accessible");
    for (value, scope) in [
        ("project", InstallScope::Project),
        ("local", InstallScope::Local),
    ] {
        let context =
            super::plugin_scope_context(Some(value), || Ok(project.path().to_path_buf())).expect("scoped context keeps explicit scope and trusted directory: the plugin_scope_context fixture operation uses valid inputs");
        assert_eq!(context.scope, scope);
        assert_eq!(context.project_dir.as_deref(), Some(project.path()));
    }
}

#[test]
fn unknown_scope_fails_before_resolving_directory() {
    for scope in ["session", "managed", "", "typo"] {
        let error =
            super::plugin_scope_context(Some(scope), || panic!("invalid scope must not resolve cwd"))
                .err()
                .expect("unknown scope fails before resolving directory: the fixture must produce the expected rejection");
        assert!(error.to_string().contains("未知的插件范围"));
    }
}

#[test]
fn scoped_directory_failure_is_not_replaced_by_user_scope() {
    for scope in ["project", "local"] {
        let error = super::plugin_scope_context(Some(scope), || {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "fixture cwd resolution failure",
            ))
        })
        .err()
        .expect("scoped directory failure is not replaced by user scope: fixture parameters must construct a valid value");
        assert!(error.to_string().contains("fixture cwd resolution failure"));
    }
}

#[test]
fn scoped_directory_must_exist_and_be_absolute() {
    let root = tempfile::tempdir().expect("scoped directory must exist and be absolute: isolated fixture workspace must remain accessible");
    for scope in ["project", "local"] {
        {
            let iteration_items = [PathBuf::from("relative"), root.path().join("missing")];
            for directory in iteration_items {
                assert!(super::plugin_scope_context(Some(scope), || Ok(directory)).is_err());
                assert!(!root.path().join("missing").exists());
            }
        }
    }
}

#[test]
fn scoped_toggle_context_preserves_user_settings() {
    let home = tempfile::tempdir().expect("scoped toggle context preserves user settings: isolated fixture workspace must remain accessible");
    let project = tempfile::tempdir().expect("scoped toggle context preserves user settings: isolated fixture workspace must remain accessible");
    let user_settings = home.path().join("settings.json");
    let original =
        r#"{"enabledPlugins":{"shared@fixture":true},"pluginConfigs":{"private":"keep"}}"#;
    std::fs::write(&user_settings, original).expect("scoped toggle context preserves user settings: isolated fixture workspace must remain accessible");
    for (value, filename) in [
        ("project", "settings.json"),
        ("local", "settings.local.json"),
    ] {
        let context =
            super::plugin_scope_context(Some(value), || Ok(project.path().to_path_buf())).expect("scoped toggle context preserves user settings: the plugin_scope_context fixture operation uses valid inputs");
        peri_middlewares::plugin::update_enabled_plugins(
            "shared@fixture",
            context.scope,
            home.path(),
            context.project_dir.as_deref(),
        )
        .expect(
            "scoped toggle context preserves user settings: fixture must have the expected variant",
        );
        let settings_path = project.path().join(".claude").join(filename);
        let settings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings_path).expect("scoped toggle context preserves user settings: isolated fixture workspace must remain accessible")).expect("scoped toggle context preserves user settings: fixture JSON must match the expected schema");
        assert_eq!(settings["enabledPlugins"]["shared@fixture"], true);
        peri_middlewares::plugin::remove_from_enabled_plugins(
            "shared@fixture",
            &context.scope,
            home.path(),
            context.project_dir.as_deref(),
        )
        .expect(
            "scoped toggle context preserves user settings: fixture must have the expected variant",
        );
        let settings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings_path).expect("scoped toggle context preserves user settings: isolated fixture workspace must remain accessible")).expect("scoped toggle context preserves user settings: fixture JSON must match the expected schema");
        assert!(settings["enabledPlugins"].get("shared@fixture").is_none());
        assert_eq!(std::fs::read_to_string(&user_settings).expect("scoped toggle context preserves user settings: the fixture storage and layout must support the requested operation"), original);
    }
}
