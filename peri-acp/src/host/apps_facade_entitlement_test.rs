//! entitlement 门控用例（R4，裁决方案 b「只接线不注入，默认全拒」）。
//!
//! 断言的都是**行为事实**：拒绝的 wire kind（`entitlement_denied`）与是否真的没有产生
//! 副作用（附件表 / 会话状态 / wire 调用），不看内部调用次数。
//!
//! 夹具默认注入「部署侧身份层」（`Fixture::deployment_authorization`），本文件的用例
//! 用 `Fixture::with_authorization` 逐一替换成未注入 / 作答 `None` / grant 错位 / 只授
//! 单项能力的桩——生产 stdio 装配**不注入**，因此 `apps/*` 六个受控方法默认拒绝。

use super::*;
use peri_acp_types::session_authorization::{
    SessionAuthorizationPort, SessionCapability, SessionEntitlementRequest, SessionGrant,
};

// ─── 部署侧身份层桩（只用于本文件） ─────────────────────────────────────────

/// 注入但**不作答**：不覆写 `grant` = 契约的默认实现（恒 `None`）。
struct DenyAll;
impl SessionAuthorizationPort for DenyAll {}

/// 恒给 grant；三元组可逐项错位（`None` = 复用请求值，即该项与请求一致）。
struct GrantAsking {
    session: Option<String>,
    caller: Option<String>,
    capability: Option<SessionCapability>,
}

impl SessionAuthorizationPort for GrantAsking {
    fn grant(&self, request: &SessionEntitlementRequest) -> Option<SessionGrant> {
        Some(SessionGrant::new(
            self.session
                .clone()
                .unwrap_or_else(|| request.session_id.clone()),
            self.caller
                .clone()
                .unwrap_or_else(|| request.caller.as_str().to_owned()),
            self.capability.unwrap_or(request.capability),
        ))
    }
}

/// 只授列出的能力（其余按「不授权」作答），session / caller 与请求一致。
struct GrantCapabilities(Vec<SessionCapability>);

impl SessionAuthorizationPort for GrantCapabilities {
    fn grant(&self, request: &SessionEntitlementRequest) -> Option<SessionGrant> {
        self.0.contains(&request.capability).then(|| {
            SessionGrant::new(
                request.session_id.clone(),
                request.caller.clone(),
                request.capability,
            )
        })
    }
}

fn deployment(
    port: impl SessionAuthorizationPort + 'static,
) -> Option<Arc<dyn SessionAuthorizationPort>> {
    Some(Arc::new(port))
}

/// `apps/attach` 的原始调用（不 panic：门控用例要断言拒绝）。
async fn attach_raw(fixture: &mut Fixture, app_id: &str) -> Result<AppsAttachResponse, AcpError> {
    let value = fixture
        .apps(
            "peri/mcp/apps/attach",
            json!({
                "sessionId": fixture.session,
                "appId": app_id,
                "clientViewId": "view-gate",
            }),
        )
        .await?;
    Ok(serde_json::from_value(value).expect("attach 响应必须可解码"))
}

// ─── 三态：未注入 / 作答 None / grant 不覆盖 ───────────────────────────────

/// 未注入端口（`None`）⇒ 拒绝，且不留下任何副作用（契约 §4.3 的 fail-closed）。
#[tokio::test]
async fn absent_port_denies_attach_without_side_effects() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::with_authorization(Arc::clone(&transport), None).await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let calls = fixture.calls();

    let error = attach_raw(&mut fixture, &app_id)
        .await
        .expect_err("未注入身份层（None）必须拒绝");
    assert_kind(&error, McpAppsErrorKind::EntitlementDenied, "未注入端口");
    assert!(
        fixture.router.attachments_for_app(&app_id).is_empty(),
        "拒绝不得留下附件（附件是副作用事实）"
    );
    assert_eq!(fixture.calls(), calls, "拒绝不得产生任何 wire 调用");
    fixture.shutdown().await;
}

/// 注入但 `grant` 返回 `None` ⇒ 拒绝（默认实现不得被当成放行）。
#[tokio::test]
async fn port_that_does_not_grant_is_denied() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture =
        Fixture::with_authorization(Arc::clone(&transport), deployment(DenyAll)).await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;

    let error = attach_raw(&mut fixture, &app_id)
        .await
        .expect_err("grant 返回 None 必须拒绝");
    assert_kind(&error, McpAppsErrorKind::EntitlementDenied, "grant = None");
    assert!(
        fixture.router.attachments_for_app(&app_id).is_empty(),
        "拒绝不得留下附件"
    );
    fixture.shutdown().await;
}

/// grant 落在**别的 session**：`covers` 不成立 ⇒ 拒绝。
#[tokio::test]
async fn grant_for_other_session_is_denied() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::with_authorization(
        Arc::clone(&transport),
        deployment(GrantAsking {
            session: Some("another-session".to_owned()),
            caller: None,
            capability: None,
        }),
    )
    .await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;

    let error = attach_raw(&mut fixture, &app_id)
        .await
        .expect_err("grant 的 session 与请求不符必须拒绝");
    assert_kind(&error, McpAppsErrorKind::EntitlementDenied, "错 session");
    assert!(
        fixture.router.attachments_for_app(&app_id).is_empty(),
        "拒绝不得留下附件"
    );
    fixture.shutdown().await;
}

/// grant 落在**别的 caller**：`covers` 不成立 ⇒ 拒绝。
#[tokio::test]
async fn grant_for_other_caller_is_denied() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::with_authorization(
        Arc::clone(&transport),
        deployment(GrantAsking {
            session: None,
            caller: Some("conn-elsewhere".to_owned()),
            capability: None,
        }),
    )
    .await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;

    let error = attach_raw(&mut fixture, &app_id)
        .await
        .expect_err("grant 的 caller 与请求不符必须拒绝");
    assert_kind(&error, McpAppsErrorKind::EntitlementDenied, "错 caller");
    assert!(
        fixture.router.attachments_for_app(&app_id).is_empty(),
        "拒绝不得留下附件"
    );
    fixture.shutdown().await;
}

/// grant 落在**别的能力**：`covers` 不成立 ⇒ 拒绝（attach 要 `AttachApps`）。
#[tokio::test]
async fn grant_for_other_capability_is_denied() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::with_authorization(
        Arc::clone(&transport),
        deployment(GrantAsking {
            session: None,
            caller: None,
            capability: Some(SessionCapability::ControlApps),
        }),
    )
    .await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;

    let error = attach_raw(&mut fixture, &app_id)
        .await
        .expect_err("grant 的能力与请求不符必须拒绝");
    assert_kind(&error, McpAppsErrorKind::EntitlementDenied, "错 capability");
    assert!(
        fixture.router.attachments_for_app(&app_id).is_empty(),
        "拒绝不得留下附件"
    );
    fixture.shutdown().await;
}

/// 三元组全等的 grant ⇒ 放行（拒绝不是无条件的）。
#[tokio::test]
async fn matching_grant_allows_attach() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::with_authorization(
        Arc::clone(&transport),
        deployment(GrantAsking {
            session: None,
            caller: None,
            capability: None,
        }),
    )
    .await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;

    let attached = attach_raw(&mut fixture, &app_id)
        .await
        .expect("三元组全等的 grant 必须放行");
    assert!(
        !attached.attachment.attachment_id.as_str().is_empty(),
        "放行后必须真的分配附件身份"
    );
    assert!(
        !fixture.router.attachments_for_app(&app_id).is_empty(),
        "放行后附件必须真实存在（与拒绝路径的副作用对照）"
    );
    fixture.shutdown().await;
}

// ─── 能力映射：只授一项时，其余能力面必须被拒 ──────────────────────────────

/// 只授 `AttachApps`：`attach` 放行，`AppOperations` 面（request）必须被拒。
#[tokio::test]
async fn attach_capability_does_not_authorize_app_operations() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::with_authorization(
        Arc::clone(&transport),
        deployment(GrantCapabilities(vec![SessionCapability::AttachApps])),
    )
    .await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = attach_raw(&mut fixture, &app_id)
        .await
        .expect("AttachApps 已授 ⇒ attach 放行");
    let calls = fixture.calls();

    let error = fixture
        .apps(
            "peri/mcp/apps/request",
            json!({
                "sessionId": fixture.session,
                "attachmentId": attached.attachment.attachment_id.as_str(),
                "epoch": attached.attachment.connection_epoch.get(),
                "seq": 1,
                "operation": Fixture::tools_call(APP_TOOL, json!({"value": 2})),
            }),
        )
        .await
        .expect_err("AppOperations 未授 ⇒ request 必须拒绝");
    assert_kind(
        &error,
        McpAppsErrorKind::EntitlementDenied,
        "只授 AttachApps",
    );
    assert_eq!(fixture.calls(), calls, "拒绝不得产生 wire 调用");
    fixture.shutdown().await;
}

/// 授 `AttachApps` + `AppOperations`：`request` 走到底，`ControlApps` 的三个方法必须被拒
/// 且不改变附件状态。
#[tokio::test]
async fn app_operations_does_not_authorize_control_apps() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::with_authorization(
        Arc::clone(&transport),
        deployment(GrantCapabilities(vec![
            SessionCapability::AttachApps,
            SessionCapability::AppOperations,
        ])),
    )
    .await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;
    let attached = attach_raw(&mut fixture, &app_id)
        .await
        .expect("AttachApps 已授 ⇒ attach 放行");
    let calls = fixture.calls();
    let _executed = fixture
        .request_wait(
            "conn-1",
            &attached.attachment,
            1,
            Fixture::tools_call(APP_TOOL, json!({"value": 2})),
        )
        .await;
    assert!(
        fixture.calls() > calls,
        "AppOperations 已授 ⇒ request 必须真的执行（wire 调用）"
    );

    let session = fixture.session.clone();
    let binding = attached.attachment.clone();
    let close_error = fixture
        .apps(
            "peri/mcp/apps/close",
            json!({"sessionId": session, "appId": app_id}),
        )
        .await
        .expect_err("ControlApps 未授 ⇒ close 必须拒绝");
    assert_kind(&close_error, McpAppsErrorKind::EntitlementDenied, "close");
    let cancel_error = fixture
        .apps(
            "peri/mcp/apps/cancel",
            json!({
                "sessionId": fixture.session,
                "attachmentId": binding.attachment_id.as_str(),
                "epoch": binding.connection_epoch.get(),
                "seq": 1,
            }),
        )
        .await
        .expect_err("ControlApps 未授 ⇒ cancel 必须拒绝");
    assert_kind(&cancel_error, McpAppsErrorKind::EntitlementDenied, "cancel");
    let detach_error = fixture
        .apps(
            "peri/mcp/apps/detach",
            json!({
                "sessionId": fixture.session,
                "attachmentId": binding.attachment_id.as_str(),
                "epoch": binding.connection_epoch.get(),
            }),
        )
        .await
        .expect_err("ControlApps 未授 ⇒ detach 必须拒绝");
    assert_kind(&detach_error, McpAppsErrorKind::EntitlementDenied, "detach");
    assert!(
        !fixture.router.attachments_for_app(&app_id).is_empty(),
        "被拒的 close / cancel / detach 不得改变附件状态"
    );
    fixture.shutdown().await;
}

/// 只授 `ControlApps`：`attach` 必须被拒，而同一会话的 `close` 放行
/// （门按能力划分，不是整面拒绝）。
#[tokio::test]
async fn control_capability_does_not_authorize_attach() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::with_authorization(
        Arc::clone(&transport),
        deployment(GrantCapabilities(vec![SessionCapability::ControlApps])),
    )
    .await;
    let app_id = fixture.instantiate(APP_TOOL, json!({"value": 1})).await;

    let error = attach_raw(&mut fixture, &app_id)
        .await
        .expect_err("AttachApps 未授 ⇒ attach 必须拒绝");
    assert_kind(&error, McpAppsErrorKind::EntitlementDenied, "attach");
    assert!(
        fixture.router.attachments_for_app(&app_id).is_empty(),
        "拒绝不得留下附件"
    );
    let closed = fixture.send_close(&app_id).await;
    assert_eq!(
        closed.verdict,
        CloseVerdict::Closed,
        "ControlApps 已授 ⇒ close 放行（门按能力划分，不是整面拒绝）"
    );
    fixture.shutdown().await;
}

/// `apps/rerun` 与 `apps/request` 同一能力门：`AppOperations` 未授时先拒，
/// 不落到 Host 域 / record 查找等更后面的判决。
#[tokio::test]
async fn rerun_shares_the_app_operations_gate() {
    let transport = FrameTransport::with_choice("allow_once");
    let mut fixture = Fixture::with_authorization(
        Arc::clone(&transport),
        deployment(GrantCapabilities(vec![
            SessionCapability::AttachApps,
            SessionCapability::ControlApps,
        ])),
    )
    .await;

    let error = fixture
        .apps(
            "peri/mcp/apps/rerun",
            json!({
                "sessionId": fixture.session,
                "hostNamespace": RequestNamespaceId::new().as_str(),
                "seq": 1,
                "sourceInvocationRef": InvocationId::generate().as_str(),
                "arguments": {"value": 1},
            }),
        )
        .await
        .expect_err("AppOperations 未授 ⇒ rerun 必须拒绝");
    assert_kind(
        &error,
        McpAppsErrorKind::EntitlementDenied,
        "rerun 与 apps/request 同一能力门",
    );
    fixture.shutdown().await;
}
