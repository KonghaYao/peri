use super::*;

#[test]
fn local_plugin_selection_overrides_project_without_changing_user_selection() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("plugins")).unwrap();
    std::fs::create_dir_all(project.path().join(".claude")).unwrap();
    let plugins = ["user", "project", "local"]
        .into_iter()
        .map(|name| {
            serde_json::json!({
                "id": name, "name": name, "version": "1", "marketplace": "test",
                "install_path": home.path().join(name), "scope": "User"
            })
        })
        .collect::<Vec<_>>();
    std::fs::write(
        home.path().join("plugins/installed_plugins.json"),
        serde_json::to_vec(&serde_json::json!({"version":2,"plugins":plugins})).unwrap(),
    )
    .unwrap();
    std::fs::write(
        home.path().join("settings.json"),
        r#"{"enabledPlugins":{"user":true}}"#,
    )
    .unwrap();
    std::fs::write(
        project.path().join(".claude/settings.json"),
        r#"{"enabledPlugins":{"project":true}}"#,
    )
    .unwrap();
    std::fs::write(
        project.path().join(".claude/settings.local.json"),
        r#"{"enabledPlugins":{"local":true}}"#,
    )
    .unwrap();
    let local = select_enabled_plugins(home.path(), Some(project.path())).unwrap();
    assert_eq!(local.plugins.len(), 1);
    assert_eq!(local.plugins[0].id, "local");
    let user = select_enabled_plugins(home.path(), None).unwrap();
    assert_eq!(user.plugins.len(), 1);
    assert_eq!(user.plugins[0].id, "user");
    std::fs::remove_file(project.path().join(".claude/settings.local.json")).unwrap();
    let inherited = select_enabled_plugins(home.path(), Some(project.path())).unwrap();
    assert_eq!(inherited.plugins.len(), 1);
    assert_eq!(inherited.plugins[0].id, "project");
}

#[test]
fn invalid_local_settings_are_reported_instead_of_using_another_scope() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("plugins")).unwrap();
    std::fs::create_dir_all(project.path().join(".claude")).unwrap();
    std::fs::write(
        home.path().join("plugins/installed_plugins.json"),
        r#"{"version":2,"plugins":[]}"#,
    )
    .unwrap();
    std::fs::write(
        project.path().join(".claude/settings.local.json"),
        "{invalid",
    )
    .unwrap();
    assert!(select_enabled_plugins(home.path(), Some(project.path())).is_err());
}
