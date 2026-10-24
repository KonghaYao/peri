use super::*;

#[test]
fn external_task_routes_are_session_and_server_bound_and_released() {
    let mut bindings = SessionBindings::default();
    bindings.current.insert("A".into(), Binding::default());
    bindings.current.insert("B".into(), Binding::default());
    bindings
        .record_external_task_route("A", "mcp-public", "workspace", "shell-raw")
        .unwrap();
    assert_eq!(
        bindings
            .resolve_external_task_route("A", "mcp-public", "workspace")
            .unwrap()
            .raw_id,
        "shell-raw"
    );
    assert_eq!(
        bindings
            .resolve_external_task_route("B", "mcp-public", "workspace")
            .unwrap_err(),
        "unknown task for this session"
    );
    assert_eq!(
        bindings
            .resolve_external_task_route("A", "shell-raw", "workspace")
            .unwrap_err(),
        "unknown task for this session"
    );
    assert_eq!(
        bindings
            .resolve_external_task_route("A", "mcp-public", "other")
            .unwrap_err(),
        "task belongs to a different server"
    );
    assert!(bindings
        .record_external_task_route("A", "mcp-public", "other", "shell-raw")
        .is_err());
    bindings.release("A");
    assert!(bindings
        .resolve_external_task_route("A", "mcp-public", "workspace")
        .is_err());
    assert!(bindings
        .record_external_task_route("A", "mcp-public", "workspace", "shell-raw")
        .is_err());
}
