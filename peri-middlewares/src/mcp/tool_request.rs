//! Own one MCP request until completion. Dropping an invocation sends cancellation
//! instead of silently leaving server-side work running.
use rmcp::model::CallToolRequest;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResponse;
use rmcp::model::CancelledNotificationParam;
use rmcp::model::ClientCapabilities;
use rmcp::model::ClientRequest;
use rmcp::model::ErrorData as McpError;
use rmcp::model::InputRequest;
use rmcp::model::InputResponses;
use rmcp::model::ProtocolVersion;
use rmcp::model::RequestId;
use rmcp::model::RequestMetaObject;
use rmcp::model::ServerResult;
use rmcp::model::DEFAULT_MRTR_MAX_ROUNDS;
use rmcp::service::Peer;
use rmcp::service::PeerRequestOptions;
use rmcp::service::RoleClient;
use rmcp::ServiceError;
use std::time::Duration;

use super::client::InteractionRoute;

const CANCEL_SEND_TIMEOUT: Duration = Duration::from_secs(1);

struct PendingRequest {
    peer: Peer<RoleClient>,
    id: Option<RequestId>,
}

impl PendingRequest {
    async fn cancel(&mut self) {
        if let Some(id) = self.id.take() {
            send_cancel(self.peer.clone(), id).await;
        }
    }
}

async fn send_cancel(peer: Peer<RoleClient>, id: RequestId) {
    let _ = peri_time::timeout(
        CANCEL_SEND_TIMEOUT,
        peer.notify_cancelled(CancelledNotificationParam::new(
            Some(id),
            Some("tool invocation cancelled".into()),
        )),
    )
    .await;
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            // Rust cannot await in Drop. This bounded transport notification owns no
            // execution; server-side resources remain owned by the request/session.
            // If runtime shutdown already occurred, pool close owns server cleanup.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(send_cancel(self.peer.clone(), id));
            }
        }
    }
}

pub(super) async fn call_tool(
    peer: &Peer<RoleClient>,
    params: CallToolRequestParams,
    timeout: Option<Duration>,
    interaction: Option<&InteractionRoute>,
    request_capabilities: Option<&ClientCapabilities>,
) -> Result<CallToolResponse, ServiceError> {
    // 每轮尝试独立 deadline；跨轮只保留 inputResponses 与 server 的 requestState。
    let mut input_responses: Option<InputResponses> = None;
    let mut request_state: Option<String> = None;

    for round in 0..=DEFAULT_MRTR_MAX_ROUNDS {
        let mut attempt = params.clone();
        attempt.input_responses = input_responses.clone();
        attempt.request_state = request_state.clone();
        let result = call_tool_once(
            peer,
            attempt,
            request_options(peer, request_capabilities),
            timeout,
        )
        .await?;
        match result {
            ServerResult::InputRequiredResult(required) => {
                if round == DEFAULT_MRTR_MAX_ROUNDS {
                    return Err(ServiceError::InputRequiredRoundsExceeded {
                        max_rounds: DEFAULT_MRTR_MAX_ROUNDS,
                    });
                }
                let requests = required.input_requests.unwrap_or_default();
                let Some(interaction) = interaction else {
                    // 没有交互入口时不能伪造 inputResponses：显式失败，由工具层转述。
                    return Err(ServiceError::McpError(McpError::invalid_params(
                        "server requires elicitation input but this call has no interaction route",
                        None,
                    )));
                };
                let mut responses = input_responses.take().unwrap_or_default();
                for (key, request) in requests {
                    match request {
                        InputRequest::Elicitation(request) => {
                            let mapped =
                                interaction.ask(&request.params).await.map_err(|failure| {
                                    ServiceError::McpError(McpError::internal_error(
                                        "MCP elicitation channel failed",
                                        Some(serde_json::json!({ "detail": failure.detail })),
                                    ))
                                })?;
                            // 工具调用整体取消：立即停止续调，不再发送下一轮 tools/call。
                            if interaction.cancellation.is_cancelled() {
                                return Err(ServiceError::Cancelled {
                                    reason: Some("tool invocation cancelled".to_string()),
                                });
                            }
                            let value =
                                serde_json::to_value(mapped.into_result()).map_err(|error| {
                                    ServiceError::McpError(McpError::internal_error(
                                        "failed to serialize elicitation result",
                                        Some(serde_json::json!({ "detail": error.to_string() })),
                                    ))
                                })?;
                            responses.insert(key, value);
                        }
                        // sampling / roots 不在本 PR 的交互能力内：显式失败，不伪造答案。
                        _ => {
                            return Err(ServiceError::McpError(McpError::invalid_params(
                                "unsupported input request: sampling/roots",
                                None,
                            )));
                        }
                    }
                }
                input_responses = Some(responses);
                // requestState 逐字回传；该轮省略时下一轮也随之省略。
                request_state = required.request_state;
            }
            ServerResult::CallToolResult(result) => {
                return Ok(CallToolResponse::Complete(result));
            }
            ServerResult::CreateTaskResult(result) => {
                return Ok(CallToolResponse::Task(result));
            }
            _ => return Err(ServiceError::UnexpectedResponse),
        }
    }
    Err(ServiceError::InputRequiredRoundsExceeded {
        max_rounds: DEFAULT_MRTR_MAX_ROUNDS,
    })
}

/// 2026-07-28 的单次请求能力覆盖：只有本次调用确有交互入口时才携带 EL。
///
/// 更早协商版本不写 `_meta`，旧 initialize 包也不声明 EL。
fn request_options(
    peer: &Peer<RoleClient>,
    capabilities: Option<&ClientCapabilities>,
) -> PeerRequestOptions {
    let Some(capabilities) = capabilities else {
        return PeerRequestOptions::no_options();
    };
    let negotiated = peer.peer_info().map(|info| info.protocol_version.clone());
    if negotiated.is_none_or(|version| version < ProtocolVersion::V_2026_07_28) {
        return PeerRequestOptions::no_options();
    }
    let mut meta = RequestMetaObject::new();
    meta.set_client_capabilities(capabilities.clone());
    PeerRequestOptions::no_options().with_meta(meta)
}

/// 单轮 `tools/call`：发送 + 响应等待。传输超时只覆盖本轮发送与响应，
/// 用户作答发生在 `InputRequiredResult` 之后，由交互 broker 自己的超时约束。
async fn call_tool_once(
    peer: &Peer<RoleClient>,
    params: CallToolRequestParams,
    options: PeerRequestOptions,
    timeout: Option<Duration>,
) -> Result<ServerResult, ServiceError> {
    let send = peer.send_request_with_option(
        ClientRequest::CallToolRequest(CallToolRequest::new(params)),
        options,
    );
    let mut handle = match timeout {
        Some(duration) => peri_time::timeout(duration, send)
            .await
            .map_err(|_| ServiceError::Timeout { timeout: duration })??,
        None => send.await?,
    };
    let mut pending = PendingRequest {
        peer: peer.clone(),
        id: Some(handle.id.clone()),
    };

    let result = match timeout {
        Some(duration) => match peri_time::timeout(duration, &mut handle.rx).await {
            Ok(response) => response.map_err(|_| ServiceError::TransportClosed)??,
            Err(_) => {
                pending.cancel().await;
                return Err(ServiceError::Timeout { timeout: duration });
            }
        },
        None => handle
            .rx
            .await
            .map_err(|_| ServiceError::TransportClosed)??,
    };
    pending.id.take();
    Ok(result)
}

#[cfg(test)]
#[path = "tool_request_test.rs"]
mod tests;
