use super::*;

#[test]
fn deployment_capability_is_immutable_and_close_invalidates_sessions() {
    let mut connection = ConnectionContext::new(true);
    assert!(!connection.apps_enabled());
    connection.commit_initialize();
    connection.commit_initialize();
    assert!(connection.apps_enabled());

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
    connection.commit_initialize();
    assert!(!connection.apps_enabled());
}
