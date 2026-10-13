use std::sync::Arc;

use peri_acp_types::interaction::AttachmentId;
use peri_acp_types::mcp_apps::{
    JsonRpcErrorResponse, JsonRpcResponse, JsonRpcResultResponse, McpAppInvokeRequest,
    McpAppInvokeResponse, McpAppOpenRequest, McpAppOpenResponse, McpAppRequest, McpAppResponse,
    McpAppsErrorKind, McpAppsRelayError, McpAppsRelayPort, McpResourceRequest, McpResourceResponse,
    SessionInvocationOutcome, MCP_APPS_ENVELOPE_VERSION, MCP_APPS_PROTOCOL_VERSION,
};
use peri_acp_types::mcp_apps_facade::{
    AppsAttachRequest, AppsOperation, AppsRequestOutcome, ClientViewId,
};
use serde_json::Value;

use super::apps_facade::SessionAppsFacade;
use super::connection::ConnectionContext;
use crate::transport::types::AcpError;

/// MCP Apps 的 ACP 分发：七个 `peri/mcp/apps/*`（唯一实现 = [`SessionAppsFacade`]）
/// 与旧 4 名**薄别名**。
///
/// 别名纪律（裁决 D1）：别名只做**字段映射**与 wire 形态回投影；准入、判决、执行
/// 全部走同一 facade / ledger / canonical 入口。别名不复制业务规则，也不持有状态。
/// 旧信封缺的权威值（附件身份、纪元、序号、目标身份）由**服务端事实**补齐，客户端
/// 自述字段一律不参与目标选择（D4）。删除条件：`side-projects/mcp-apps/check-peri.ts`
/// 等客户端切到 `peri/mcp/apps/*` 后随旧名一并删除（S8）。
pub(crate) async fn handle_request(
    method: &str,
    params: &Value,
    connection: &mut ConnectionContext,
    facade: Option<&Arc<SessionAppsFacade>>,
) -> Result<Value, AcpError> {
    if !connection.apps_enabled() {
        return Err(outer_error(McpAppsErrorKind::CapabilityDisabled));
    }
    let facade = facade.ok_or_else(|| outer_error(McpAppsErrorKind::CapabilityDisabled))?;
    if method.starts_with("peri/mcp/apps/") {
        let connection_id = connection.id().to_string();
        return handle_apps_method(method, params, &connection_id, facade).await;
    }
    match method {
        "peri/mcp/open" => handle_open(params, connection, Some(facade)).await,
        "peri/mcp/resource" => handle_resource(params, connection, Some(facade)).await,
        "peri/mcp/app" => handle_app(params, connection, Some(facade)).await,
        _ => Err(outer_error(McpAppsErrorKind::UnsupportedMethod)),
    }
}

/// 七个新方法的分发（入参解码失败 = 信封不合法 ⇒ `AcpError`，不由别名兜底）。
async fn handle_apps_method(
    method: &str,
    params: &Value,
    connection_id: &str,
    facade: &Arc<SessionAppsFacade>,
) -> Result<Value, AcpError> {
    match method {
        "peri/mcp/apps/list" => {
            let response = facade.list(connection_id, decode(params)?).await?;
            serialize(response)
        }
        "peri/mcp/apps/attach" => {
            let response = facade.attach(connection_id, decode(params)?).await?;
            serialize(response)
        }
        "peri/mcp/apps/request" => {
            let response = facade.request(connection_id, decode(params)?).await?;
            serialize(response)
        }
        "peri/mcp/apps/cancel" => {
            let response = facade.cancel(connection_id, decode(params)?).await?;
            serialize(response)
        }
        "peri/mcp/apps/detach" => {
            let response = facade.detach(connection_id, decode(params)?).await?;
            serialize(response)
        }
        "peri/mcp/apps/close" => {
            let response = facade.close(connection_id, decode(params)?).await?;
            serialize(response)
        }
        "peri/mcp/apps/rerun" => {
            let response = facade.rerun(connection_id, decode(params)?).await?;
            serialize(response)
        }
        _ => Err(outer_error(McpAppsErrorKind::UnsupportedMethod)),
    }
}

pub(crate) struct InvokeSessionGate {
    pub known: bool,
    /// 会话不在关闭中。
    ///
    /// invoke 会真的执行工具并向该会话下发 `session/update`。
    pub open: bool,
    pub prompt_in_flight: bool,
}

/// Host invoke 的响应投影：**只有**原请求的返回值。
///
/// 裁决 R3：Host / App 回调不得冒充模型工具调用——不产生 `session/update`
/// 合成模型卡片（宿主需要可见记录时由阶段 4 的 Host 记录承载）。
#[derive(Debug)]
pub(crate) struct InvokeHostResult {
    pub value: Value,
}

pub(crate) async fn handle_invoke(
    params: &Value,
    connection: &ConnectionContext,
    relay: Option<&Arc<dyn McpAppsRelayPort>>,
    facade: Option<&Arc<SessionAppsFacade>>,
    gate: InvokeSessionGate,
) -> Result<InvokeHostResult, AcpError> {
    if !connection.apps_enabled() {
        return Err(outer_error(McpAppsErrorKind::CapabilityDisabled));
    }
    let relay = relay.ok_or_else(|| outer_error(McpAppsErrorKind::CapabilityDisabled))?;
    let facade = facade.ok_or_else(|| outer_error(McpAppsErrorKind::CapabilityDisabled))?;
    // 归属值（Host namespace + 序号）由 facade 的 Host 操作域与请求账本给出：
    // 本层不自持计数器（§A 运行中裁决第 5 条；旧 `host_sequence` 已删除）。
    let attribution = facade.host_attribution(connection.id())?;
    let request: McpAppInvokeRequest = decode(params)?;
    validate_versions(&request.envelope_version, &request.apps_protocol_version)?;
    if !gate.known {
        return Err(outer_error(McpAppsErrorKind::InvalidSession));
    }
    if !gate.open {
        return Err(outer_error(McpAppsErrorKind::PolicyDenied));
    }
    if gate.prompt_in_flight {
        return Err(outer_error(McpAppsErrorKind::PolicyDenied));
    }
    let cancellation = connection.cancellation();
    let outcome = tokio::select! {
        _ = cancellation.cancelled() => {
            return Err(outer_error(McpAppsErrorKind::Cancelled));
        }
        result = relay.invoke_app(&request, attribution, cancellation.child_token()) => {
            result.map_err(map_relay_error)?
        }
    };
    if outcome.tool_call_id.trim().is_empty() {
        return Err(outer_error(McpAppsErrorKind::UpstreamProtocolError));
    }
    let response = McpAppInvokeResponse {
        envelope_version: MCP_APPS_ENVELOPE_VERSION.into(),
        apps_protocol_version: MCP_APPS_PROTOCOL_VERSION.into(),
        mcp_protocol_version: outcome.mcp_protocol_version,
        server_id: request.server_id,
        tool_call_id: outcome.tool_call_id,
    };
    Ok(InvokeHostResult {
        value: serialize(response)?,
    })
}

/// 旧 `peri/mcp/open` → `peri/mcp/apps/attach` 的薄别名（§A 运行中裁决第 2/4 条）。
///
/// 字段映射：`invocationToken` → record（`model.invocation_id` 同源匹配，经
/// `list_apps`，**不新增索引状态**）；响应的 `appSessionId` = router 分配的
/// `attachment_id`（旧客户端视为不透明句柄），`resourceUri` / `serverId` /
/// `mcpProtocolVersion` 全部取自 record 投影。附件身份、namespace、纪元、账本
/// 全走 `apps/attach` 的同一路径；连接上下文不再持有任何 App 会话权威（旧 lease
/// 表已随 S8 删除），别名也没有第二份状态。
async fn handle_open(
    params: &Value,
    connection: &mut ConnectionContext,
    facade: Option<&Arc<SessionAppsFacade>>,
) -> Result<Value, AcpError> {
    let request: McpAppOpenRequest = decode(params)?;
    validate_versions(&request.envelope_version, &request.apps_protocol_version)?;
    let facade = facade.ok_or_else(|| outer_error(McpAppsErrorKind::CapabilityDisabled))?;
    let record = facade
        .record_for_invocation_token(
            &request.owner_session_id,
            &request.invocation_token,
            &request.server_id,
            &request.tool_name,
        )
        .await
        .map_err(map_relay_error)?;
    let attached = facade
        .attach(
            connection.id(),
            AppsAttachRequest {
                session_id: request.owner_session_id.clone(),
                app_id: record.descriptor.app_id.clone(),
                client_view_id: ClientViewId::new(connection.id()),
                reattach: None,
            },
        )
        .await?;
    serialize(McpAppOpenResponse {
        envelope_version: MCP_APPS_ENVELOPE_VERSION.into(),
        apps_protocol_version: MCP_APPS_PROTOCOL_VERSION.into(),
        mcp_protocol_version: record.mcp_protocol_version,
        server_id: record.descriptor.server_id,
        // §A-2：`appSessionId` 的语义 = 附件身份（router 分配，旧客户端只当句柄用）。
        app_session_id: attached.attachment.attachment_id.as_str().to_string(),
        resource_uri: record.descriptor.resource_uri,
    })
}

/// 旧 `peri/mcp/resource` → `peri/mcp/apps/request`（`resources/read`）的薄别名。
///
/// 旧信封没有 epoch / seq：两者由服务端权威值补齐（附件身份 = `appSessionId`，
/// 纪元取 router 的当前值，序号取 ledger 的下一个），执行与判决与新方法同一路径。
/// 客户端 `serverId` / `resourceUri` 不参与目标选择（record 是唯一权威）；响应里
/// 的中转值取自 record 投影（不由客户端自述回显）。
async fn handle_resource(
    params: &Value,
    connection: &ConnectionContext,
    facade: Option<&Arc<SessionAppsFacade>>,
) -> Result<Value, AcpError> {
    let request: McpResourceRequest = decode(params)?;
    validate_versions(&request.envelope_version, &request.apps_protocol_version)?;
    let facade = facade.ok_or_else(|| outer_error(McpAppsErrorKind::CapabilityDisabled))?;
    let attachment_id = AttachmentId::new(request.app_session_id.clone());
    let app = facade
        .alias_record_for_attachment(&attachment_id)
        .await
        .map_err(map_relay_error)?;
    let response = facade
        .alias_request(
            connection.id(),
            &attachment_id,
            AppsOperation::ResourcesRead {
                uri: request.resource_uri.clone(),
            },
        )
        .await?;
    let resources = match response.outcome {
        Some(AppsRequestOutcome::ResourcesRead { resources }) => resources,
        Some(AppsRequestOutcome::Pending) => {
            return Err(outer_error(McpAppsErrorKind::Cancelled));
        }
        Some(AppsRequestOutcome::ToolsCall(outcome)) => {
            return Err(outer_error(outcome.error_kind()));
        }
        Some(AppsRequestOutcome::ResourcesList { .. }) | None => {
            return Err(outer_error(McpAppsErrorKind::UpstreamProtocolError));
        }
    };
    if !resources
        .iter()
        .any(|resource| resource.is_valid_app_resource(&request.resource_uri))
    {
        // 旧 wire 的错误形态在此保留（读取确实发生了，但内容不是可用的 UI 文档）。
        return Err(outer_error(McpAppsErrorKind::InvalidResource));
    }
    serialize(McpResourceResponse {
        envelope_version: MCP_APPS_ENVELOPE_VERSION.into(),
        apps_protocol_version: MCP_APPS_PROTOCOL_VERSION.into(),
        mcp_protocol_version: app.mcp_protocol_version,
        server_id: app.descriptor.server_id,
        resources,
    })
}

/// 旧 `peri/mcp/app` → `peri/mcp/apps/request`（`tools/call`）的薄别名。
///
/// JSON-RPC id / method 留在旧 wire 形态里（契约 §5：App JSON-RPC ID 归 AppBridge，
/// 新方法不内嵌 JSON-RPC）；业务身份改由 `{namespace, seq}` 承载。
async fn handle_app(
    params: &Value,
    connection: &ConnectionContext,
    facade: Option<&Arc<SessionAppsFacade>>,
) -> Result<Value, AcpError> {
    let request: McpAppRequest = decode(params)?;
    validate_versions(&request.envelope_version, &request.apps_protocol_version)?;
    if request.payload.method != "tools/call" {
        return Err(outer_error(McpAppsErrorKind::UnsupportedMethod));
    }
    let facade = facade.ok_or_else(|| outer_error(McpAppsErrorKind::CapabilityDisabled))?;
    let attachment_id = AttachmentId::new(request.app_session_id.clone());
    let app = facade
        .alias_record_for_attachment(&attachment_id)
        .await
        .map_err(map_relay_error)?;
    let response = facade
        .alias_request(
            connection.id(),
            &attachment_id,
            AppsOperation::ToolsCall {
                name: request.payload.params.name.clone(),
                arguments: request.payload.params.arguments.clone(),
            },
        )
        .await?;
    let outcome = response
        .outcome
        .ok_or_else(|| outer_error(McpAppsErrorKind::UpstreamProtocolError))?;
    let payload = match outcome {
        AppsRequestOutcome::ToolsCall(SessionInvocationOutcome::Completed(raw)) => {
            JsonRpcResponse::Result(JsonRpcResultResponse {
                jsonrpc: request.payload.jsonrpc,
                id: request.payload.id,
                result: raw.into_value(),
            })
        }
        AppsRequestOutcome::ToolsCall(SessionInvocationOutcome::RemoteProtocolError(error)) => {
            JsonRpcResponse::Error(JsonRpcErrorResponse {
                jsonrpc: request.payload.jsonrpc,
                id: request.payload.id.clone(),
                error,
            })
        }
        AppsRequestOutcome::ToolsCall(other) => return Err(outer_error(other.error_kind())),
        AppsRequestOutcome::Pending => return Err(outer_error(McpAppsErrorKind::Cancelled)),
        _ => return Err(outer_error(McpAppsErrorKind::UpstreamProtocolError)),
    };
    serialize(McpAppResponse {
        envelope_version: MCP_APPS_ENVELOPE_VERSION.into(),
        apps_protocol_version: MCP_APPS_PROTOCOL_VERSION.into(),
        mcp_protocol_version: app.mcp_protocol_version,
        server_id: app.descriptor.server_id,
        // 旧 wire 的 JSON-RPC id 往返：`payload.id` 必须等于请求 id（§A-1）。
        app_session_id: request.app_session_id,
        resource_uri: app.descriptor.resource_uri,
        payload,
    })
}

fn validate_versions(envelope: &str, apps: &str) -> Result<(), AcpError> {
    if envelope != MCP_APPS_ENVELOPE_VERSION {
        return Err(outer_error(McpAppsErrorKind::UnsupportedEnvelopeVersion));
    }
    if apps != MCP_APPS_PROTOCOL_VERSION {
        return Err(outer_error(McpAppsErrorKind::UnsupportedAppsVersion));
    }
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, AcpError> {
    serde_json::from_value(value.clone())
        .map_err(|_| outer_error(McpAppsErrorKind::InvalidEnvelope))
}

fn serialize<T: serde::Serialize>(value: T) -> Result<Value, AcpError> {
    serde_json::to_value(value).map_err(|_| AcpError::new(-32603, "MCP Apps relay request failed"))
}

fn map_relay_error(error: McpAppsRelayError) -> AcpError {
    outer_error(error.kind)
}

fn outer_error(kind: McpAppsErrorKind) -> AcpError {
    let kind =
        serde_json::to_value(kind).unwrap_or(Value::String("upstream_protocol_error".into()));
    AcpError::new(-32000, "MCP Apps relay request failed")
        .with_data(serde_json::json!({"kind": kind}))
}

#[cfg(test)]
#[path = "mcp_apps_test.rs"]
mod tests;
