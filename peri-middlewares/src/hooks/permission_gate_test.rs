use super::*;

fn external(name: &str) -> BoundToolOrigin {
    BoundToolOrigin {
        mcp_server_name: Some("external".to_string()),
        mcp_tool_name: Some(name.to_string()),
        builtin_mcp_instance: None,
    }
}

/// [回归测试] raw Read 的外部 MCP 来源也触发 PermissionRequest。
#[test]
fn test_external_raw_read_fires_permission_request() {
    let origin = external("Read");
    assert!(should_fire_permission_request_for_origin(
        PermissionMode::Default,
        "Read",
        crate::permission::default_requires_approval,
        Some(&origin),
    ));
}

/// [回归测试] AcceptEdit 不能将外部 raw Write 视为本地可信编辑工具。
#[test]
fn test_external_raw_write_fires_in_accept_edit_mode() {
    let origin = external("Write");
    assert!(should_fire_permission_request_for_origin(
        PermissionMode::AcceptEdit,
        "Write",
        crate::permission::default_requires_approval,
        Some(&origin),
    ));
}

#[test]
fn test_builtin_read_does_not_fire_permission_request() {
    let origin = BoundToolOrigin {
        mcp_server_name: Some("workspace".to_string()),
        mcp_tool_name: Some("Read".to_string()),
        builtin_mcp_instance: Some("workspace".to_string()),
    };
    assert!(!should_fire_permission_request_for_origin(
        PermissionMode::Default,
        "Read",
        crate::permission::default_requires_approval,
        Some(&origin),
    ));
}
