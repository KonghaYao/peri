use super::*;
use peri_acp_types::interaction::AttachmentId;
use peri_acp_types::mcp_apps::{
    McpAppInvokeOutcome, McpAppsRelayError, McpAppsRelayPort, RawResource,
};
use peri_acp_types::mcp_apps_facade::{
    AppAttachRequest, AppAttachSnapshot, AppDescriptor, AttachmentDetachReason, CloseAppOutcome,
    CloseVerdict, ResourceList, ResourceListRequest, ResourceReadRequest, TargetIdentity,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct StubRelay {
    invoke: std::sync::Mutex<Option<Result<McpAppInvokeOutcome, McpAppsErrorKind>>>,
}

impl StubRelay {
    fn invoke_ok() -> Self {
        Self {
            invoke: std::sync::Mutex::new(Some(Ok(McpAppInvokeOutcome {
                mcp_protocol_version: "2025-03-26".into(),
                tool_call_id: "tool-new-1".into(),
                effective_tool_name: "mcp__cursor-canvas__show_canvas".into(),
                arguments: serde_json::Map::from_iter([(
                    "source".into(),
                    json!("export default function App(){return null}"),
                )]),
                output: "ok".into(),
            }))),
        }
    }
}

#[async_trait::async_trait]
impl McpAppsRelayPort for StubRelay {
    fn close_connection(&self, _: &str) {}
    fn close_session(&self, _: &str) {}

    async fn binding_protocol_version(&self, _: &str) -> Result<String, McpAppsRelayError> {
        Err(McpAppsRelayError {
            kind: McpAppsErrorKind::UnsupportedMethod,
        })
    }

    async fn invoke_app(
        &self,
        _: &McpAppInvokeRequest,
        _: peri_acp_types::mcp_apps::HostInvocationRef,
        _: CancellationToken,
    ) -> Result<McpAppInvokeOutcome, McpAppsRelayError> {
        match self.invoke.lock().unwrap().take() {
            Some(Ok(outcome)) => Ok(outcome),
            Some(Err(kind)) => Err(McpAppsRelayError { kind }),
            None => Err(McpAppsRelayError {
                kind: McpAppsErrorKind::UpstreamProtocolError,
            }),
        }
    }

    // ── App facts 面：本桩只用于旧 4 方法的 wire 层用例；未实现的入口一律
    //    **显式**报 `UnsupportedMethod`（fail-closed，不静默降级）。
    //    4a 的真实路径证据在 `mcp_apps_idle_test.rs`（真实 relay）。

    fn runtime_epoch(&self) -> u64 {
        1
    }

    async fn list_apps(&self, _: &str) -> Result<Vec<AppDescriptor>, McpAppsRelayError> {
        Err(McpAppsRelayError {
            kind: McpAppsErrorKind::UnsupportedMethod,
        })
    }

    async fn attach_app(
        &self,
        _: AppAttachRequest,
    ) -> Result<AppAttachSnapshot, McpAppsRelayError> {
        Err(McpAppsRelayError {
            kind: McpAppsErrorKind::UnsupportedMethod,
        })
    }

    fn detach_attachment(&self, _: &AttachmentId, _: AttachmentDetachReason) {}

    fn close_app(&self, _: &str, _: &str) -> CloseAppOutcome {
        CloseAppOutcome {
            verdict: CloseVerdict::AlreadyClosed,
            detached_attachments: Vec::new(),
        }
    }

    async fn read_resources(
        &self,
        _: ResourceReadRequest,
    ) -> Result<Vec<RawResource>, McpAppsRelayError> {
        Err(McpAppsRelayError {
            kind: McpAppsErrorKind::UnsupportedMethod,
        })
    }

    async fn list_resources(
        &self,
        _: ResourceListRequest,
    ) -> Result<ResourceList, McpAppsRelayError> {
        Err(McpAppsRelayError {
            kind: McpAppsErrorKind::UnsupportedMethod,
        })
    }

    fn source_target(
        &self,
        _: &str,
        _: &peri_acp_types::tool_invocation::InvocationId,
    ) -> Result<TargetIdentity, McpAppsRelayError> {
        Err(McpAppsRelayError {
            kind: McpAppsErrorKind::UnsupportedMethod,
        })
    }
}

fn enabled_connection() -> ConnectionContext {
    let mut connection = ConnectionContext::new(true);
    connection.commit_initialize(peri_acp_types::PeriCaps::default());
    connection
}

fn invoke_params() -> Value {
    json!({
        "envelopeVersion": MCP_APPS_ENVELOPE_VERSION,
        "appsProtocolVersion": MCP_APPS_PROTOCOL_VERSION,
        "serverId": "cursor-canvas",
        "toolName": "show_canvas",
        "ownerSessionId": "acp-session-1",
        "arguments": {"source": "export default function App(){return null}"}
    })
}

#[test]
fn version_validation_is_fail_closed() {
    assert!(validate_versions(MCP_APPS_ENVELOPE_VERSION, MCP_APPS_PROTOCOL_VERSION).is_ok());
    let error = validate_versions("2", MCP_APPS_PROTOCOL_VERSION).unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "unsupported_envelope_version");
}

// ── 测试侧 facade（`handle_invoke` 的归属值来源） ────────────────────────────

/// 记录通知的极简 transport（facade 的 endpoint 注册需要一条投递面）。
#[derive(Default)]
struct RecordingTransport;

#[async_trait::async_trait]
impl crate::transport::AcpTransport for RecordingTransport {
    async fn send_request(
        &self,
        _: &str,
        _: Value,
    ) -> Result<Value, crate::transport::types::AcpError> {
        Ok(Value::Null)
    }

    async fn send_notification(
        &self,
        _: &str,
        _: Value,
    ) -> Result<(), crate::transport::types::AcpError> {
        Ok(())
    }

    async fn recv(&self) -> Option<crate::transport::types::IncomingMessage> {
        None
    }

    async fn send_response(
        &self,
        _: crate::transport::types::RequestId,
        _: Result<Value, crate::transport::types::AcpError>,
    ) -> Result<(), crate::transport::types::AcpError> {
        Ok(())
    }
}

/// 未接线的 canonical 入口桩：本文件的用例都停在归属 / 门禁 / relay 投影面。
struct StubInvocation;

#[async_trait::async_trait]
impl peri_acp_types::mcp_apps::SessionInvocationPort for StubInvocation {
    async fn invoke(
        &self,
        _: peri_acp_types::mcp_apps::SessionInvocationRequest,
    ) -> Result<
        peri_acp_types::mcp_apps::SessionInvocationOutcome,
        peri_acp_types::mcp_apps::McpAppsRelayError,
    > {
        Err(McpAppsRelayError {
            kind: McpAppsErrorKind::UnsupportedMethod,
        })
    }

    fn session_closing(&self) {}

    fn revoke_app(&self, _: &str) {}

    fn revoke_attachment(&self, _: &AttachmentId) {}

    fn source_target(
        &self,
        _: &str,
        _: &peri_acp_types::tool_invocation::InvocationId,
    ) -> Option<TargetIdentity> {
        None
    }
}

/// facade + 任务 owner（owner 必须存活，spawner 经 `Weak` 引用它）。
struct TestFacade {
    facade: Arc<SessionAppsFacade>,
    #[allow(dead_code)] // 观察面：facade 的 endpoint 注册事实
    router: Arc<super::super::interaction_router::SessionInteractionRouter>,
    _owner: crate::host::task_scope::HostTaskOwner,
}

impl TestFacade {
    fn new(relay: Arc<dyn McpAppsRelayPort>, connection_ids: &[&str]) -> Self {
        let router = Arc::new(
            super::super::interaction_router::SessionInteractionRouter::new(
                "mcp-apps-test",
                Arc::new(tokio::sync::Mutex::new(())),
            ),
        );
        for connection_id in connection_ids {
            router
                .register_connection(
                    connection_id,
                    Arc::new(RecordingTransport) as Arc<dyn crate::transport::AcpTransport>,
                )
                .expect("测试连接必须可注册");
        }
        let (owner, spawner) = crate::host::task_scope::HostTaskOwner::new();
        let facade = Arc::new(SessionAppsFacade::new(
            "mcp-apps-test",
            1,
            Arc::clone(&router),
            relay,
            Arc::new(StubInvocation) as Arc<dyn peri_acp_types::mcp_apps::SessionInvocationPort>,
            spawner,
            // 生产 stdio 装配不注入身份层（默认拒绝）；夹具模拟「部署侧已接入身份层」。
            crate::host::apps_facade::entitlement::test_support::allow_all(),
        ));
        Self {
            facade,
            router,
            _owner: owner,
        }
    }
}

#[tokio::test]
async fn invoke_disabled_capability_fails_closed() {
    let mut connection = ConnectionContext::new(false);
    connection.commit_initialize(peri_acp_types::PeriCaps::default());
    let relay: Arc<dyn McpAppsRelayPort> = Arc::new(StubRelay::invoke_ok());
    let error = handle_invoke(
        &invoke_params(),
        &connection,
        Some(&relay),
        None,
        InvokeSessionGate {
            known: true,
            open: true,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "capability_disabled");
}

#[tokio::test]
async fn invoke_unknown_session_fails_closed() {
    let connection = enabled_connection();
    let relay: Arc<dyn McpAppsRelayPort> = Arc::new(StubRelay::invoke_ok());
    let test_facade = TestFacade::new(Arc::clone(&relay), &[connection.id()]);
    let error = handle_invoke(
        &invoke_params(),
        &connection,
        Some(&relay),
        Some(&test_facade.facade),
        InvokeSessionGate {
            known: false,
            open: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "invalid_session");
}

/// R8 取代（原用例 `invoke_during_prompt_is_policy_denied`，钉死「会话有在飞 prompt ⇒
/// `policy_denied`」）：准入面**没有**「会话忙 / prompt 在飞」这一维。
///
/// 旧判据读 `SessionState.cancel_token` 的布尔快照——prompt 与 invoke 之间无锁序、
/// 无队列，它不提供真实互斥；却让别名与 canonical 面（`apps/*` 全链不读 prompt 状态）
/// 判决不同。删除后本面只剩 `known` / `open` 两条检查，故同一会话连续两次 invoke 都
/// **走到 relay 执行面**：第一次取到 relay 的真实回执，第二次的拒绝来自 relay 自己的
/// 错误面（`upstream_protocol_error`），而不是任何「会话忙」门（那才会是 `policy_denied`）。
///
/// 「prompt 真在飞」这一事实在删除后不再是本面的输入（读 `cancel_token` 的代码已随
/// R8 消失），因此本用例无法在本层设置它；该场景的证据缺口见
/// `spec/issues/2026-10-12-mcp-apps-redesign/implementation-log.md` 的「4b 批 G」。
#[tokio::test]
async fn invoke_is_admitted_without_session_busy_refusal() {
    let connection = enabled_connection();
    let relay: Arc<dyn McpAppsRelayPort> = Arc::new(StubRelay::invoke_ok());
    let test_facade = TestFacade::new(Arc::clone(&relay), &[connection.id()]);
    let admitted = handle_invoke(
        &invoke_params(),
        &connection,
        Some(&relay),
        Some(&test_facade.facade),
        InvokeSessionGate {
            known: true,
            open: true,
        },
    )
    .await
    .expect("会话存在且未关闭 ⇒ 必须准入并走到 relay 执行面");
    assert_eq!(admitted.value["toolCallId"], "tool-new-1");

    // 第二次 invoke：准入面无状态可选，拒绝只能来自 relay 自己的错误面。
    let error = handle_invoke(
        &invoke_params(),
        &connection,
        Some(&relay),
        Some(&test_facade.facade),
        InvokeSessionGate {
            known: true,
            open: true,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.data.unwrap()["kind"],
        "upstream_protocol_error",
        "拒绝必须来自 relay（桩只应答一次），而不是准入层的「会话忙」门"
    );
}

/// 正在关闭的会话不能执行工具或发送新的工具更新。
#[tokio::test]
async fn invoke_while_session_is_closing_is_policy_denied() {
    let connection = enabled_connection();
    let relay: Arc<dyn McpAppsRelayPort> = Arc::new(StubRelay::invoke_ok());
    let test_facade = TestFacade::new(Arc::clone(&relay), &[connection.id()]);
    let error = handle_invoke(
        &invoke_params(),
        &connection,
        Some(&relay),
        Some(&test_facade.facade),
        InvokeSessionGate {
            known: true,
            open: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "policy_denied");
}

#[tokio::test]
async fn invoke_relay_policy_denial_does_not_project_success() {
    let connection = enabled_connection();
    let relay: Arc<dyn McpAppsRelayPort> = Arc::new(StubRelay {
        invoke: std::sync::Mutex::new(Some(Err(McpAppsErrorKind::PolicyDenied))),
    });
    let test_facade = TestFacade::new(Arc::clone(&relay), &[connection.id()]);
    let error = handle_invoke(
        &invoke_params(),
        &connection,
        Some(&relay),
        Some(&test_facade.facade),
        InvokeSessionGate {
            known: true,
            open: true,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "policy_denied");
}

#[tokio::test]
async fn invoke_success_returns_tool_call_id_without_synthetic_model_cards() {
    let connection = enabled_connection();
    let relay: Arc<dyn McpAppsRelayPort> = Arc::new(StubRelay::invoke_ok());
    let test_facade = TestFacade::new(Arc::clone(&relay), &[connection.id()]);
    let result = handle_invoke(
        &invoke_params(),
        &connection,
        Some(&relay),
        Some(&test_facade.facade),
        InvokeSessionGate {
            known: true,
            open: true,
        },
    )
    .await
    .expect("invoke");
    // R3：Host 回调只经原请求返回结果，不产生冒充模型工具调用的 session/update。
    assert_eq!(result.value["toolCallId"], "tool-new-1");
    assert_eq!(result.value["serverId"], "cursor-canvas");

    assert!(
        result.value.get("updates").is_none(),
        "Host invoke 响应不得携带合成模型卡片"
    );
}

#[tokio::test]
async fn invoke_unknown_method_through_request_router_is_unsupported() {
    let mut connection = enabled_connection();
    // `peri/mcp/invoke` 由 `handle_invoke` 分支受理（server_loop），不经本分发面；
    // 旧 relay 面自 S6 起也不再参与 `handle_request` 的任何分支（别名走 facade）。
    let relay: Arc<dyn McpAppsRelayPort> = Arc::new(StubRelay::invoke_ok());
    let test_facade = TestFacade::new(relay, &[connection.id()]);
    let error = handle_request(
        "peri/mcp/invoke",
        &invoke_params(),
        &mut connection,
        Some(&test_facade.facade),
    )
    .await
    .unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "unsupported_method");
}

/// 旧 4 名与新 7 名是**同一条链**（裁决 D1）：没有 facade 时一律 `capability_disabled`
/// ——别名不再回落 relay 面，新方法不自建第二执行路径。
#[tokio::test]
async fn legacy_aliases_and_new_methods_share_the_same_facade_requirement() {
    let relay_present: Option<&Arc<dyn McpAppsRelayPort>> = None;
    let _ = relay_present;
    for method in [
        "peri/mcp/apps/list",
        "peri/mcp/apps/attach",
        "peri/mcp/apps/request",
        "peri/mcp/apps/cancel",
        "peri/mcp/apps/detach",
        "peri/mcp/apps/close",
        "peri/mcp/apps/rerun",
        "peri/mcp/open",
        "peri/mcp/app",
        "peri/mcp/resource",
    ] {
        let mut connection = enabled_connection();
        let error = handle_request(method, &json!({}), &mut connection, None)
            .await
            .unwrap_err();
        assert_eq!(
            error.data.unwrap()["kind"],
            "capability_disabled",
            "{method} 未装配 facade 时必须能力关闭（同一执行链，无第二路径）"
        );
    }
}

/// 能力关闭的连接：新旧方法一律 `capability_disabled`（能力门只有一处）。
#[tokio::test]
async fn capability_disabled_connection_is_refused_for_new_methods() {
    let mut connection = ConnectionContext::new(false);
    connection.commit_initialize(peri_acp_types::PeriCaps::default());
    let error = handle_request("peri/mcp/apps/list", &json!({}), &mut connection, None)
        .await
        .unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "capability_disabled");
}
