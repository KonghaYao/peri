//! Tests for global_hub

use super::*;

#[test]
fn test_handle_initialize() {
    let id = serde_json::json!(1);
    let resp = handle_initialize(&id);
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["protocolVersion"], 1);
    // 关键：字段名应为 agentCapabilities 而非 capabilities
    assert!(resp["result"].get("agentCapabilities").is_some());
    assert!(resp["result"]
        .get("agentCapabilities")
        .unwrap()
        .get("sessionCapabilities")
        .is_some());
}

#[test]
fn test_handle_commands_list() {
    let id = serde_json::json!(2);
    let resp = handle_commands_list(&id);
    let cmds = resp["result"].as_array().unwrap();
    assert!(cmds.len() >= 2);
    assert_eq!(cmds[0]["name"], "/clear");
}

#[test]
fn test_handle_session_list_empty() {
    let id = serde_json::json!(3);
    let sessions: Vec<crate::router::SessionInfo> = vec![];
    let resp = handle_session_list(&id, &sessions);
    assert_eq!(resp["id"], 3);
    let list = resp["result"].as_array().unwrap();
    assert!(list.is_empty());
}

#[test]
fn test_handle_session_list_with_items() {
    let id = serde_json::json!(4);
    let sessions = vec![crate::router::SessionInfo {
        session_id: "abc-123".to_string(),
        cwd: "/home/user/project".to_string(),
        title: Some("My Session".to_string()),
        updated_at: Some("2026-07-18T12:00:00Z".to_string()),
        created_at: chrono::Utc::now(),
        status: crate::router::SessionStatus::Ready,
    }];
    let resp = handle_session_list(&id, &sessions);
    let list = resp["result"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["sessionId"], "abc-123");
    assert_eq!(list[0]["cwd"], "/home/user/project");
    assert_eq!(list[0]["title"], "My Session");
}
