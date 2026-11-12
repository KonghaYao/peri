use super::*;

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

fn assert_session_switch_reason(error: &AcpError, expected: &str) {
    assert_eq!(error.code, -32010, "会话切换拒绝码必须稳定");
    assert_eq!(
        error.data.as_ref().and_then(|data| data["reason"].as_str()),
        Some(expected),
        "客户端按 typed data.reason 分类：{error:?}"
    );
}

/// [回归测试] 当前连接绑定 A 后，B 必须先 close；同 ID load/resume 仍可复用。
#[test]
fn session_switch_rejects_a_to_b_and_allows_same_id_reuse() {
    let connection = ConnectionContext::new(false);
    let first = connection
        .reserve_session_setup(SessionSetupRequest::RestoresSession(Some("A")))
        .expect("空连接应取得 setup reservation");
    first.commit("A");
    let error = connection
        .reserve_session_setup(SessionSetupRequest::RestoresSession(Some("B")))
        .expect_err("未关闭 A 时 B 必须拒绝");
    assert_session_switch_reason(&error, SESSION_SWITCH_REQUIRED_REASON);
    let reuse = connection
        .reserve_session_setup(SessionSetupRequest::RestoresSession(Some("A")))
        .expect("同 ID setup 必须保持兼容");
    reuse.commit("A");
    assert_eq!(connection.current_session_id().as_deref(), Some("A"));
}

/// [回归测试] setup 的所有 early return 都由 RAII 回滚，失败请求不得占住连接。
#[test]
fn session_switch_failed_setup_reservation_rolls_back_on_drop() {
    let connection = ConnectionContext::new(false);
    let failed = connection
        .reserve_session_setup(SessionSetupRequest::CreatesSession)
        .expect("首个 setup 应取得 reservation");
    assert!(failed.commits_new_binding());
    drop(failed);
    let next = connection
        .reserve_session_setup(SessionSetupRequest::RestoresSession(Some("B")))
        .expect("失败 guard drop 后必须允许下一 setup");
    next.commit("B");
    assert_eq!(connection.current_session_id().as_deref(), Some("B"));
}

/// [回归测试] 未绑定连接上的并发 setup 只能有一个 Preparing，不能同时装配两套环境。
#[test]
fn session_switch_concurrent_setup_has_one_reservation() {
    let connection = ConnectionContext::new(false);
    let first = connection
        .reserve_session_setup(SessionSetupRequest::CreatesSession)
        .expect("首个 setup 应取得 reservation");
    let error = connection
        .reserve_session_setup(SessionSetupRequest::RestoresSession(Some("B")))
        .expect_err("并发 setup 必须拒绝");
    assert_session_switch_reason(&error, SESSION_SETUP_IN_PROGRESS_REASON);
    drop(first);
}

/// [回归测试] 只有匹配当前 ID 的成功 close/delete 才释放连接绑定。
#[test]
fn session_switch_close_releases_only_matching_current_id() {
    let connection = ConnectionContext::new(false);
    connection
        .reserve_session_setup(SessionSetupRequest::RestoresSession(Some("A")))
        .expect("A 应取得 reservation")
        .commit("A");
    connection.release_current_session("other");
    assert_eq!(connection.current_session_id().as_deref(), Some("A"));
    connection.release_current_session("A");
    assert_eq!(connection.current_session_id(), None);
    connection
        .reserve_session_setup(SessionSetupRequest::RestoresSession(Some("B")))
        .expect("匹配 close 后 B 必须放行")
        .commit("B");
    assert_eq!(connection.current_session_id().as_deref(), Some("B"));
}

/// [回归测试] 旧 reuse guard 不能覆盖 close 后建立的新 generation。
#[test]
fn session_switch_old_generation_cannot_overwrite_new_binding() {
    let connection = ConnectionContext::new(false);
    connection
        .reserve_session_setup(SessionSetupRequest::RestoresSession(Some("A")))
        .expect("A 应取得 reservation")
        .commit("A");
    let stale_reuse = connection
        .reserve_session_setup(SessionSetupRequest::RestoresSession(Some("A")))
        .expect("A 可取得 reuse guard");
    connection.release_current_session("A");
    connection
        .reserve_session_setup(SessionSetupRequest::RestoresSession(Some("B")))
        .expect("close 后 B 应取得新 generation")
        .commit("B");
    stale_reuse.commit("A");
    assert_eq!(connection.current_session_id().as_deref(), Some("B"));
}
