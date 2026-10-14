use super::*;

/// 未覆写 `grant` 的实现（= 未接入可信身份层）。
struct NotWired;

impl SessionAuthorizationPort for NotWired {}

/// 部署侧身份层：只授权 conn-1 对 session-1 的 attach。
struct DeploymentIdentity;

impl SessionAuthorizationPort for DeploymentIdentity {
    fn grant(&self, request: &SessionEntitlementRequest) -> Option<SessionGrant> {
        (request.caller.as_str() == "conn-1"
            && request.session_id == "session-1"
            && request.capability == SessionCapability::AttachApps)
            .then(|| SessionGrant::new("session-1", "conn-1", SessionCapability::AttachApps))
    }
}

fn attach_request() -> SessionEntitlementRequest {
    SessionEntitlementRequest::new("session-1", "conn-1", SessionCapability::AttachApps)
}

#[test]
fn default_implementation_is_fail_closed() {
    let port: &dyn SessionAuthorizationPort = &NotWired;
    for capability in [
        SessionCapability::AttachApps,
        SessionCapability::AppOperations,
        SessionCapability::ControlApps,
    ] {
        let request = SessionEntitlementRequest::new("session-1", "conn-1", capability);
        assert_eq!(port.grant(&request), None, "默认实现不得授权任何请求");
    }
}

#[test]
fn unknown_caller_is_denied() {
    let port: &dyn SessionAuthorizationPort = &DeploymentIdentity;
    // 「猜到 session id」不构成授权：调用者身份不匹配即拒绝。
    let request =
        SessionEntitlementRequest::new("session-1", "conn-2", SessionCapability::AttachApps);
    assert_eq!(port.grant(&request), None);
}

#[test]
fn capability_is_part_of_the_grant() {
    let port: &dyn SessionAuthorizationPort = &DeploymentIdentity;
    let request =
        SessionEntitlementRequest::new("session-1", "conn-1", SessionCapability::AppOperations);
    assert_eq!(
        port.grant(&request),
        None,
        "attach 授权不得外溢成 App 操作授权"
    );
}

#[test]
fn grant_must_cover_the_exact_request() {
    let grant = SessionGrant::new("session-1", "conn-1", SessionCapability::AttachApps);
    assert!(grant.covers(&attach_request()));

    // 端口返回「别的 session / 别的调用者 / 别的能力」的 grant 都不构成本次授权。
    assert!(!grant.covers(&SessionEntitlementRequest::new(
        "session-2",
        "conn-1",
        SessionCapability::AttachApps
    )));
    assert!(!grant.covers(&SessionEntitlementRequest::new(
        "session-1",
        "conn-2",
        SessionCapability::AttachApps
    )));
    assert!(!grant.covers(&SessionEntitlementRequest::new(
        "session-1",
        "conn-1",
        SessionCapability::ControlApps
    )));

    assert_eq!(grant.session_id(), "session-1");
    assert_eq!(grant.caller().as_str(), "conn-1");
    assert_eq!(grant.capability(), SessionCapability::AttachApps);
}

#[test]
fn authorized_request_yields_covering_grant() {
    let port: &dyn SessionAuthorizationPort = &DeploymentIdentity;
    let request = attach_request();
    let grant = port.grant(&request).expect("部署身份层授权 attach");
    assert!(grant.covers(&request), "返回的 grant 必须覆盖本次请求");
}

#[test]
fn capability_round_trips_through_wire_shape() {
    assert_eq!(
        serde_json::to_value(SessionCapability::AppOperations).expect("capability serializes"),
        serde_json::json!("app_operations")
    );
}
