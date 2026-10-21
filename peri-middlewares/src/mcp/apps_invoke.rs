//! Host-initiated MCP Apps invocation admission.
//!
//! Host 调用与 App 回调走**同一个** session-owned canonical 入口
//!（[`SessionInvocationPort`]）：本模块只做请求级前置校验（信封 / server / 工具
//! 可见性 / 连接在线）与结果投影，准入与执行交给 service。
//!
//! 一切准入失败都由 service 以 typed `NotExecuted` 事实报告，并在 wire 调用之前
//! 停下；canonical 入口在 relay 构造期注入（设计 §4.1 与裁决「终点状态必须功能可用」）。

use peri_acp_types::mcp_apps::{
    InvocationTarget, McpAppInvokeOutcome, McpAppInvokeRequest, McpAppsErrorKind,
    McpAppsRelayError, SessionInvocationOutcome, SessionInvocationRequest,
};
use peri_acp_types::tool_invocation::{InvocationId, InvocationOrigin};
use tokio_util::sync::CancellationToken;

use super::apps::{tool_resource_uri, tool_visibility};
use super::apps_relay::{relay_error, PoolMcpAppsRelay};

impl PoolMcpAppsRelay {
    pub(super) async fn invoke_app_inner(
        &self,
        request: &McpAppInvokeRequest,
        attribution: peri_acp_types::mcp_apps::HostInvocationRef,
        cancellation: CancellationToken,
    ) -> Result<McpAppInvokeOutcome, McpAppsRelayError> {
        if cancellation.is_cancelled() {
            return Err(relay_error(McpAppsErrorKind::Cancelled));
        }
        if request.owner_session_id.trim().is_empty() {
            return Err(relay_error(McpAppsErrorKind::InvalidSession));
        }
        if request.server_id.trim().is_empty() {
            return Err(relay_error(McpAppsErrorKind::UnknownServer));
        }
        if request.tool_name.trim().is_empty() {
            return Err(relay_error(McpAppsErrorKind::ToolNotFound));
        }
        let handle = self
            .pool
            .get_client(&request.server_id)
            .ok_or_else(|| relay_error(McpAppsErrorKind::UnknownServer))?;
        let _permit = handle
            .call_gate
            .try_acquire()
            .map_err(|_| relay_error(McpAppsErrorKind::ServerDisconnected))?;
        let tool = handle
            .tools
            .iter()
            .find(|tool| tool.name.as_ref() == request.tool_name)
            .ok_or_else(|| relay_error(McpAppsErrorKind::ToolNotFound))?;
        if !tool_visibility(tool).app {
            return Err(relay_error(McpAppsErrorKind::ToolNotAppVisible));
        }
        if tool_resource_uri(tool).is_none() {
            return Err(relay_error(McpAppsErrorKind::InvalidResource));
        }
        if handle.peer.is_none() {
            return Err(relay_error(McpAppsErrorKind::ServerDisconnected));
        }
        let protocol = handle
            .peer
            .as_ref()
            .and_then(|peer| peer.peer_info())
            .map(|info| info.protocol_version.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // canonical 入口在装配期注入（`PoolMcpAppsRelay::new` 必填）：Host invoke
        // 与 App 回调走同一条准入管线，拒绝时由 service 以 typed `NotExecuted`
        // 事实报告（零 wire 调用），不返回「恒 PolicyDenied」的占位结论。
        let (runtime_epoch, port) = self.invocation();
        // Host 面回执需要稳定的 `toolCallId`：调用方提供身份，service 只把它当作
        // 回执关联键（不参与任何安全裁决）。
        let invocation_id = InvocationId::generate();
        let outcome = port
            .invoke(SessionInvocationRequest {
                session_id: request.owner_session_id.clone(),
                runtime_epoch: *runtime_epoch,
                origin: InvocationOrigin::Host {
                    // 归属值由**调用方**给出（`SessionAppsFacade` 的 Host 操作域 +
                    // 请求账本推进的序号）：本层不自持计数器（设计 §1.2 的取代点；
                    // 旧的 `host_sequence` 局部计数器已删除）。
                    namespace: attribution.namespace,
                    seq: attribution.seq,
                },
                target: InvocationTarget::Host {
                    server_id: request.server_id.clone(),
                    tool_name: request.tool_name.clone(),
                    arguments: request.arguments.clone(),
                },
                cancel: cancellation,
                invocation_id: Some(invocation_id.clone()),
            })
            .await?;
        match outcome {
            SessionInvocationOutcome::Completed(raw) => Ok(McpAppInvokeOutcome {
                mcp_protocol_version: protocol,
                tool_call_id: invocation_id.to_string(),
                effective_tool_name: super::tool_bridge::effective_mcp_tool_name(
                    &request.server_id,
                    &request.tool_name,
                ),
                arguments: request.arguments.clone(),
                // raw 事实的 JSON 文本投影（宿主面）：与模型面同一事实，不二次解释。
                output: raw.into_value().to_string(),
            }),
            other => Err(relay_error(other.error_kind())),
        }
    }
}

#[cfg(test)]
#[path = "apps_invoke_test.rs"]
mod tests;
