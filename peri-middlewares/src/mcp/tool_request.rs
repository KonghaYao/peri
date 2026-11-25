//! Own one MCP request until completion. Dropping an invocation sends cancellation
//! instead of silently leaving server-side work running.
use rmcp::model::CallToolRequest;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResponse;
use rmcp::model::CancelledNotificationParam;
use rmcp::model::ClientRequest;
use rmcp::model::RequestId;
use rmcp::model::ServerResult;
use rmcp::service::Peer;
use rmcp::service::PeerRequestOptions;
use rmcp::service::RoleClient;
use rmcp::ServiceError;
use std::time::Duration;

use super::client::ElicitationLease;

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
    elicitation: Option<&ElicitationLease>,
) -> Result<CallToolResponse, ServiceError> {
    // 每轮尝试独立 deadline：发送与响应各自有界，等待用户的时间不吞掉
    // 工具本身的传输预算。
    let send_deadline = timeout.map(|duration| peri_time::monotonic_now() + duration);
    let send = peer.send_request_with_option(
        ClientRequest::CallToolRequest(CallToolRequest::new(params)),
        PeerRequestOptions::no_options(),
    );
    let mut handle =
        match send_deadline {
            Some(deadline) => peri_time::timeout_at(deadline, send).await.map_err(|_| {
                ServiceError::Timeout {
                    timeout: timeout.expect("deadline has a duration"),
                }
            })??,
            None => send.await?,
        };
    let mut pending = PendingRequest {
        peer: peer.clone(),
        id: Some(handle.id.clone()),
    };

    // 用户在 elicitation 中思考时续期：deadline 到点仍在提问 → 重置；提问结束
    // （作答 / 取消）→ 立即给 server 一个新的完整窗口。
    let mut deadline = timeout.map(|duration| peri_time::monotonic_now() + duration);
    let mut activity_rx = elicitation.map(ElicitationLease::activity_receiver);
    let mut settled = None;
    let result = loop {
        let until = deadline;
        let inactivity = async {
            match &mut activity_rx {
                Some(rx) => {
                    let _ = rx.changed().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            result = &mut handle.rx => {
                let response = result.map_err(|_| ServiceError::TransportClosed)??;
                settled = Some(response);
            }
            _ = async {
                match until {
                    Some(until) => peri_time::sleep_until(until).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                let duration = timeout.expect("deadline has a duration");
                if elicitation.is_some_and(ElicitationLease::is_asking) {
                    deadline = Some(peri_time::monotonic_now() + duration);
                    continue;
                }
                pending.cancel().await;
                return Err(ServiceError::Timeout { timeout: duration });
            }
            _ = inactivity => {
                if let Some(duration) = timeout {
                    deadline = Some(peri_time::monotonic_now() + duration);
                }
            }
        }
        if let Some(response) = settled.take() {
            break response;
        }
    };
    pending.id.take();
    match result {
        ServerResult::CallToolResult(result) => Ok(CallToolResponse::Complete(result)),
        ServerResult::CreateTaskResult(result) => Ok(CallToolResponse::Task(result)),
        _ => Err(ServiceError::UnexpectedResponse),
    }
}
