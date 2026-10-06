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
