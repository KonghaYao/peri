use super::*;

/// 协商成功前 `history_v2` 必须为 false；只有携带服务端回显的 caps 才打开。
#[test]
fn history_v2_opens_only_after_a_successful_negotiation() {
    let mut connection = ConnectionContext::new(false);
    assert!(!connection.history_v2());
    assert!(!connection.facts().history_v2);

    connection.commit_initialize(PeriCaps::default());
    assert!(!connection.history_v2());
    assert!(!connection.facts().history_v2);

    let mut negotiated = ConnectionContext::new(false);
    negotiated.commit_initialize(PeriCaps {
        history_v2: Some(peri_acp_types::history_v2::HistoryServerCapability {
            version: peri_acp_types::history_v2::HISTORY_V2_VERSION,
            selected_profile: peri_acp_types::history_v2::HISTORY_PROFILE_WINDOWED.to_owned(),
            method: peri_acp_types::history_v2::HISTORY_METHOD.to_owned(),
            notification: peri_acp_types::history_v2::HISTORY_CHANGED_NOTIFICATION.to_owned(),
            limits: peri_acp_types::history_v2::HistoryLimits::default(),
        }),
        ..PeriCaps::default()
    });
    assert!(negotiated.history_v2());
    assert!(negotiated.facts().history_v2);

    // 只认第一次 initialize：后一次不能把已建立的 V2 抹掉。
    negotiated.commit_initialize(PeriCaps::default());
    assert!(negotiated.history_v2());
}

#[test]
fn deployment_capability_is_immutable_and_close_invalidates_sessions() {
    let mut connection = ConnectionContext::new(true);
    assert!(!connection.apps_enabled());
    connection.commit_initialize(PeriCaps::default());
    connection.commit_initialize(PeriCaps::all_enabled());
    assert!(connection.apps_enabled());
    assert_eq!(connection.caps(), PeriCaps::default());

    // 关闭期间能力门必须立即失效；关闭完成后保持失效。
    // （旧 lease 表已随 S8 删除：别名与新方法的附件身份都由 router 分配，
    // 连接上下文不再持有任何 App 会话权威。）
    connection.begin_close();
    assert!(!connection.apps_enabled());
    connection.finish_close();
    assert!(!connection.apps_enabled());
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

#[test]
fn connection_context_remains_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ConnectionContext>();
    let snapshot = ConnectionContext::new(false).snapshot_for_request();
    assert_send_sync::<ConnectionContext>();
    drop(snapshot);
}

#[test]
fn mcp_control_requires_current_connection_initialize_and_open_lifecycle() {
    let mut absent = ConnectionContext::new(false);
    assert!(absent.caps().mcp_control_v1);
    assert_eq!(absent.admission_caps(), PeriCaps::default());
    assert!(!absent.mcp_control_v1());
    absent.commit_initialize(PeriCaps::default());
    assert!(!absent.mcp_control_v1());
    let mut enabled = ConnectionContext::new(false);
    enabled.commit_initialize(PeriCaps {
        mcp_control_v1: true,
        ..PeriCaps::default()
    });
    assert!(enabled.facts().mcp_control_v1);
    assert!(enabled.admission_caps().mcp_control_v1);
    assert!(!ConnectionContext::new(false).mcp_control_v1());
    enabled.begin_close();
    assert!(!enabled.facts().mcp_control_v1);
    assert_eq!(enabled.admission_caps(), PeriCaps::default());
}
