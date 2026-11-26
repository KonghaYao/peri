//! MRTR（2026-07-28）`tools/call` 轮次、能力声明与 elicitation 输入回归。
#![allow(deprecated)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::interaction::{
    ElicitationDecision, InteractionContext, InteractionResponse, TrustedInteractionOwner,
    UserInteractionBroker,
};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ContentBlock,
    ElicitRequest, ElicitRequestParams, ElicitationSchema, InitializeRequestParams,
    InitializeResult, InputRequest, InputRequests, InputRequiredResult, ProtocolVersion,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ServerHandler;
use tokio_util::sync::CancellationToken;

use super::call_tool;
use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::client::{client_capabilities_for_call, InteractionRoute};

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

struct RecordingBroker {
    decisions: Mutex<Vec<ElicitationDecision>>,
    seen: Mutex<Vec<InteractionContext>>,
}

impl RecordingBroker {
    fn new(decisions: Vec<ElicitationDecision>) -> Arc<Self> {
        Arc::new(Self {
            decisions: Mutex::new(decisions),
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl UserInteractionBroker for RecordingBroker {
    async fn request(
        &self,
        _owner: TrustedInteractionOwner,
        ctx: InteractionContext,
    ) -> InteractionResponse {
        self.seen.lock().push(ctx);
        let decision = {
            let mut decisions = self.decisions.lock();
            assert!(!decisions.is_empty(), "unexpected extra elicitation");
            decisions.remove(0)
        };
        InteractionResponse::Elicitation(decision)
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

fn form_input_required(state: Option<&str>) -> CallToolResponse {
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
        state.map(str::to_string),
    ))
}

/// 0728 URL：MCP 自身不再提供 `elicitationId`（rmcp 用空串表示）。
fn url_input_required(state: Option<&str>) -> CallToolResponse {
    let mut requests = InputRequests::new();
    requests.insert(
        "authorize".to_string(),
        InputRequest::Elicitation(ElicitRequest::new(
            ElicitRequestParams::UrlElicitationParams {
                meta: None,
                message: "Authorize in browser".to_string(),
                url: "https://example.com/connect".to_string(),
                elicitation_id: String::new(),
            },
        )),
    );
    CallToolResponse::InputRequired(InputRequiredResult::new(
        Some(requests),
        state.map(str::to_string),
    ))
}

fn fixed_result() -> CallToolResponse {
    CallToolResponse::Complete(CallToolResult::success(vec![ContentBlock::text(
        "receipt-001",
    )]))
}

struct RoundHandler {
    calls: Arc<Mutex<Vec<CallToolRequestParams>>>,
    metas: Arc<Mutex<Vec<serde_json::Value>>>,
    parallel_rounds: Arc<Mutex<HashMap<String, usize>>>,
    mode: Mode,
    declared_on_initialize: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[derive(Clone, Copy)]
enum Mode {
    Plain,
    TwoRounds,
    CancelRound,
    AlwaysInputRequired,
    Sampling,
    UrlTwoRounds,
    UrlTwice,
    StateOmitted,
    Parallel,
    Slow,
}

impl ServerHandler for RoundHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            rmcp::model::Implementation::new("peri-mrtr-fixture", "tool_request_test"),
        )
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, rmcp::ErrorData> {
        self.declared_on_initialize
            .lock()
            .push(serde_json::to_value(&request.capabilities).unwrap_or_default());
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
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
        self.metas
            .lock()
            .push(serde_json::to_value(&context.meta).unwrap_or_default());
        // 0728 单次请求 `_meta`：需要提问的调用必须在线上声明 Form + URL。
        let capabilities = context.meta.client_capabilities();
        let has_elicitation = capabilities
            .as_ref()
            .and_then(|caps| caps.elicitation.as_ref())
            .is_some_and(|elicitation| elicitation.form.is_some() && elicitation.url.is_some());
        if !has_elicitation && !matches!(self.mode, Mode::Plain) {
            return Err(rmcp::ErrorData::internal_error(
                format!("missing elicitation meta; meta={:?}", context.meta),
                None,
            ));
        }
        match self.mode {
            Mode::Plain => Ok(fixed_result()),
            Mode::Slow => {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(fixed_result())
            }
            Mode::Parallel => {
                let tag = request
                    .arguments
                    .as_ref()
                    .and_then(|arguments| arguments.get("action"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let round = {
                    let mut rounds = self.parallel_rounds.lock();
                    let entry = rounds.entry(tag.clone()).or_insert(0);
                    *entry += 1;
                    *entry
                };
                if round == 1 {
                    return Ok(form_input_required(Some(&format!("state-{tag}"))));
                }
                let responses = request
                    .input_responses
                    .expect("parallel retry carries inputResponses");
                assert_eq!(
                    responses["approval"]["action"], "accept",
                    "parallel call {tag} must receive its own accept"
                );
                assert_eq!(
                    responses["approval"]["content"]["note"], tag,
                    "parallel call {tag} must receive its own answer"
                );
                Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                    ContentBlock::text(format!("result-{tag}")),
                ])))
            }
            Mode::TwoRounds if round == 1 => Ok(form_input_required(Some("opaque-1"))),
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
                Ok(fixed_result())
            }
            Mode::CancelRound if round == 1 => Ok(form_input_required(Some("cancel-1"))),
            Mode::CancelRound => {
                let responses = request
                    .input_responses
                    .expect("cancel round carries inputResponses");
                assert_eq!(
                    responses["approval"],
                    serde_json::json!({ "action": "cancel" })
                );
                assert_eq!(request.request_state.as_deref(), Some("cancel-1"));
                Ok(fixed_result())
            }
            Mode::UrlTwoRounds if round == 1 => Ok(url_input_required(Some("url-1"))),
            Mode::UrlTwoRounds => {
                let responses = request
                    .input_responses
                    .expect("url round 2 carries inputResponses");
                assert_eq!(
                    responses["authorize"],
                    serde_json::json!({ "action": "accept" })
                );
                assert_eq!(request.request_state.as_deref(), Some("url-1"));
                Ok(fixed_result())
            }
            Mode::UrlTwice if round < 3 => Ok(url_input_required(Some("url-repeat"))),
            Mode::UrlTwice => {
                let responses = request
                    .input_responses
                    .expect("url round 3 carries inputResponses");
                assert_eq!(
                    responses["authorize"],
                    serde_json::json!({ "action": "accept" })
                );
                Ok(fixed_result())
            }
            Mode::StateOmitted if round == 1 => Ok(form_input_required(None)),
            Mode::StateOmitted => {
                assert!(
                    request.request_state.is_none(),
                    "a round that omitted requestState must stay omitted"
                );
                assert!(request.input_responses.is_some());
                Ok(fixed_result())
            }
            Mode::AlwaysInputRequired => Ok(form_input_required(Some("opaque-repeat"))),
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
    Arc<Mutex<Vec<serde_json::Value>>>,
    Arc<Mutex<Vec<serde_json::Value>>>,
    crate::mcp::client::McpServiceWrapper,
    crate::mcp::builtin::runtime::BuiltinServerTask,
);

async fn fixture(mode: Mode) -> Fixture {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let metas = Arc::new(Mutex::new(Vec::new()));
    let parallel_rounds = Arc::new(Mutex::new(HashMap::new()));
    let declared_on_initialize = Arc::new(Mutex::new(Vec::new()));
    let transport = crate::mcp::builtin::runtime::spawn_builtin_transport_with_handler(
        "mrtr-fixture",
        RoundHandler {
            calls: Arc::clone(&calls),
            metas: Arc::clone(&metas),
            parallel_rounds: Arc::clone(&parallel_rounds),
            mode,
            declared_on_initialize: Arc::clone(&declared_on_initialize),
        },
    );
    let crate::mcp::builtin::runtime::BuiltinTransport {
        io, server_task, ..
    } = transport;
    let service = crate::mcp::client::serve_client_auto(
        io,
        &McpCapabilityProfile::disabled(),
        Duration::from_secs(5),
    )
    .await
    .expect("handshake timeout")
    .expect("handshake");
    let peer = service.peer().clone();
    (
        peer,
        calls,
        metas,
        declared_on_initialize,
        service,
        server_task,
    )
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

fn route(
    tool: &str,
    broker: Arc<dyn UserInteractionBroker>,
    cancellation: CancellationToken,
) -> InteractionRoute {
    InteractionRoute {
        server: "mrtr-fixture".to_string(),
        tool: tool.to_string(),
        owner: owner(),
        broker,
        cancellation,
    }
}

fn call_capabilities() -> ClientCapabilities {
    client_capabilities_for_call(&McpCapabilityProfile::disabled())
}

#[test]
fn initialize_capabilities_do_not_declare_elicitation() {
    let info = crate::mcp::client::mcpp_client_info_for_profile(&McpCapabilityProfile::disabled());
    let value = serde_json::to_value(&info).expect("initialize info serializes");
    assert!(
        value["capabilities"].get("elicitation").is_none(),
        "旧 initialize 包不得声明 EL: {value}"
    );
}

#[test]
fn call_capabilities_declare_form_and_url() {
    let value = serde_json::to_value(call_capabilities()).expect("capabilities serialize");
    assert_eq!(
        value["elicitation"],
        serde_json::json!({ "form": {}, "url": {} })
    );
}

#[tokio::test]
async fn two_rounds_echo_opaque_state_and_send_keyed_responses() {
    let (peer, calls, metas, _declared, service, server_task) = fixture(Mode::TwoRounds).await;
    let broker = Arc::new(AcceptBroker {
        content: serde_json::json!({ "approve": true, "note": "ok" }),
    });
    let route = route("multi_round", broker, CancellationToken::new());
    let capabilities = call_capabilities();

    let result = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        Some(&route),
        Some(&capabilities),
    )
    .await
    .expect("two MRTR rounds");
    let CallToolResponse::Complete(result) = result else {
        panic!("expected final CallToolResult");
    };
    assert_eq!(result.content.len(), 1);

    {
        let calls = calls.lock();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].input_responses, None);
        assert_eq!(calls[0].request_state, None);
        assert_eq!(calls[1].request_state.as_deref(), Some("opaque-1"));
        let metas = metas.lock();
        assert_eq!(
            metas[0]["io.modelcontextprotocol/clientCapabilities"]["elicitation"],
            serde_json::json!({ "form": {}, "url": {} }),
            "wire _meta must declare form + url when an interaction route exists"
        );
    }
    close(service, server_task).await;
}

#[tokio::test]
async fn plain_tool_without_an_interaction_route_still_works() {
    let (peer, calls, metas, _declared, service, server_task) = fixture(Mode::Plain).await;
    let result = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        None,
        None,
    )
    .await
    .expect("plain tool call without an answerer");
    assert!(matches!(result, CallToolResponse::Complete(_)));
    assert_eq!(calls.lock().len(), 1);
    {
        let metas = metas.lock();
        assert!(
            metas[0]["io.modelcontextprotocol/clientCapabilities"]
                .get("elicitation")
                .is_none(),
            "no answerer must not declare elicitation"
        );
    }
    close(service, server_task).await;
}

#[tokio::test]
async fn explicit_cancel_is_returned_as_cancel_and_the_call_finishes() {
    let (peer, calls, _metas, _declared, service, server_task) = fixture(Mode::CancelRound).await;
    let broker = RecordingBroker::new(vec![ElicitationDecision::Cancel]);
    let route = route("multi_round", broker, CancellationToken::new());
    let capabilities = call_capabilities();

    let result = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        Some(&route),
        Some(&capabilities),
    )
    .await
    .expect("cancel round still receives the final result");
    assert!(matches!(result, CallToolResponse::Complete(_)));
    assert_eq!(calls.lock().len(), 2);
    close(service, server_task).await;
}

#[tokio::test]
async fn url_accept_omits_content_and_returns_the_final_result_once() {
    let (peer, calls, _metas, _declared, service, server_task) = fixture(Mode::UrlTwoRounds).await;
    let broker = RecordingBroker::new(vec![ElicitationDecision::Accept {
        content: serde_json::json!({ "must": "be ignored" }),
    }]);
    let route = route(
        "multi_round",
        Arc::clone(&broker) as Arc<dyn UserInteractionBroker>,
        CancellationToken::new(),
    );
    let capabilities = call_capabilities();

    let result = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        Some(&route),
        Some(&capabilities),
    )
    .await
    .expect("URL MRTR round");
    let CallToolResponse::Complete(result) = result else {
        panic!("expected final CallToolResult");
    };
    assert_eq!(
        result.content[0].as_text().expect("text").text,
        "receipt-001"
    );
    {
        let seen = broker.seen.lock();
        assert_eq!(seen.len(), 1);
        let InteractionContext::Elicitation {
            request: peri_acp_types::interaction::ElicitationRequest::Url { url, .. },
        } = &seen[0]
        else {
            panic!("expected URL elicitation");
        };
        assert_eq!(url, "https://example.com/connect");
    }
    assert_eq!(calls.lock().len(), 2);
    close(service, server_task).await;
}

#[tokio::test]
async fn repeated_url_requirements_are_asked_again_instead_of_auto_continuing() {
    let (peer, calls, _metas, _declared, service, server_task) = fixture(Mode::UrlTwice).await;
    let broker = RecordingBroker::new(vec![
        ElicitationDecision::Accept {
            content: serde_json::Value::Null,
        },
        ElicitationDecision::Accept {
            content: serde_json::Value::Null,
        },
    ]);
    let route = route(
        "multi_round",
        Arc::clone(&broker) as Arc<dyn UserInteractionBroker>,
        CancellationToken::new(),
    );
    let capabilities = call_capabilities();

    let result = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        Some(&route),
        Some(&capabilities),
    )
    .await
    .expect("URL requirements across three rounds");
    assert!(matches!(result, CallToolResponse::Complete(_)));
    assert_eq!(calls.lock().len(), 3);
    assert_eq!(broker.seen.lock().len(), 2);
    close(service, server_task).await;
}

#[tokio::test]
async fn omitted_request_state_stays_omitted_on_the_next_round() {
    let (peer, calls, _metas, _declared, service, server_task) = fixture(Mode::StateOmitted).await;
    let broker = Arc::new(AcceptBroker {
        content: serde_json::json!({ "approve": true }),
    });
    let route = route("multi_round", broker, CancellationToken::new());
    let capabilities = call_capabilities();

    call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        Some(&route),
        Some(&capabilities),
    )
    .await
    .expect("state omission round");
    {
        let calls = calls.lock();
        assert_eq!(calls.len(), 2);
        assert!(calls[1].request_state.is_none());
    }
    close(service, server_task).await;
}

#[tokio::test]
async fn cancelling_the_tool_route_stops_continuation() {
    let (peer, calls, _metas, _declared, service, server_task) = fixture(Mode::TwoRounds).await;
    let broker = RecordingBroker::new(vec![ElicitationDecision::Accept {
        content: serde_json::json!({ "approve": true }),
    }]);
    let cancellation = CancellationToken::new();
    let route = route(
        "multi_round",
        Arc::clone(&broker) as Arc<dyn UserInteractionBroker>,
        cancellation.clone(),
    );
    let capabilities = call_capabilities();
    cancellation.cancel();

    let error = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        Some(&route),
        Some(&capabilities),
    )
    .await
    .expect_err("cancelled tool call must not continue");
    assert!(
        matches!(error, rmcp::ServiceError::Cancelled { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(
        calls.lock().len(),
        1,
        "cancellation must stop before the next tools/call"
    );
    close(service, server_task).await;
}

#[tokio::test]
async fn repeated_input_required_stops_at_round_cap() {
    let (peer, calls, _metas, _declared, service, server_task) =
        fixture(Mode::AlwaysInputRequired).await;
    let broker = Arc::new(AcceptBroker {
        content: serde_json::json!({ "approve": true }),
    });
    let route = route("multi_round", broker, CancellationToken::new());
    let capabilities = call_capabilities();

    let error = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        Some(&route),
        Some(&capabilities),
    )
    .await
    .expect_err("round cap must fail explicitly");
    assert!(matches!(
        error,
        rmcp::ServiceError::InputRequiredRoundsExceeded { .. }
    ));
    assert_eq!(calls.lock().len(), rmcp::model::DEFAULT_MRTR_MAX_ROUNDS + 1);
    close(service, server_task).await;
}

#[tokio::test]
async fn sampling_request_fails_explicitly_without_fabricated_answer() {
    let (peer, _calls, _metas, _declared, service, server_task) = fixture(Mode::Sampling).await;
    let broker = Arc::new(AcceptBroker {
        content: serde_json::Value::Null,
    });
    let route = route("multi_round", broker, CancellationToken::new());
    let capabilities = call_capabilities();

    let error = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        Some(&route),
        Some(&capabilities),
    )
    .await
    .expect_err("sampling must fail explicitly");
    assert!(
        error.to_string().contains("unsupported input request"),
        "unexpected error: {error}"
    );
    close(service, server_task).await;
}

#[tokio::test]
async fn input_required_without_route_fails_explicitly() {
    let (peer, _calls, _metas, _declared, service, server_task) = fixture(Mode::TwoRounds).await;
    let capabilities = call_capabilities();
    let error = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_secs(5)),
        None,
        Some(&capabilities),
    )
    .await
    .expect_err("no interaction route must fail");
    assert!(
        error.to_string().contains("no interaction route"),
        "unexpected error: {error}"
    );
    close(service, server_task).await;
}

#[tokio::test]
async fn parallel_calls_with_the_same_input_key_stay_isolated() {
    let (peer, _calls, _metas, _declared, service, server_task) = fixture(Mode::Parallel).await;
    let capabilities = call_capabilities();
    let alpha = RecordingBroker::new(vec![ElicitationDecision::Accept {
        content: serde_json::json!({ "approve": true, "note": "alpha" }),
    }]);
    let beta = RecordingBroker::new(vec![ElicitationDecision::Accept {
        content: serde_json::json!({ "approve": true, "note": "beta" }),
    }]);
    let alpha_route = route(
        "multi_round",
        Arc::clone(&alpha) as Arc<dyn UserInteractionBroker>,
        CancellationToken::new(),
    );
    let beta_route = route(
        "multi_round",
        Arc::clone(&beta) as Arc<dyn UserInteractionBroker>,
        CancellationToken::new(),
    );
    let alpha_call = async {
        call_tool(
            &peer,
            CallToolRequestParams::new("multi_round").with_arguments(serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("alpha")),
            ])),
            Some(Duration::from_secs(5)),
            Some(&alpha_route),
            Some(&capabilities),
        )
        .await
    };
    let beta_call = async {
        call_tool(
            &peer,
            CallToolRequestParams::new("multi_round").with_arguments(serde_json::Map::from_iter([
                ("action".to_string(), serde_json::json!("beta")),
            ])),
            Some(Duration::from_secs(5)),
            Some(&beta_route),
            Some(&capabilities),
        )
        .await
    };
    let (alpha_result, beta_result) = tokio::join!(alpha_call, beta_call);
    let alpha_result = alpha_result.expect("alpha call");
    let beta_result = beta_result.expect("beta call");
    let CallToolResponse::Complete(alpha_result) = alpha_result else {
        panic!("alpha must complete");
    };
    let CallToolResponse::Complete(beta_result) = beta_result else {
        panic!("beta must complete");
    };
    assert_eq!(
        alpha_result.content[0].as_text().expect("alpha text").text,
        "result-alpha"
    );
    assert_eq!(
        beta_result.content[0].as_text().expect("beta text").text,
        "result-beta"
    );
    close(service, server_task).await;
}

#[tokio::test]
async fn transport_timeout_is_reported_without_waiting_for_the_user() {
    let (peer, _calls, _metas, _declared, service, server_task) = fixture(Mode::Slow).await;
    let capabilities = call_capabilities();
    let error = call_tool(
        &peer,
        CallToolRequestParams::new("multi_round"),
        Some(Duration::from_millis(50)),
        None,
        Some(&capabilities),
    )
    .await
    .expect_err("slow tool must time out");
    assert!(
        matches!(error, rmcp::ServiceError::Timeout { .. }),
        "unexpected error: {error}"
    );
    close(service, server_task).await;
}

/// 旧握手（2025-11-25 initialize）不得声明 EL；普通工具照常成功。
#[tokio::test]
async fn legacy_initialize_has_no_elicitation_and_normal_tools_still_work() {
    struct LegacyHandler {
        declared: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl ServerHandler for LegacyHandler {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
        }

        async fn initialize(
            &self,
            request: InitializeRequestParams,
            context: RequestContext<RoleServer>,
        ) -> Result<InitializeResult, rmcp::ErrorData> {
            self.declared
                .lock()
                .push(serde_json::to_value(&request.capabilities).unwrap_or_default());
            context.peer.set_peer_info(request.clone());
            self.negotiate_initialize(&request)
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
            Ok(rmcp::model::ListToolsResult::with_all_items(vec![
                Tool::new("echo", "legacy fixture", Arc::new(serde_json::Map::new())),
            ]))
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, rmcp::ErrorData> {
            Ok(fixed_result())
        }
    }

    let declared = Arc::new(Mutex::new(Vec::new()));
    let transport = crate::mcp::builtin::runtime::spawn_builtin_transport_with_handler(
        "legacy-fixture",
        LegacyHandler {
            declared: Arc::clone(&declared),
        },
    );
    let crate::mcp::builtin::runtime::BuiltinTransport {
        io,
        mut server_task,
        ..
    } = transport;
    let info = crate::mcp::client::mcpp_client_info_for_profile(&McpCapabilityProfile::disabled())
        .with_protocol_version(ProtocolVersion::V_2025_11_25);
    let service =
        rmcp::service::serve_client(crate::mcp::client::PeriClientHandler::new(info, None), io)
            .await
            .expect("legacy handshake");
    let peer = service.peer().clone();

    let result = call_tool(
        &peer,
        CallToolRequestParams::new("echo"),
        Some(Duration::from_secs(5)),
        None,
        None,
    )
    .await
    .expect("normal tool call");
    assert!(matches!(result, CallToolResponse::Complete(_)));

    {
        let declared = declared.lock();
        assert_eq!(declared.len(), 1);
        assert!(
            declared[0].get("elicitation").is_none(),
            "legacy initialize must not declare elicitation: {:?}",
            declared[0]
        );
    }
    let mut service = service;
    let _ = service
        .close_with_timeout(crate::mcp::builtin::runtime::BUILTIN_CONVERGE_TIMEOUT)
        .await;
    let _ = server_task
        .converge(crate::mcp::builtin::runtime::BUILTIN_CONVERGE_TIMEOUT)
        .await;
}
