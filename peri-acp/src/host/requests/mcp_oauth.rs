//! MCP OAuth 授权交互命令 handler（专用 peri.oauth + legacy TUI 兼容）：
//! mcp/list / oauth_start / oauth_callback / oauth_cancel（自 requests.rs
//! 拆出，请求分发见 `host/requests.rs`）。

use serde_json::Value;

use super::super::AcpServerConfig;
use crate::transport::types::AcpError;

fn dynamic_oauth_identity(
    params: &Value,
) -> Result<Option<(peri_acp_types::dynamic_mcp::DynamicMcpInstanceKey, String)>, AcpError> {
    let fields = [
        params.get("session_id").and_then(Value::as_str),
        params.get("server_name").and_then(Value::as_str),
        params.get("incarnation_id").and_then(Value::as_str),
        params.get("flow_id").and_then(Value::as_str),
    ];
    if fields[0].is_none() && fields[2].is_none() && fields[3].is_none() {
        return Ok(None);
    }
    let [Some(session_id), Some(server_name), Some(incarnation_id), Some(flow_id)] = fields else {
        return Err(AcpError::new(-32602, "incomplete Dynamic MCP identity"));
    };
    crate::event::oauth::validate_identifier(session_id)
        .and_then(|_| crate::event::oauth::validate_server_name(server_name))
        .and_then(|_| crate::event::oauth::validate_identifier(incarnation_id))
        .and_then(|_| crate::event::oauth::validate_identifier(flow_id))
        .map_err(|error| AcpError::new(-32602, error.to_string()))?;
    Ok(Some((
        peri_acp_types::dynamic_mcp::DynamicMcpInstanceKey {
            logical: peri_acp_types::dynamic_mcp::DynamicMcpLogicalKey {
                session_id: session_id.to_string(),
                server_name: server_name.to_string(),
            },
            incarnation_id: peri_acp_types::dynamic_mcp::DynamicMcpIncarnationId::from_string(
                incarnation_id,
            ),
        },
        flow_id.to_string(),
    )))
}

fn validate_dynamic_instance(
    cfg: &AcpServerConfig,
    instance: &peri_acp_types::dynamic_mcp::DynamicMcpInstanceKey,
) -> Result<(), AcpError> {
    let deployment = cfg
        .dynamic_mcp
        .as_ref()
        .ok_or_else(|| AcpError::new(-32603, "dynamic mcp not available"))?;
    if !deployment.accepts_instance(instance) {
        return Err(AcpError::new(-32602, "stale Dynamic MCP identity"));
    }
    Ok(())
}

pub(super) fn handle_list(_params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    let caps = cfg.session_manager.effective_host_caps();
    if !caps.oauth {
        return Err(AcpError::new(
            -32601,
            "peri.oauth capability not negotiated",
        ));
    }
    let pool = cfg
        .mcp_pool
        .clone()
        .ok_or_else(|| AcpError::new(-32603, "mcp pool not available"))?
        .downcast_arc::<peri_middlewares::mcp::McpClientPool>()
        .map_err(|_| AcpError::new(-32603, "mcp pool type mismatch"))?;
    let mut servers = pool.all_server_infos();
    servers.sort_by(|left, right| left.name.cmp(&right.name));
    let servers = servers
        .into_iter()
        .filter(|server| crate::event::oauth::validate_server_name(&server.name).is_ok())
        .take(256)
        .map(|server| {
            let connection_status = match server.status {
                peri_middlewares::mcp::ClientStatus::Connected => "connected",
                peri_middlewares::mcp::ClientStatus::Failed(_) => "failed",
                peri_middlewares::mcp::ClientStatus::Disconnected => "disconnected",
                peri_middlewares::mcp::ClientStatus::Disabled => "disabled",
                peri_middlewares::mcp::ClientStatus::Uninitialized => "uninitialized",
            };
            let oauth_status = match server.oauth_status {
                peri_middlewares::mcp::OAuthStatus::None => "none",
                peri_middlewares::mcp::OAuthStatus::Authorized => "authorized",
                peri_middlewares::mcp::OAuthStatus::NeedsAuthorization => "needs_authorization",
            };
            serde_json::json!({
                "name": server.name,
                "transport": server.transport_type,
                "connectionStatus": connection_status,
                "oauthStatus": oauth_status,
                "activeFlowId": pool.active_oauth_flow(&server.name),
                "toolsCount": server.tool_count,
                "resourcesCount": server.resource_count,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "servers": servers }))
}

pub(super) fn handle_oauth_start(params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    // 用户经 MCP 面板显式发起授权：host pool 异步执行 OAuth 流程
    // （spawn_oauth_flow 内部标记 NeedsAuthorization → run_oauth_flow
    // → AuthorizationNeeded 事件 → TUI 弹 popup）。不阻塞请求。
    let server_name = params
        .get("server_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing 'server_name'"))?
        .to_string();
    crate::event::oauth::validate_server_name(&server_name)
        .map_err(|error| AcpError::new(-32602, error.to_string()))?;
    let caps = cfg.session_manager.effective_host_caps();
    if !caps.oauth && !caps.agent_event {
        return Err(AcpError::new(-32601, "OAuth capability not negotiated"));
    }
    let flow_id = match params.get("flow_id").and_then(Value::as_str) {
        Some(flow_id) => {
            crate::event::oauth::validate_identifier(flow_id)
                .map_err(|error| AcpError::new(-32602, error.to_string()))?;
            flow_id.to_string()
        }
        None if caps.agent_event && !caps.oauth => uuid::Uuid::now_v7().to_string(),
        None => return Err(AcpError::new(-32602, "missing 'flow_id'")),
    };
    let pool = cfg
        .mcp_pool
        .clone()
        .ok_or_else(|| AcpError::new(-32603, "mcp pool not available"))?;
    match pool.downcast_arc::<peri_middlewares::mcp::McpClientPool>() {
        Ok(p) => {
            let disposition = p.spawn_oauth_flow_with_id(&server_name, &flow_id);
            let (status, active_flow_id) = match disposition {
                peri_middlewares::mcp::OAuthStartDisposition::Started => {
                    ("started", flow_id.clone())
                }
                peri_middlewares::mcp::OAuthStartDisposition::AlreadyActive => {
                    ("already_active", flow_id.clone())
                }
                peri_middlewares::mcp::OAuthStartDisposition::Conflict { active_flow_id } => {
                    ("conflict", active_flow_id)
                }
            };
            Ok(serde_json::json!({
                "success": status != "conflict",
                "status": status,
                "flowId": flow_id,
                "activeFlowId": active_flow_id,
            }))
        }
        Err(_) => Err(AcpError::new(-32603, "mcp pool type mismatch")),
    }
}

pub(super) fn handle_oauth_callback(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    let caps = cfg.session_manager.effective_host_caps();
    if caps.oauth {
        return Err(AcpError::new(
            -32601,
            "peri.oauth uses the loopback callback; callback codes are not accepted over ACP",
        ));
    }
    if !caps.agent_event {
        return Err(AcpError::new(-32601, "OAuth capability not negotiated"));
    }
    let dynamic_identity = dynamic_oauth_identity(params)?;
    let server_name = params
        .get("server_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing 'server_name'"))?
        .to_string();
    let code = params
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let state = params
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if code.len() > 4096 || state.len() > 4096 {
        return Err(AcpError::new(-32602, "OAuth callback value too long"));
    }
    let pool = cfg
        .mcp_pool
        .clone()
        .ok_or_else(|| AcpError::new(-32603, "mcp pool not available"))?;
    let pool = pool
        .downcast_arc::<peri_middlewares::mcp::McpClientPool>()
        .map_err(|_| AcpError::new(-32603, "mcp pool type mismatch"))?;
    let result = match dynamic_identity {
        Some((instance, flow_id)) => {
            validate_dynamic_instance(cfg, &instance)?;
            pool.deliver_dynamic_oauth_callback(instance, &flow_id, code, state)
        }
        None => pool.deliver_oauth_callback(&server_name, code, state),
    };
    result
        .map(|_| serde_json::json!({ "success": true }))
        .map_err(|e| AcpError::new(-32603, e))
}

pub(super) fn handle_oauth_cancel(
    params: &Value,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    let caps = cfg.session_manager.effective_host_caps();
    if !caps.oauth && !caps.agent_event {
        return Err(AcpError::new(-32601, "OAuth capability not negotiated"));
    }
    let pool = cfg
        .mcp_pool
        .clone()
        .ok_or_else(|| AcpError::new(-32603, "mcp pool not available"))?;
    let pool = pool
        .downcast_arc::<peri_middlewares::mcp::McpClientPool>()
        .map_err(|_| AcpError::new(-32603, "mcp pool type mismatch"))?;
    let cancelled = match dynamic_oauth_identity(params)? {
        Some((instance, flow_id)) => {
            validate_dynamic_instance(cfg, &instance)?;
            pool.cancel_dynamic_oauth_flow(instance, &flow_id)
        }
        None if caps.agent_event && !caps.oauth => {
            let server_name = params
                .get("server_name")
                .and_then(Value::as_str)
                .ok_or_else(|| AcpError::new(-32602, "missing 'server_name'"))?;
            crate::event::oauth::validate_server_name(server_name)
                .map_err(|error| AcpError::new(-32602, error.to_string()))?;
            pool.cancel_oauth_callback(server_name)
        }
        None => return Err(AcpError::new(-32602, "missing Dynamic MCP identity")),
    };
    Ok(serde_json::json!({ "success": true, "cancelled": cancelled }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::dynamic_oauth_identity;

    #[test]
    fn dynamic_oauth_wire_identity_requires_all_four_fields() {
        for partial in [
            json!({ "session_id": "session-a" }),
            json!({ "server_name": "docs", "flow_id": "flow-1" }),
            json!({
                "session_id": "session-a",
                "server_name": "docs",
                "incarnation_id": "inc-1"
            }),
        ] {
            let error = dynamic_oauth_identity(&partial).unwrap_err();
            assert_eq!(error.code, -32602);
            assert_eq!(error.message, "incomplete Dynamic MCP identity");
        }
    }

    #[test]
    fn callback_and_cancel_share_canonical_dynamic_oauth_wire_identity() {
        let params = json!({
            "session_id": "session-a",
            "server_name": "docs",
            "incarnation_id": "inc-1",
            "flow_id": "flow-1"
        });
        let (instance, flow_id) = dynamic_oauth_identity(&params).unwrap().unwrap();
        assert_eq!(instance.logical.session_id, "session-a");
        assert_eq!(instance.logical.server_name, "docs");
        assert_eq!(instance.incarnation_id.as_str(), "inc-1");
        assert_eq!(flow_id, "flow-1");
    }

    #[test]
    fn static_legacy_wire_has_no_dynamic_identity() {
        assert!(dynamic_oauth_identity(&json!({ "server_name": "docs" }))
            .unwrap()
            .is_none());
        assert!(dynamic_oauth_identity(&json!({})).unwrap().is_none());
    }
}
