//! MRTR（2026-07-28）`tools/call` 轮次与 elicitation 输入回归。
#![allow(deprecated)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::interaction::{
    ElicitationDecision, InteractionContext, InteractionResponse, TrustedInteractionOwner,
    UserInteractionBroker,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ElicitRequest,
    ElicitRequestParams, ElicitationSchema, InputRequest, InputRequests, InputRequiredResult,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ServerHandler;
use tokio_util::sync::CancellationToken;

use super::call_tool;
use crate::mcp::client::elicitation::ElicitationRouter;

struct AcceptBroker {
    content: serde_json::Value,
}

#[async_trait]
impl UserInteractionBroker for AcceptBroker {
    async fn request(
        &self,
        _owner: TrustedInteractionOwner,
        ctx: InteractionContext,
    ) -> InteractionResponse {
        assert!(matches!(ctx, InteractionContext::Elicitation { .. }));
        InteractionResponse::Elicitation(ElicitationDecision::Accept {
            content: self.content.clone(),
        })
    }
}

fn owner() -> TrustedInteractionOwner {
    TrustedInteractionOwner::host("peri/test", 1)
}

fn elicitation_schema() -> ElicitationSchema {
    ElicitationSchema::builder()
        .required_bool("approve")
        .optional_string("note")
        .build()
        .expect("schema")
}

fn input_required(state: &str) -> CallToolResponse {
    let mut requests = InputRequests::new();
    requests.insert(
        "approval".to_string(),
        InputRequest::Elicitation(ElicitRequest::new(
            ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: "Approve?".to_string(),
                requested_schema: elicitation_schema(),
            },
        )),
    );
    CallToolResponse::InputRequired(InputRequiredResult::new(
        Some(requests),
        Some(state.to_string()),
    ))
}

struct RoundHandler {
    calls: Arc<Mutex<Vec<CallToolRequestParams>>>,
    mode: Mode,
}

#[derive(Clone, Copy)]
enum Mode {
    TwoRounds,
    AlwaysInputRequired,
    Sampling,
}

impl ServerHandler for RoundHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            rmcp::model::Implementation::new("peri-mrtr-fixture", "tool_request_test"),
        )
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        Ok(rmcp::model::ListToolsResult::with_all_items(vec![
            Tool::new(
                "multi_round",
                "MRTR fixture",
                Arc::new(serde_json::Map::new()),
            ),
        ]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let round = {
            let mut calls = self.calls.lock();
            calls.push(request.clone());
            calls.len()
        };
        // 0728 逐请求 `_meta` 由 rmcp 从 handler info 自动注入：能力必须在 wire 上可见。
        let capabilities = context.meta.client_capabilities();
        let has_elicitation = capabilities
            .as_ref()
            .and_then(|caps| caps.elicitation.as_ref())
            .is_some_and(|elicitation| elicitation.form.is_some() && elicitation.url.is_some());
        if !has_elicitation {
            return Err(rmcp::ErrorData::internal_error(
                format!("missing elicitation meta; meta={:?}", context.meta),
                None,
            ));
        }
        match self.mode {
            Mode::TwoRounds if round == 1 => Ok(input_required("opaque-1")),
            Mode::TwoRounds => {
                let responses = request
                    .input_responses
                    .expect("round 2 carries inputResponses");
                assert_eq!(
                    responses["approval"],
                    serde_json::json!({
                        "action": "accept",
                        "content": { "approve": true, "note": "ok" }
                    })
                );
                assert_eq!(request.request_state.as_deref(), Some("opaque-1"));
                Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                    ContentBlock::text("done"),
                ])))
            }
            Mode::AlwaysInputRequired => Ok(input_required("opaque-repeat")),
            Mode::Sampling => {
                let mut requests = InputRequests::new();
                requests.insert(
                    "sample".to_string(),
                    InputRequest::CreateMessage(rmcp::model::CreateMessageRequest::new(
                        rmcp::model::CreateMessageRequestParams::new(Vec::new(), 32),
                    )),
                );
                Ok(CallToolResponse::InputRequired(InputRequiredResult::new(
                    Some(requests),
                    None,
                )))
            }
        }
    }
}

type Fixture = (
    rmcp::service::Peer<rmcp::service::RoleClient>,
    Arc<Mutex<Vec<CallToolRequestParams>>>,
    crate::mcp::client::McpServiceWrapper,
    crate::mcp::builtin::runtime::BuiltinServerTask,
);

async fn fixture(mode: Mode) -> Fixture {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let transport = crate::mcp::builtin::runtime::spawn_builtin_transport_with_handler(
        "mrtr-fixture",
        RoundHandler {
            calls: Arc::clone(&calls),
            mode,
        },
    );
    let crate::mcp::builtin::runtime::BuiltinTransport {
        io, server_task, ..
    } = transport;
    let service = crate::mcp::client::serve_client_auto(
        io,
        &crate::mcp::apps::McpCapabilityProfile::disabled(),
        Duration::from_secs(5),
    )
    .await
    .expect("handshake timeout")
    .expect("handshake");
    let peer = service.peer().clone();
    (peer, calls, service, server_task)
}

async fn close(
    mut service: crate::mcp::client::McpServiceWrapper,
    mut server_task: crate::mcp::builtin::runtime::BuiltinServerTask,
) {
    let _ = service
        .close_with_timeout(crate::mcp::builtin::runtime::BUILTIN_CONVERGE_TIMEOUT)
        .await;
    let _ = server_task
        .converge(crate::mcp::builtin::runtime::BUILTIN_CONVERGE_TIMEOUT)
        .await;
}

#[test]
fn client_info_declares_form_and_url_elicitation() {
    let info = crate::mcp::client::mcpp_client_info_for_profile(
        &crate::mcp::apps::McpCapabilityProfile::disabled(),
    );
    let value = serde_json::to_value(&info).expect("initialize info serializes");
    assert_eq!(
        value["capabilities"]["elicitation"],
        serde_json::json!({ "form": {}, "url": {} })
    );
}

#[tokio::test]
async fn two_rounds_echo_opaque_state_and_send_keyed_responses() {
    let (peer, calls, service, server_task) = fixture(Mode::TwoRounds).await;
    let router = ElicitationRouter::new("mrtr-fixture", 1);
    let lease = router.register(
        "s1",
        "multi_round",
        owner(),
        Arc::new(AcceptBroker {
            content: serde_json::json!({ "approve": true, "note": "ok" }),
        }),
        CancellationToken::new(),
    );

    let result = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        Some(&lease),
    )
    .await
    .expect("two MRTR rounds");
    let CallToolResponse::Complete(result) = result else {
        panic!("expected final CallToolResult");
    };
    assert_eq!(result.content.len(), 1);

    let (rounds, round1_responses, round1_state, approval_action) = {
        let calls = calls.lock();
        (
            calls.len(),
            calls[0].input_responses.clone(),
            calls[0].request_state.clone(),
            calls[1].input_responses.as_ref().unwrap()["approval"]["action"].clone(),
        )
    };
    assert_eq!(rounds, 2);
    assert!(round1_responses.is_none());
    assert!(round1_state.is_none());
    let request_state = calls.lock()[1].request_state.clone();
    assert_eq!(request_state.as_deref(), Some("opaque-1"));
    assert_eq!(approval_action, "accept");
    close(service, server_task).await;
}

#[tokio::test]
async fn repeated_input_required_stops_at_round_cap() {
    let (peer, calls, service, server_task) = fixture(Mode::AlwaysInputRequired).await;
    let router = ElicitationRouter::new("mrtr-fixture", 1);
    let lease = router.register(
        "s1",
        "multi_round",
        owner(),
        Arc::new(AcceptBroker {
            content: serde_json::json!({ "approve": true }),
        }),
        CancellationToken::new(),
    );
    let error = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        Some(&lease),
    )
    .await
    .expect_err("round cap must fail explicitly");
    assert!(
        matches!(
            error,
            rmcp::ServiceError::InputRequiredRoundsExceeded { max_rounds: 10 }
        ),
        "unexpected error: {error:?}"
    );
    assert_eq!(
        calls.lock().len(),
        rmcp::model::DEFAULT_MRTR_MAX_ROUNDS + 1,
        "initial attempt + cap retries"
    );
    close(service, server_task).await;
}

#[tokio::test]
async fn sampling_request_fails_explicitly_without_fabricated_answer() {
    let (peer, _calls, service, server_task) = fixture(Mode::Sampling).await;
    let router = ElicitationRouter::new("mrtr-fixture", 1);
    let lease = router.register(
        "s1",
        "multi_round",
        owner(),
        Arc::new(AcceptBroker {
            content: serde_json::json!({ "approve": true }),
        }),
        CancellationToken::new(),
    );
    let error = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        Some(&lease),
    )
    .await
    .expect_err("sampling must fail");
    assert!(
        error.to_string().contains("unsupported input request"),
        "unexpected error: {error}"
    );
    close(service, server_task).await;
}

#[tokio::test]
async fn input_required_without_lease_fails_explicitly() {
    let (peer, _calls, service, server_task) = fixture(Mode::TwoRounds).await;
    let error = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        None,
    )
    .await
    .expect_err("no interaction route must fail");
    assert!(
        error.to_string().contains("no interaction route"),
        "unexpected error: {error}"
    );
    close(service, server_task).await;
}

// ─── 2025-11-25 反向 elicitation（server → client，原 RPC ID 自动配对） ───

struct ReverseHandler {
    observed: Arc<Mutex<Vec<serde_json::Value>>>,
    url_mode: bool,
}

impl ServerHandler for ReverseHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            rmcp::model::Implementation::new("peri-reverse-fixture", "tool_request_test"),
        )
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        Ok(rmcp::model::ListToolsResult::with_all_items(vec![
            Tool::new(
                "reverse",
                "reverse fixture",
                Arc::new(serde_json::Map::new()),
            ),
        ]))
    }

    async fn call_tool(
        &self,
        _request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let params = if self.url_mode {
            ElicitRequestParams::UrlElicitationParams {
                meta: None,
                message: "Authorize".to_string(),
                url: "https://example.com/connect".to_string(),
                elicitation_id: "e-1".to_string(),
            }
        } else {
            ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: "Approve?".to_string(),
                requested_schema: elicitation_schema(),
            }
        };
        let response = context
            .peer
            .send_request(rmcp::model::ServerRequest::ElicitRequest(
                ElicitRequest::new(params),
            ))
            .await
            .map_err(|error| rmcp::ErrorData::internal_error(error.to_string(), None))?;
        let rmcp::model::ClientResult::ElicitResult(result) = response else {
            return Err(rmcp::ErrorData::internal_error(
                "unexpected client result",
                None,
            ));
        };
        self.observed
            .lock()
            .push(serde_json::to_value(&result).expect("elicit result serializes"));
        if self.url_mode {
            context
                .peer
                .send_notification(rmcp::model::ServerNotification::CustomNotification(
                    rmcp::model::CustomNotification::new(
                        "notifications/elicitation/complete",
                        Some(serde_json::json!({ "elicitationId": "e-1" })),
                    ),
                ))
                .await
                .map_err(|error| rmcp::ErrorData::internal_error(error.to_string(), None))?;
        }
        Ok(CallToolResponse::Complete(CallToolResult::success(vec![
            ContentBlock::text("ok"),
        ])))
    }
}

async fn reverse_fixture(
    url_mode: bool,
    router: Arc<ElicitationRouter>,
) -> (
    rmcp::service::Peer<rmcp::service::RoleClient>,
    Arc<Mutex<Vec<serde_json::Value>>>,
    rmcp::service::RunningService<rmcp::service::RoleClient, crate::mcp::client::PeriClientHandler>,
    crate::mcp::builtin::runtime::BuiltinServerTask,
) {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let transport = crate::mcp::builtin::runtime::spawn_builtin_transport_with_handler(
        "reverse-fixture",
        ReverseHandler {
            observed: Arc::clone(&observed),
            url_mode,
        },
    );
    let crate::mcp::builtin::runtime::BuiltinTransport {
        io, server_task, ..
    } = transport;
    let handler = crate::mcp::client::PeriClientHandler::new(
        crate::mcp::client::mcpp_client_info_for_profile(
            &crate::mcp::apps::McpCapabilityProfile::disabled(),
        ),
        None,
    )
    .with_elicitation_router(router);
    let service = rmcp::service::serve_client(handler, io)
        .await
        .expect("reverse fixture handshake");
    let peer = service.peer().clone();
    (peer, observed, service, server_task)
}

#[tokio::test]
async fn reverse_elicitation_form_accept_uses_same_interaction_entry() {
    let router = ElicitationRouter::new("reverse-fixture", 1);
    let (peer, observed, service, server_task) = reverse_fixture(false, Arc::clone(&router)).await;
    let lease = router.register(
        "s1",
        "reverse",
        owner(),
        Arc::new(AcceptBroker {
            content: serde_json::json!({ "approve": false, "note": "later" }),
        }),
        CancellationToken::new(),
    );
    let result = call_tool(
        &peer,
        CallToolRequestParams::new("reverse"),
        Some(Duration::from_secs(5)),
        Some(&lease),
    )
    .await
    .expect("reverse elicitation");
    assert!(matches!(result, CallToolResponse::Complete(_)));
    assert_eq!(
        observed.lock().as_slice(),
        &[serde_json::json!({
            "action": "accept",
            "content": { "approve": false, "note": "later" }
        })]
    );
    drop(lease);
    let _ = service;
    let _ = server_task;
}

#[tokio::test]
async fn reverse_url_accept_omits_content_and_completion_is_recorded() {
    let router = ElicitationRouter::new("reverse-fixture", 1);
    let (peer, observed, service, server_task) = reverse_fixture(true, Arc::clone(&router)).await;
    let lease = router.register(
        "s1",
        "reverse",
        owner(),
        Arc::new(AcceptBroker {
            content: serde_json::json!({ "ignored": true }),
        }),
        CancellationToken::new(),
    );
    let result = call_tool(
        &peer,
        CallToolRequestParams::new("reverse"),
        Some(Duration::from_secs(5)),
        Some(&lease),
    )
    .await
    .expect("reverse url elicitation");
    assert!(matches!(result, CallToolResponse::Complete(_)));
    assert_eq!(
        observed.lock().as_slice(),
        &[serde_json::json!({ "action": "accept" })]
    );
    // completion notification 在 call_tool 返回前后到达；给 handler 一个调度窗口。
    for _ in 0..20 {
        if router.completed_url_count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        router.completed_url_count(),
        1,
        "server→client completion notification must be recorded"
    );
    drop(lease);
    let _ = service;
    let _ = server_task;
}

/// 模拟用户思考 150ms 后再作答：工具 20ms deadline 必须因 elicitation 活动续期。
struct SlowAcceptBroker {
    delay: Duration,
}

#[async_trait]
impl UserInteractionBroker for SlowAcceptBroker {
    async fn request(
        &self,
        _owner: TrustedInteractionOwner,
        _ctx: InteractionContext,
    ) -> InteractionResponse {
        peri_time::sleep(self.delay).await;
        InteractionResponse::Elicitation(ElicitationDecision::Accept {
            content: serde_json::json!({ "approve": true }),
        })
    }
}

#[tokio::test]
async fn elicitation_activity_renews_the_tool_deadline() {
    let router = ElicitationRouter::new("reverse-fixture", 1);
    let (peer, observed, service, server_task) = reverse_fixture(false, Arc::clone(&router)).await;
    let lease = router.register(
        "s1",
        "reverse",
        owner(),
        Arc::new(SlowAcceptBroker {
            delay: Duration::from_millis(150),
        }),
        CancellationToken::new(),
    );
    let result = call_tool(
        &peer,
        CallToolRequestParams::new("reverse"),
        Some(Duration::from_millis(20)),
        Some(&lease),
    )
    .await
    .expect("user thinking must renew the tool deadline");
    assert!(matches!(result, CallToolResponse::Complete(_)));
    assert_eq!(
        observed.lock().as_slice(),
        &[serde_json::json!({
            "action": "accept",
            "content": { "approve": true }
        })]
    );
    drop(lease);
    let _ = service;
    let _ = server_task;
}

/// 交互通道超时（用户始终不答）→ 反向 elicitation 必须回 cancel，而不是协议错误。
struct TimeoutBroker;

#[async_trait]
impl UserInteractionBroker for TimeoutBroker {
    async fn request(
        &self,
        _owner: TrustedInteractionOwner,
        _ctx: InteractionContext,
    ) -> InteractionResponse {
        InteractionResponse::Failed {
            failure: peri_acp_types::interaction::InteractionFailure::Timeout,
        }
    }
}

#[tokio::test]
async fn broker_timeout_returns_cancel_to_the_server() {
    let router = ElicitationRouter::new("reverse-fixture", 1);
    let (peer, observed, service, server_task) = reverse_fixture(false, Arc::clone(&router)).await;
    let lease = router.register(
        "s1",
        "reverse",
        owner(),
        Arc::new(TimeoutBroker),
        CancellationToken::new(),
    );
    let result = call_tool(
        &peer,
        CallToolRequestParams::new("reverse"),
        Some(Duration::from_secs(1)),
        Some(&lease),
    )
    .await
    .expect("timeout is a cancellation, not a channel failure");
    assert!(matches!(result, CallToolResponse::Complete(_)));
    assert_eq!(
        observed.lock().as_slice(),
        &[serde_json::json!({ "action": "cancel" })]
    );
    drop(lease);
    let _ = service;
    let _ = server_task;
}

/// 工具调用结束（lease Drop 取消令牌）时仍在等待的 elicitation 必须以 cancel 结算。
#[tokio::test]
async fn cancelling_the_lease_token_ends_an_in_flight_elicitation() {
    struct HangingBroker;
    #[async_trait]
    impl UserInteractionBroker for HangingBroker {
        async fn request(
            &self,
            _owner: TrustedInteractionOwner,
            _ctx: InteractionContext,
        ) -> InteractionResponse {
            std::future::pending().await
        }
    }

    let router = ElicitationRouter::new("reverse-fixture", 1);
    let (peer, observed, service, server_task) = reverse_fixture(false, Arc::clone(&router)).await;
    let cancel = CancellationToken::new();
    let lease = router.register(
        "s1",
        "reverse",
        owner(),
        Arc::new(HangingBroker),
        cancel.clone(),
    );
    let call = tokio::spawn({
        let peer = peer.clone();
        async move {
            call_tool(
                &peer,
                CallToolRequestParams::new("reverse"),
                Some(Duration::from_secs(5)),
                Some(&lease),
            )
            .await
        }
    });
    // 等 server 真正发出 elicitation，再结束工具调用。
    for _ in 0..50 {
        if observed.lock().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        } else {
            break;
        }
    }
    // 让 lease 随调用结束；handler 必须收到 cancel 并回包，server 侧的
    // `observed` 在 elicitation 返回后才写入，所以这里只断言不挂死。
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), call)
        .await
        .expect("cancelled elicitation must not hang the tool call")
        .expect("tool call settles");
    assert_eq!(
        observed.lock().as_slice(),
        &[serde_json::json!({ "action": "cancel" })]
    );
    let _ = service;
    let _ = server_task;
}
