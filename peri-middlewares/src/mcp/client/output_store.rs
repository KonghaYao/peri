use std::{sync::Arc, time::Duration};

use peri_acp_types::workspace_output::{StoreOutputRequest, StoredOutput, STORE_OUTPUT_METHOD};
use rmcp::{
    model::{CancelledNotificationParam, ClientRequest, CustomRequest, RequestId, ServerResult},
    service::{Peer, PeerRequestOptions, RoleClient},
};

use super::{ClientStatus, McpClientPool};

const STORE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_LINES: usize = 2000;

struct PendingOutput {
    peer: Peer<RoleClient>,
    id: Option<RequestId>,
}

impl Drop for PendingOutput {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let peer = self.peer.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(1),
                        peer.notify_cancelled(CancelledNotificationParam::new(
                            Some(id),
                            Some("output store cancelled".into()),
                        )),
                    )
                    .await;
                });
            }
        }
    }
}

fn workspace_available(pool: &McpClientPool) -> bool {
    pool.is_open()
        && !pool.builtin_instance_context().is_some_and(|context| {
            let source = pool
                .get_client("workspace")
                .and_then(|handle| handle.source.clone());
            crate::mcp::builtin::is_closed_source("workspace", source.as_ref(), &context.closed)
        })
}

impl McpClientPool {
    pub(crate) async fn store_output(
        &self,
        session_id: Option<&str>,
        content: &str,
    ) -> Result<StoredOutput, &'static str> {
        self.store_output_with_timeout(session_id, content, STORE_TIMEOUT)
            .await
    }

    async fn store_output_with_timeout(
        &self,
        session_id: Option<&str>,
        content: &str,
        timeout: Duration,
    ) -> Result<StoredOutput, &'static str> {
        if !workspace_available(self) {
            return Err("workspace output store unavailable or disabled");
        }
        let handle = self
            .get_client_visible_to("workspace", session_id)
            .filter(|handle| matches!(handle.status, ClientStatus::Connected))
            .filter(|handle| crate::mcp::builtin::is_workspace_source(handle.source.as_ref()))
            .ok_or("workspace output store unavailable")?;
        let peer = handle.peer.as_ref().ok_or("workspace disconnected")?;
        let generation = self.handle_generation(&handle);
        let owner = self.acp_owners.read().get("workspace").cloned();
        let params = serde_json::to_value(StoreOutputRequest {
            content: content.to_string(),
        })
        .map_err(|_| "invalid output store request")?;
        let deadline = tokio::time::Instant::now() + timeout;
        let request = tokio::time::timeout_at(
            deadline,
            peer.send_request_with_option(
                ClientRequest::CustomRequest(CustomRequest::new(STORE_OUTPUT_METHOD, Some(params))),
                PeerRequestOptions::no_options(),
            ),
        )
        .await
        .map_err(|_| "output store timed out")?
        .map_err(|_| "output store RPC failed")?;
        let mut pending = PendingOutput {
            peer: peer.clone(),
            id: Some(request.id.clone()),
        };
        let result = tokio::time::timeout_at(deadline, request.await_response())
            .await
            .map_err(|_| "output store timed out")?
            .map_err(|_| "output store RPC failed")?;
        pending.id.take();
        if !workspace_available(self)
            || self.acp_owners.read().get("workspace").cloned() != owner
            || !self
                .get_client_visible_to("workspace", session_id)
                .is_some_and(|current| {
                    Arc::ptr_eq(&current, &handle)
                        && matches!(current.status, ClientStatus::Connected)
                        && self.handle_generation(&current) == generation
                })
        {
            return Err("workspace output store changed during request");
        }
        let ServerResult::CustomResult(result) = result else {
            return Err("invalid output store response");
        };
        let stored: StoredOutput =
            serde_json::from_value(result.0).map_err(|_| "invalid output store response")?;
        let valid_uri = stored
            .uri
            .strip_prefix("peri-output://")
            .is_some_and(|suffix| {
                suffix.split_once('/').is_some_and(|(store, artifact)| {
                    uuid::Uuid::parse_str(store).is_ok() && uuid::Uuid::parse_str(artifact).is_ok()
                })
            });
        if !valid_uri || stored.path.is_empty() || stored.byte_length != content.len() as u64 {
            return Err("invalid output store response");
        }
        Ok(stored)
    }
}

pub(crate) async fn format_output(
    pool: Option<&McpClientPool>,
    session_id: Option<&str>,
    formatted: String,
    error: bool,
) -> String {
    let lines: Vec<&str> = formatted.lines().collect();
    if lines.len() <= MAX_LINES {
        return formatted;
    }
    let saved = match pool {
        Some(pool) => pool.store_output(session_id, &formatted).await,
        None => Err("workspace output store not configured"),
    };
    let hint = match saved {
        Ok(stored) => format!(
            "\nFull output saved in tool environment: server=workspace, resource_uri={}, path={} ({} bytes). Read with mcp_read_resource(server_name=workspace, uri=resource_uri) or Read(file_path=<returned path>, offset=<start line>, limit=<line count>).",
            serde_json::json!(stored.uri),
            serde_json::json!(stored.path),
            stored.byte_length
        ),
        Err(reason) => format!("\nFull output NOT saved: {reason}. No host filesystem fallback."),
    };
    let label = if error {
        "MCP error output"
    } else {
        "MCP output"
    };
    format!(
        "{}\n\n[{label} truncated: {} total lines]{hint}",
        lines[..MAX_LINES].join("\n"),
        lines.len()
    )
}

#[cfg(test)]
#[path = "output_store_test.rs"]
pub(crate) mod tests;
