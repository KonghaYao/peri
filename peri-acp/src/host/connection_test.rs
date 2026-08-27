use super::*;
use peri_acp_types::mcp_apps::{AppSessionBinding, MCP_APPS_PROTOCOL_VERSION};

#[test]
fn deployment_capability_is_immutable_and_close_invalidates_sessions() {
    let mut connection = ConnectionContext::new(true);
    connection.commit_initialize();
    connection.commit_initialize();
    assert!(connection.apps_enabled());

    connection.insert_app_session(AppSessionBinding {
        app_session_id: "app".into(),
        owner_connection_id: connection.id().into(),
        owner_session_id: String::new(),
        server_id: "server".into(),
        server_generation: 1,
        resource_uri: "ui://app".into(),
        instantiating_tool: "open".into(),
        apps_protocol_version: MCP_APPS_PROTOCOL_VERSION.into(),
    });
    assert!(connection.app_session("app").is_some());
    connection.begin_close();
    assert!(connection.app_session("app").is_none());
    connection.finish_close();
}

#[test]
fn absent_deployment_capability_stays_disabled() {
    let mut connection = ConnectionContext::new(false);
    connection.commit_initialize();
    assert!(!connection.apps_enabled());
}
