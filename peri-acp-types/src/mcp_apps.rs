//! MCP Apps Stable ACP relay contracts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MCP_APPS_PROTOCOL_VERSION: &str = "2026-01-26";
pub const MCP_APPS_ENVELOPE_VERSION: &str = "1";
pub const MCP_APPS_HTML_MIME: &str = "text/html;profile=mcp-app";
pub const MCP_APPS_ENV: &str = "PERI_MCP_APPS";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpAppOpenRequest {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub server_id: String,
    pub tool_name: String,
    pub owner_session_id: String,
    pub invocation_token: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAppOpenResponse {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub mcp_protocol_version: String,
    pub server_id: String,
    pub app_session_id: String,
    pub resource_uri: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpAppRequest {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub server_id: String,
    pub app_session_id: String,
    pub resource_uri: String,
    pub payload: JsonRpcRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpResourceRequest {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub server_id: String,
    pub app_session_id: String,
    pub resource_uri: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAppResponse {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub mcp_protocol_version: String,
    pub server_id: String,
    pub app_session_id: String,
    pub resource_uri: String,
    pub payload: JsonRpcResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceResponse {
    pub envelope_version: String,
    pub apps_protocol_version: String,
    pub mcp_protocol_version: String,
    pub server_id: String,
    pub resources: Vec<RawResource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: JsonRpcVersion,
    pub id: JsonRpcId,
    pub method: String,
    pub params: CallToolParams,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallToolParams {
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    String(String),
    Number(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JsonRpcVersion {
    #[serde(rename = "2.0")]
    V2,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcResponse {
    Result(JsonRpcResultResponse),
    Error(JsonRpcErrorResponse),
}

impl JsonRpcResponse {
    pub fn id(&self) -> &JsonRpcId {
        match self {
            Self::Result(response) => &response.id,
            Self::Error(response) => &response.id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonRpcResultResponse {
    pub jsonrpc: JsonRpcVersion,
    pub id: JsonRpcId,
    pub result: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonRpcErrorResponse {
    pub jsonrpc: JsonRpcVersion,
    pub id: JsonRpcId,
    pub error: JsonRpcError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawResource {
    pub uri: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(rename = "_meta", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl RawResource {
    pub fn is_valid_app_resource(&self, requested_uri: &str) -> bool {
        self.uri == requested_uri
            && self.uri.starts_with("ui://")
            && self.mime_type == MCP_APPS_HTML_MIME
            && self.text.is_some() != self.blob.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCallToolResult {
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(rename = "_meta", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSessionBinding {
    pub app_session_id: String,
    pub owner_connection_id: String,
    pub owner_session_id: String,
    pub server_id: String,
    pub server_generation: u64,
    pub resource_uri: String,
    pub instantiating_tool: String,
    pub apps_protocol_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpAppsErrorKind {
    InvalidEnvelope,
    UnsupportedEnvelopeVersion,
    UnsupportedAppsVersion,
    CapabilityDisabled,
    UnknownServer,
    ServerDisconnected,
    StaleServerGeneration,
    InvalidSession,
    Forbidden,
    ToolNotFound,
    ToolNotAppVisible,
    ResourceNotFound,
    InvalidResource,
    PolicyDenied,
    Cancelled,
    UnsupportedMethod,
    UpstreamProtocolError,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("MCP Apps relay request failed")]
pub struct McpAppsRelayError {
    pub kind: McpAppsErrorKind,
}

#[async_trait::async_trait]
pub trait McpAppsRelayPort: Send + Sync {
    fn close_connection(&self, owner_connection_id: &str);
    fn close_session(&self, owner_session_id: &str);
    fn begin_session_turn(&self, owner_session_id: &str);

    async fn open_app(
        &self,
        owner_connection_id: &str,
        request: &McpAppOpenRequest,
    ) -> Result<(String, AppSessionBinding), McpAppsRelayError>;

    async fn validate_binding(
        &self,
        binding: &AppSessionBinding,
    ) -> Result<String, McpAppsRelayError>;

    async fn read_resource(
        &self,
        binding: &AppSessionBinding,
    ) -> Result<(String, Vec<RawResource>), McpAppsRelayError>;

    async fn call_tool(
        &self,
        binding: &AppSessionBinding,
        request: JsonRpcRequest,
    ) -> Result<(String, JsonRpcResponse), McpAppsRelayError>;
}

#[cfg(test)]
#[path = "mcp_apps_test.rs"]
mod tests;
