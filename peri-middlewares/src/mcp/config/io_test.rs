use super::*;

#[test]
fn existence_errors_are_not_missing_configuration() {
    let path = Path::new("invalid\0config.json");
    for error in [load_from_path(path), load_global_config(path)] {
        let McpConfigError::ReadError {
            path: actual,
            source,
        } = error.unwrap_err()
        else {
            panic!("expected existence failure to remain a read error");
        };
        assert_eq!(actual, path.display().to_string());
        assert_eq!(source.kind(), std::io::ErrorKind::InvalidInput);
    }
}

#[test]
fn invalid_utf8_remains_a_read_error() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    std::fs::write(&path, [0xff]).unwrap();

    for result in [load_from_path(&path), load_global_config(&path)] {
        let McpConfigError::ReadError {
            path: actual,
            source,
        } = result.unwrap_err()
        else {
            panic!("expected invalid text to remain a read error");
        };
        assert_eq!(actual, path.display().to_string());
        assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
    }
}

#[test]
fn atomic_write_failure_retains_target_and_io_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    std::fs::create_dir(&path).unwrap();
    let retained = path.join("retained.json");
    std::fs::write(&retained, "unchanged").unwrap();
    let error = atomic_write_json(&path, &serde_json::json!({"mcpServers": {}})).unwrap_err();
    let McpConfigError::WriteError {
        path: actual,
        source,
    } = error
    else {
        panic!("expected atomic data plane failure to remain a write error");
    };
    assert_eq!(actual, path.display().to_string());
    assert_ne!(source.kind(), std::io::ErrorKind::NotFound);
    assert!(path.is_dir());
    assert_eq!(std::fs::read_to_string(&retained).unwrap(), "unchanged");
}

#[test]
fn disabled_update_prefers_project_and_removes_false_flag() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join(".mcp.json");
    let global_path = directory.path().join("settings.json");
    let original = r#"{"mcpServers":{"target":{"command":"echo"}},"unrelated":42}"#;
    std::fs::write(&project_path, original).unwrap();
    std::fs::write(&global_path, original).unwrap();

    set_server_disabled_with_paths(directory.path(), &global_path, "target", true).unwrap();
    let disabled = load_from_path(&project_path).unwrap();
    assert_eq!(disabled.mcp_servers["target"].disabled, Some(true));
    assert_eq!(std::fs::read_to_string(&global_path).unwrap(), original);

    set_server_disabled_with_paths(directory.path(), &global_path, "target", false).unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&project_path).unwrap()).unwrap();
    assert!(written["mcpServers"]["target"].get("disabled").is_none());
    assert_eq!(written["unrelated"], 42);
    assert_eq!(std::fs::read_to_string(&global_path).unwrap(), original);
}

#[test]
fn disabled_update_prefers_nested_global_and_preserves_other_fields() {
    let directory = tempfile::tempdir().unwrap();
    let global_path = directory.path().join("settings.json");
    std::fs::write(
        &global_path,
        r#"{"config":{"mcpServers":{"target":{"command":"echo"}},"other":42},"mcpServers":{"target":{"command":"other"}},"unrelated":true}"#,
    )
    .unwrap();

    set_server_disabled_with_paths(directory.path(), &global_path, "target", true).unwrap();
    let disabled = load_global_config(&global_path).unwrap();
    assert_eq!(disabled.mcp_servers["target"].disabled, Some(true));
    assert_eq!(
        disabled.mcp_servers["target"].command.as_deref(),
        Some("echo")
    );
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&global_path).unwrap()).unwrap();
    assert!(written["mcpServers"]["target"].get("disabled").is_none());
    assert_eq!(written["config"]["other"], 42);
    assert_eq!(written["unrelated"], true);

    set_server_disabled_with_paths(directory.path(), &global_path, "target", false).unwrap();
    assert_eq!(
        load_global_config(&global_path).unwrap().mcp_servers["target"].disabled,
        None
    );
}

#[test]
fn disabled_update_falls_back_to_flat_global_when_project_has_no_target() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join(".mcp.json");
    let global_path = directory.path().join("settings.json");
    let project = r#"{"mcpServers":{"other":{"command":"echo"}}}"#;
    std::fs::write(&project_path, project).unwrap();
    std::fs::write(
        &global_path,
        r#"{"mcpServers":{"target":{"command":"echo"}}}"#,
    )
    .unwrap();

    set_server_disabled_with_paths(directory.path(), &global_path, "target", true).unwrap();
    assert_eq!(
        load_global_config(&global_path).unwrap().mcp_servers["target"].disabled,
        Some(true)
    );
    assert_eq!(std::fs::read_to_string(&project_path).unwrap(), project);
}

#[test]
fn missing_files_remain_noop_for_mutations() {
    let directory = tempfile::tempdir().unwrap();
    let global_path = directory.path().join("settings.json");
    set_server_disabled_with_paths(directory.path(), &global_path, "missing", true).unwrap();
    remove_server_from_config_with_paths(directory.path(), &global_path, "missing").unwrap();
    assert!(!global_path.exists());
    assert!(!directory.path().join(".mcp.json").exists());
}
