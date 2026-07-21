//! Tests for requests

use super::*;

fn make_workspace_folders() -> Vec<WorkspaceFolder> {
    vec![WorkspaceFolder {
        uri: "file:///tmp".parse().unwrap(),
        name: "workspace".to_string(),
    }]
}

#[test]
fn test_initialize_params_without_init_options() {
    let params = initialize_params("file:///tmp".to_string(), make_workspace_folders(), None);
    assert_eq!(params["rootUri"], "file:///tmp");
    assert!(params.get("initializationOptions").is_none());
    assert!(params["capabilities"]["textDocument"]["definition"].is_object());
}

#[test]
fn test_initialize_params_with_init_options() {
    let opts = serde_json::json!({
        "maxTsServerMemory": 8192,
        "checkOnSave": { "command": "clippy" }
    });
    let params = initialize_params(
        "file:///tmp".to_string(),
        make_workspace_folders(),
        Some(opts),
    );
    assert_eq!(params["initializationOptions"]["maxTsServerMemory"], 8192);
    assert_eq!(
        params["initializationOptions"]["checkOnSave"]["command"],
        "clippy"
    );
}

#[test]
fn test_initialize_params_has_process_id() {
    let params = initialize_params("file:///tmp".to_string(), make_workspace_folders(), None);
    assert!(params["processId"].is_number());
    assert!(params["processId"].as_u64().unwrap() > 0);
}
