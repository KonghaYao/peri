//! Own one MCP request until completion. Dropping an invocation sends cancellation
//! instead of silently leaving server-side work running.
use rmcp::{
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResponse, CancelledNotificationParam,
        ClientRequest, RequestId, ServerResult,
    },
    service::{Peer, PeerRequestOptions, RoleClient},
    ServiceError,
};
use std::time::Duration;

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
    let _ = tokio::time::timeout(
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
) -> Result<CallToolResponse, ServiceError> {
    // One deadline covers transport backpressure and response latency together.
    let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);
    let send = peer.send_request_with_option(
        ClientRequest::CallToolRequest(CallToolRequest::new(params)),
        PeerRequestOptions::no_options(),
    );
    let handle = match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, send)
            .await
            .map_err(|_| ServiceError::Timeout {
                timeout: timeout.expect("deadline has a duration"),
            })??,
        None => send.await?,
    };
    let mut pending = PendingRequest {
        peer: peer.clone(),
        id: Some(handle.id.clone()),
    };
    let response = handle.await_response();
    let result = match timeout {
        Some(duration) => {
            match tokio::time::timeout_at(deadline.expect("bounded request"), response).await {
                Ok(result) => result,
                Err(_) => {
                    pending.cancel().await;
                    return Err(ServiceError::Timeout { timeout: duration });
                }
            }
        }
        None => response.await,
    };
    pending.id.take();
    match result? {
        ServerResult::CallToolResult(result) => Ok(CallToolResponse::Complete(result)),
        ServerResult::CreateTaskResult(result) => Ok(CallToolResponse::Task(result)),
        _ => Err(ServiceError::UnexpectedResponse),
    }
}
