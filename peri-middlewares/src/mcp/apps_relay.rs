use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::{
    mcp_apps::{
        AppSessionBinding, JsonRpcRequest, JsonRpcResponse, JsonRpcResultResponse,
        McpAppOpenRequest, McpAppsErrorKind, McpAppsRelayError, McpAppsRelayPort, RawResource,
    },
    tools::{EffectiveToolCall, EffectiveToolErrorCode},
};

use super::{
    apps::{tool_resource_uri, tool_visibility},
    client::McpClientPool,
};

const MAX_ACTIVE_LEASES: usize = 1024;

/// Production MCP Apps relay backed by the deployment-scoped pool.
///
/// Resource/catalog access and App `tools/call` are backed by the same deployment pool.
/// Tool calls require a consumed, connection-bound canonical dispatcher lease.
pub struct PoolMcpAppsRelay {
    pool: Arc<McpClientPool>,
    active_leases: Mutex<HashMap<String, super::apps::McpAppBindingLease>>,
}

impl PoolMcpAppsRelay {
    pub fn new(pool: Arc<McpClientPool>) -> Self {
        Self {
            pool,
            active_leases: Mutex::new(HashMap::new()),
        }
    }

    fn cleanup_active_leases(&self) {
        let mut leases = self.active_leases.lock();
        leases.retain(|_, lease| lease.is_valid());
        while leases.len() > MAX_ACTIVE_LEASES {
            let Some(key) = leases.keys().next().cloned() else {
                break;
            };
            if let Some(lease) = leases.remove(&key) {
                lease.cancellation.cancel();
            }
        }
    }

    fn handle(
        &self,
        server_id: &str,
    ) -> Result<Arc<super::client::McpClientHandle>, McpAppsRelayError> {
        self.pool
            .get_client(server_id)
            .filter(|handle| handle.peer.is_some())
            .ok_or_else(|| relay_error(McpAppsErrorKind::ServerDisconnected))
    }
}

#[async_trait]
impl McpAppsRelayPort for PoolMcpAppsRelay {
    fn close_connection(&self, owner_connection_id: &str) {
        self.pool
            .app_binding_leases
            .purge_raw_results_for_connection(owner_connection_id);
        let mut active = self.active_leases.lock();
        active.retain(|_, lease| {
            let keep = lease.owner_connection_id.as_deref() != Some(owner_connection_id);
            if !keep {
                lease.cancellation.cancel();
            }
            keep
        });
    }

    fn close_session(&self, owner_session_id: &str) {
        self.pool
            .app_binding_leases
            .revoke_session(owner_session_id);
        let mut active = self.active_leases.lock();
        active.retain(|_, lease| {
            let keep = lease.owner_session_id != owner_session_id;
            if !keep {
                lease.cancellation.cancel();
            }
            keep
        });
    }

    fn begin_session_turn(&self, owner_session_id: &str) {
        self.close_session(owner_session_id);
    }

    async fn open_app(
        &self,
        owner_connection_id: &str,
        request: &McpAppOpenRequest,
    ) -> Result<(String, AppSessionBinding), McpAppsRelayError> {
        let handle = self.handle(&request.server_id)?;
        let tool = handle
            .tools
            .iter()
            .find(|tool| tool.name.as_ref() == request.tool_name)
            .ok_or_else(|| relay_error(McpAppsErrorKind::ToolNotFound))?;
        if !tool_visibility(tool).app {
            return Err(relay_error(McpAppsErrorKind::ToolNotAppVisible));
        }
        let resource_uri = tool_resource_uri(tool)
            .ok_or_else(|| relay_error(McpAppsErrorKind::InvalidResource))?;
        let generation = self.pool.handle_generation(&handle);
        let lease = self
            .pool
            .app_binding_leases
            .consume(
                &request.server_id,
                &request.tool_name,
                generation,
                &resource_uri,
                &request.owner_session_id,
                &request.invocation_token,
                owner_connection_id,
            )
            .ok_or_else(|| relay_error(McpAppsErrorKind::PolicyDenied))?;
        let app_session_id = uuid::Uuid::now_v7().to_string();
        let binding = AppSessionBinding {
            app_session_id: app_session_id.clone(),
            owner_connection_id: owner_connection_id.to_string(),
            owner_session_id: lease.owner_session_id.clone(),
            server_id: request.server_id.clone(),
            server_generation: generation,
            resource_uri,
            instantiating_tool: request.tool_name.clone(),
            apps_protocol_version: request.apps_protocol_version.clone(),
        };
        self.cleanup_active_leases();
        self.active_leases.lock().insert(app_session_id, lease);
        self.cleanup_active_leases();
        let protocol = handle
            .peer
            .as_ref()
            .and_then(|peer| peer.peer_info())
            .map(|info| info.protocol_version.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        Ok((protocol, binding))
    }

    async fn validate_binding(
        &self,
        binding: &AppSessionBinding,
    ) -> Result<String, McpAppsRelayError> {
        self.cleanup_active_leases();
        let handle = self.handle(&binding.server_id)?;
        if self.pool.handle_generation(&handle) != binding.server_generation {
            return Err(relay_error(McpAppsErrorKind::StaleServerGeneration));
        }
        handle
            .peer
            .as_ref()
            .and_then(|peer| peer.peer_info())
            .map(|info| info.protocol_version.to_string())
            .ok_or_else(|| relay_error(McpAppsErrorKind::ServerDisconnected))
    }

    async fn read_resource(
        &self,
        binding: &AppSessionBinding,
    ) -> Result<(String, Vec<RawResource>), McpAppsRelayError> {
        let handle = self.handle(&binding.server_id)?;
        if self.pool.handle_generation(&handle) != binding.server_generation {
            return Err(relay_error(McpAppsErrorKind::StaleServerGeneration));
        }
        let peer = handle
            .peer
            .as_ref()
            .ok_or_else(|| relay_error(McpAppsErrorKind::ServerDisconnected))?;
        let result = peer
            .read_resource(rmcp::model::ReadResourceRequestParams::new(
                binding.resource_uri.clone(),
            ))
            .await
            .map_err(|_| relay_error(McpAppsErrorKind::ResourceNotFound))?;
        let raw: Vec<RawResource> = serde_json::to_value(result)
            .ok()
            .and_then(|value| value.get("contents")?.as_array().cloned())
            .and_then(|values| {
                values
                    .into_iter()
                    .map(serde_json::from_value)
                    .collect::<Result<Vec<_>, _>>()
                    .ok()
            })
            .filter(|values| !values.is_empty())
            .ok_or_else(|| relay_error(McpAppsErrorKind::InvalidResource))?;
        let protocol = peer
            .peer_info()
            .map(|info| info.protocol_version.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        Ok((protocol, raw))
    }

    async fn call_tool(
        &self,
        binding: &AppSessionBinding,
        request: JsonRpcRequest,
    ) -> Result<(String, JsonRpcResponse), McpAppsRelayError> {
        self.cleanup_active_leases();
        let protocol = self.validate_binding(binding).await?;
        let lease = self
            .active_leases
            .lock()
            .get(&binding.app_session_id)
            .cloned()
            .filter(super::apps::McpAppBindingLease::is_valid)
            .filter(|lease| self.pool.app_binding_leases.is_current_turn(lease))
            .ok_or_else(|| relay_error(McpAppsErrorKind::PolicyDenied))?;
        if lease.owner_session_id != binding.owner_session_id
            || lease.server_generation != binding.server_generation
        {
            return Err(relay_error(McpAppsErrorKind::StaleServerGeneration));
        }
        let effective_tool_name = lease
            .allowed_tools
            .get(&request.params.name)
            .cloned()
            .ok_or_else(|| relay_error(McpAppsErrorKind::Forbidden))?;
        let invocation_id = format!(
            "mcp-app:{}:{}",
            binding.owner_connection_id,
            uuid::Uuid::new_v4()
        );
        let result = lease
            .dispatcher
            .dispatch(
                EffectiveToolCall {
                    invocation_id: invocation_id.clone(),
                    tool_name: effective_tool_name,
                    input: serde_json::Value::Object(request.params.arguments),
                    parent_invocation_id: None,
                },
                lease.cancellation.child_token(),
            )
            .await;
        let response = match result {
            Ok(_) => {
                let raw = self
                    .pool
                    .app_binding_leases
                    .take_raw_result(&invocation_id)
                    .ok_or_else(|| relay_error(McpAppsErrorKind::UpstreamProtocolError))?;
                JsonRpcResponse::Result(JsonRpcResultResponse {
                    jsonrpc: request.jsonrpc,
                    id: request.id,
                    result: raw,
                })
            }
            Err(error) => {
                // MCP `isError: true` is still a protocol-successful raw CallToolResult.
                // McpToolBridge records it before projecting the model-facing error text.
                if let Some(raw) = self.pool.app_binding_leases.take_raw_result(&invocation_id) {
                    JsonRpcResponse::Result(JsonRpcResultResponse {
                        jsonrpc: request.jsonrpc,
                        id: request.id,
                        result: raw,
                    })
                } else {
                    let kind = match error.code {
                        EffectiveToolErrorCode::Cancelled => McpAppsErrorKind::Cancelled,
                        EffectiveToolErrorCode::PermissionDenied
                        | EffectiveToolErrorCode::UserRejected => McpAppsErrorKind::PolicyDenied,
                        EffectiveToolErrorCode::UnknownTool => McpAppsErrorKind::ToolNotFound,
                        _ => McpAppsErrorKind::UpstreamProtocolError,
                    };
                    return Err(relay_error(kind));
                }
            }
        };
        Ok((protocol, response))
    }
}

fn relay_error(kind: McpAppsErrorKind) -> McpAppsRelayError {
    McpAppsRelayError { kind }
}
