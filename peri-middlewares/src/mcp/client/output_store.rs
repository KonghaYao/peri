use std::{sync::Arc, time::Duration};

use peri_acp_types::workspace_output::{StoreOutputRequest, StoredOutput, STORE_OUTPUT_METHOD};
use rmcp::{
    model::{CancelledNotificationParam, ClientRequest, CustomRequest, RequestId, ServerResult},
    service::{Peer, PeerRequestOptions, RoleClient},
};

use super::{ClientStatus, McpClientPool};

const STORE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_LINES: usize = 2000;
/// 模型面可见文本的字节预算（M7）。与仓库既有工具输出的「2000 行 / 100 KB」
/// 双层约定一致（`mcp-packages/web/src/web_fetch.rs`）：行数上限挡不住单行巨量
/// 输出或多字节正文，字节预算兜住它们。**预算包含截断提示**（提示计入总长，
/// 正文按剩余额度截断），因此模型看到的文本有硬上界。
const MAX_BYTES: usize = 100_000;

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
                    let _ = peri_time::timeout(
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
        let deadline = peri_time::monotonic_now() + timeout;
        let request = peri_time::timeout_at(
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
        let result = peri_time::timeout_at(deadline, request.await_response())
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
    let exceeds_lines = lines.len() > MAX_LINES;
    let exceeds_bytes = formatted.len() > MAX_BYTES;
    if !exceeds_lines && !exceeds_bytes {
        return formatted;
    }
    let saved = match pool {
        Some(pool) => pool.store_output(session_id, &formatted).await,
        None => Err("workspace output store not configured"),
    };
    let hint = match saved {
        Ok(stored) => saved_output_hint(&stored),
        // 落存失败必须明说：不得给出任何看似可回查的地址。
        Err(reason) => format!("\nFull output NOT saved: {reason}. No host filesystem fallback."),
    };
    let label = if error {
        "MCP error output"
    } else {
        "MCP output"
    };
    // 触发原因（行数 / 字节数）必须在提示里说清，二者可同时命中。
    let reason = match (exceeds_lines, exceeds_bytes) {
        (true, true) => format!("{} total lines, {} bytes", lines.len(), formatted.len()),
        (true, false) => format!("{} total lines", lines.len()),
        (false, true) => format!("{} bytes", formatted.len()),
        (false, false) => unreachable!("至少一层预算已超限"),
    };
    let notice = format!("\n\n[{label} truncated: {reason}]{hint}");
    // 预算包含截断提示：正文额度 = 总预算 - 提示长度（按 UTF-8 边界截断，
    // 不产生半个字符）。
    let body_budget = MAX_BYTES.saturating_sub(notice.len());
    let head = lines[..lines.len().min(MAX_LINES)].join("\n");
    let body = if head.len() > body_budget {
        peri_agent::agent::async_tasks::truncate_bytes(&head, body_budget)
    } else {
        head
    };
    format!("{body}{notice}")
}

fn saved_output_hint(stored: &StoredOutput) -> String {
    let hint = format!(
        "\nFull output saved in tool environment: server=workspace, resource_uri={}, path={} ({} bytes). Read with mcp_read_resource(server_name=workspace, uri=resource_uri) or Read(file_path=<returned path>, offset=<start line>, limit=<line count>).",
        serde_json::json!(stored.uri),
        serde_json::json!(stored.path),
        stored.byte_length
    );
    if hint.len() <= 4096 {
        return hint;
    }
    format!(
        "\nFull output saved in tool environment: server=workspace, resource_uri={} ({} bytes). Path omitted: reference exceeds the byte budget. Read with mcp_read_resource(server_name=workspace, uri=resource_uri).",
        serde_json::json!(stored.uri),
        stored.byte_length
    )
}

#[cfg(test)]
#[path = "output_store_test.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "output_budget_test.rs"]
mod budget_tests;
