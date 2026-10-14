use super::*;
use peri_acp_types::mcp_apps::{AppSessionBinding, MCP_APPS_PROTOCOL_VERSION};

#[test]
fn deployment_capability_is_immutable_and_close_invalidates_sessions() {
    let mut connection = ConnectionContext::new(true);
    assert!(!connection.apps_enabled());
    connection.commit_initialize(PeriCaps::default());
    connection.commit_initialize(PeriCaps::all_enabled());
    assert!(connection.apps_enabled());
    assert_eq!(connection.caps(), PeriCaps::default());

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
    connection.commit_initialize(PeriCaps::default());
    assert!(!connection.apps_enabled());
}

/// 未调用 `initialize` 的进程内路径（MpscTransport / TUI）保持历史全能力语义；
/// 显式协商的连接只拿到本连接声明的那一份，且不受后一次 initialize 覆盖。
#[test]
fn connection_caps_fall_back_to_all_enabled_only_before_initialize() {
    let in_process = ConnectionContext::new(false);
    assert_eq!(in_process.caps(), PeriCaps::all_enabled());

    let mut negotiated = ConnectionContext::new(false);
    negotiated.commit_initialize(PeriCaps {
        history_pagination: true,
        ..PeriCaps::default()
    });
    assert_eq!(
        negotiated.caps(),
        PeriCaps {
            history_pagination: true,
            ..PeriCaps::default()
        }
    );
    assert_eq!(negotiated.snapshot_for_request().caps(), negotiated.caps());
}

/// 分页准入与 `caps()` 的兜底**刻意不同**（REQ §5.3 / A25）：`caps()` 在未协商时给出
/// 全能力兜底，分页准入却必须失败关闭，否则「从未协商」会被读成「默认获得分页语义」。
#[test]
fn history_pagination_stays_closed_until_this_connection_negotiates_it() {
    let mut unnegotiated = ConnectionContext::new(false);
    assert!(unnegotiated.caps().history_pagination);
    assert!(!unnegotiated.history_pagination());
    assert!(!unnegotiated.snapshot_for_request().history_pagination());

    unnegotiated.commit_initialize(PeriCaps::default());
    assert!(!unnegotiated.history_pagination());

    let mut negotiated = ConnectionContext::new(false);
    negotiated.commit_initialize(PeriCaps {
        history_pagination: true,
        ..PeriCaps::default()
    });
    assert!(negotiated.history_pagination());

    // 只认**第一次** initialize：后一次不能把准入从关改成开，也不能把它改回去。
    negotiated.commit_initialize(PeriCaps::default());
    assert!(negotiated.history_pagination());
}
