//! Dynamic MCP cross-layer contracts.
//!
//! This module contains only serializable DTOs, opaque identities and runtime
//! port payloads. Transport, registry and lifecycle implementations belong to
//! `peri-middlewares`.

use std::{collections::BTreeMap, fmt, sync::Arc};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{plugin::McpProtocolVersion, tools::BaseTool};

macro_rules! opaque_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(format!("{}{}", $prefix, Uuid::now_v7()))
            }

            pub fn from_string(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.0)
                    .finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

opaque_id!(DynamicMcpOperationId, "mcpop_");
opaque_id!(DynamicMcpIncarnationId, "mcpinc_");

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicMcpLogicalKey {
    pub session_id: String,
    pub server_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicMcpInstanceKey {
    pub logical: DynamicMcpLogicalKey,
    pub incarnation_id: DynamicMcpIncarnationId,
}

/// An opaque reference submitted by the model. It can never contain an inline
/// secret value because unknown fields are rejected during deserialization.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretRef {
    secret_ref: String,
}

impl SecretRef {
    pub fn new(value: impl Into<String>) -> Result<Self, DynamicMcpConfigError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(DynamicMcpConfigError::Invalid(
                "secretRef must not be empty".to_string(),
            ));
        }
        Ok(Self { secret_ref: value })
    }

    pub fn as_str(&self) -> &str {
        &self.secret_ref
    }
}

/// Resolved secret material. Deliberately not `Clone`, `Debug`, `Serialize` or
/// `Deserialize`; it may only be exposed at the transport construction seam.
pub struct ResolvedSecret(String);

impl ResolvedSecret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DynamicMcpHeaderValue {
    Literal(String),
    Secret(SecretRef),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicMcpConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, SecretRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, DynamicMcpHeaderValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<McpProtocolVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscriptions: Option<crate::plugin::McpSubscriptionsConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum CanonicalDynamicMcpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, SecretRef>,
        cwd: Option<String>,
    },
    StreamableHttp {
        url: String,
        headers: BTreeMap<String, DynamicMcpHeaderValue>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanonicalDynamicMcpConfig {
    #[serde(flatten)]
    pub transport: CanonicalDynamicMcpTransport,
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<McpProtocolVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscriptions: Option<crate::plugin::McpSubscriptionsConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "secretRef", rename_all = "snake_case")]
pub enum DynamicMcpHeaderSummary {
    Literal(String),
    Secret(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum DynamicMcpConfigSummary {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: Option<String>,
        timeout_ms: u64,
        protocol_version: Option<McpProtocolVersion>,
        subscriptions: Option<crate::plugin::McpSubscriptionsConfig>,
    },
    StreamableHttp {
        url: String,
        headers: BTreeMap<String, DynamicMcpHeaderSummary>,
        timeout_ms: u64,
        protocol_version: Option<McpProtocolVersion>,
        subscriptions: Option<crate::plugin::McpSubscriptionsConfig>,
    },
}

impl CanonicalDynamicMcpConfig {
    pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

    pub fn safe_summary(&self) -> DynamicMcpConfigSummary {
        match &self.transport {
            CanonicalDynamicMcpTransport::Stdio {
                command,
                args,
                env,
                cwd,
            } => DynamicMcpConfigSummary::Stdio {
                command: command.clone(),
                args: args.clone(),
                env: env
                    .iter()
                    .map(|(name, reference)| (name.clone(), reference.as_str().to_string()))
                    .collect(),
                cwd: cwd.clone(),
                timeout_ms: self.timeout_ms,
                protocol_version: self.protocol_version,
                subscriptions: self.subscriptions.clone(),
            },
            CanonicalDynamicMcpTransport::StreamableHttp { url, headers } => {
                let mut parsed = url::Url::parse(url).expect("canonical Dynamic MCP URL is valid");
                parsed.set_query(None);
                parsed.set_fragment(None);
                DynamicMcpConfigSummary::StreamableHttp {
                    url: parsed.to_string(),
                    headers: headers
                        .iter()
                        .map(|(name, value)| {
                            let summary = match value {
                                DynamicMcpHeaderValue::Literal(value) => {
                                    DynamicMcpHeaderSummary::Literal(value.clone())
                                }
                                DynamicMcpHeaderValue::Secret(reference) => {
                                    DynamicMcpHeaderSummary::Secret(reference.as_str().to_string())
                                }
                            };
                            (name.clone(), summary)
                        })
                        .collect(),
                    timeout_ms: self.timeout_ms,
                    protocol_version: self.protocol_version,
                    subscriptions: self.subscriptions.clone(),
                }
            }
        }
    }

    pub fn digest(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let canonical = serde_json::to_vec(self).expect("canonical Dynamic MCP config serializes");
        Sha256::digest(canonical).into()
    }

    pub fn secret_refs(&self) -> Vec<&SecretRef> {
        match &self.transport {
            CanonicalDynamicMcpTransport::Stdio { env, .. } => env.values().collect(),
            CanonicalDynamicMcpTransport::StreamableHttp { headers, .. } => headers
                .values()
                .filter_map(|value| match value {
                    DynamicMcpHeaderValue::Secret(reference) => Some(reference),
                    DynamicMcpHeaderValue::Literal(_) => None,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DynamicMcpConfigError {
    #[error("invalid Dynamic MCP configuration: {0}")]
    Invalid(String),
}

fn valid_dynamic_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn contains_template(value: &str) -> bool {
    value.contains("${") || value.contains("{{")
}

impl DynamicMcpConfig {
    pub fn canonicalize(self) -> Result<CanonicalDynamicMcpConfig, DynamicMcpConfigError> {
        let timeout_ms = self
            .timeout_ms
            .unwrap_or(CanonicalDynamicMcpConfig::DEFAULT_TIMEOUT_MS);
        if timeout_ms == 0 {
            return Err(DynamicMcpConfigError::Invalid(
                "timeoutMs must be greater than zero".to_string(),
            ));
        }
        let transport = match (self.command, self.url) {
            (Some(command), None) => {
                if command.trim().is_empty() || contains_template(&command) {
                    return Err(DynamicMcpConfigError::Invalid(
                        "command must be a non-empty executable without templates".to_string(),
                    ));
                }
                if self.args.iter().any(|arg| contains_template(arg))
                    || self.cwd.as_deref().is_some_and(contains_template)
                {
                    return Err(DynamicMcpConfigError::Invalid(
                        "stdio arguments and cwd must not contain templates".to_string(),
                    ));
                }
                if !self.headers.is_empty() {
                    return Err(DynamicMcpConfigError::Invalid(
                        "headers are only valid for Streamable HTTP".to_string(),
                    ));
                }
                CanonicalDynamicMcpTransport::Stdio {
                    command,
                    args: self.args,
                    env: self.env,
                    cwd: self.cwd,
                }
            }
            (None, Some(url)) => {
                if !self.args.is_empty() || !self.env.is_empty() || self.cwd.is_some() {
                    return Err(DynamicMcpConfigError::Invalid(
                        "args, env and cwd are only valid for stdio".to_string(),
                    ));
                }
                let parsed = url::Url::parse(&url).map_err(|_| {
                    DynamicMcpConfigError::Invalid(
                        "url must be an absolute HTTP(S) URL".to_string(),
                    )
                })?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                    return Err(DynamicMcpConfigError::Invalid(
                        "url must be an absolute HTTP(S) URL".to_string(),
                    ));
                }
                for (name, value) in &self.headers {
                    let sensitive = matches!(
                        name.to_ascii_lowercase().as_str(),
                        "authorization" | "proxy-authorization" | "cookie" | "x-api-key"
                    );
                    if sensitive && matches!(value, DynamicMcpHeaderValue::Literal(_)) {
                        return Err(DynamicMcpConfigError::Invalid(format!(
                            "sensitive header {name} requires secretRef"
                        )));
                    }
                }
                CanonicalDynamicMcpTransport::StreamableHttp {
                    url,
                    headers: self.headers,
                }
            }
            (Some(_), Some(_)) | (None, None) => {
                return Err(DynamicMcpConfigError::Invalid(
                    "config must select exactly one of command or url".to_string(),
                ));
            }
        };
        Ok(CanonicalDynamicMcpConfig {
            transport,
            timeout_ms,
            protocol_version: self.protocol_version,
            subscriptions: self.subscriptions.filter(|value| !value.is_empty()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DynamicMcpMethod {
    Load,
    Status,
    Unload,
}

impl DynamicMcpMethod {
    pub const fn policy_name(self) -> &'static str {
        match self {
            Self::Load => "DynamicMCP.load",
            Self::Status => "DynamicMCP.status",
            Self::Unload => "DynamicMCP.unload",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicMcpLoadRequest {
    pub name: String,
    pub config: DynamicMcpConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicMcpStatusRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<DynamicMcpOperationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicMcpUnloadRequest {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "lowercase")]
pub enum DynamicMcpAction {
    Load(DynamicMcpLoadRequest),
    Status(DynamicMcpStatusRequest),
    Unload(DynamicMcpUnloadRequest),
}

impl DynamicMcpAction {
    pub fn from_tool_input(input: serde_json::Value) -> Result<Self, DynamicMcpConfigError> {
        let object = input.as_object().ok_or_else(|| {
            DynamicMcpConfigError::Invalid("DynamicMCP input must be an object".to_string())
        })?;
        if object.keys().any(|key| key != "method" && key != "params") {
            return Err(DynamicMcpConfigError::Invalid(
                "DynamicMCP input contains unknown fields".to_string(),
            ));
        }
        let method = object
            .get("method")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| DynamicMcpConfigError::Invalid("method must be a string".to_string()))?;
        let params = object
            .get("params")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        match method {
            "load" => serde_json::from_value(params)
                .map(Self::Load)
                .map_err(|_| DynamicMcpConfigError::Invalid("invalid load params".to_string())),
            "status" => serde_json::from_value(params)
                .map(Self::Status)
                .map_err(|_| DynamicMcpConfigError::Invalid("invalid status params".to_string())),
            "unload" => serde_json::from_value(params)
                .map(Self::Unload)
                .map_err(|_| DynamicMcpConfigError::Invalid("invalid unload params".to_string())),
            _ => Err(DynamicMcpConfigError::Invalid(
                "unknown DynamicMCP method".to_string(),
            )),
        }
    }

    pub fn method(&self) -> DynamicMcpMethod {
        match self {
            Self::Load(_) => DynamicMcpMethod::Load,
            Self::Status(_) => DynamicMcpMethod::Status,
            Self::Unload(_) => DynamicMcpMethod::Unload,
        }
    }

    pub fn canonicalize(self) -> Result<CanonicalDynamicMcpAction, DynamicMcpConfigError> {
        match self {
            Self::Load(request) => {
                if !valid_dynamic_name(&request.name) {
                    return Err(DynamicMcpConfigError::Invalid(
                        "name must match ^[A-Za-z0-9_-]+$".to_string(),
                    ));
                }
                Ok(CanonicalDynamicMcpAction::Load(
                    CanonicalDynamicMcpLoadRequest {
                        name: request.name,
                        config: request.config.canonicalize()?,
                    },
                ))
            }
            Self::Status(request) => {
                if request
                    .name
                    .as_deref()
                    .is_some_and(|name| !valid_dynamic_name(name))
                {
                    return Err(DynamicMcpConfigError::Invalid(
                        "name must match ^[A-Za-z0-9_-]+$".to_string(),
                    ));
                }
                if request.operation_id.is_some() && request.name.is_some() {
                    return Err(DynamicMcpConfigError::Invalid(
                        "status accepts operationId or name, not both".to_string(),
                    ));
                }
                Ok(CanonicalDynamicMcpAction::Status(request))
            }
            Self::Unload(request) => {
                if !valid_dynamic_name(&request.name) {
                    return Err(DynamicMcpConfigError::Invalid(
                        "name must match ^[A-Za-z0-9_-]+$".to_string(),
                    ));
                }
                Ok(CanonicalDynamicMcpAction::Unload(
                    CanonicalDynamicMcpUnloadRequest {
                        name: request.name,
                        expected_instance: None,
                    },
                ))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanonicalDynamicMcpLoadRequest {
    pub name: String,
    pub config: CanonicalDynamicMcpConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanonicalDynamicMcpUnloadRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_instance: Option<DynamicMcpInstanceKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "lowercase")]
pub enum CanonicalDynamicMcpAction {
    Load(CanonicalDynamicMcpLoadRequest),
    Status(DynamicMcpStatusRequest),
    Unload(CanonicalDynamicMcpUnloadRequest),
}

impl CanonicalDynamicMcpAction {
    pub fn method(&self) -> DynamicMcpMethod {
        match self {
            Self::Load(_) => DynamicMcpMethod::Load,
            Self::Status(_) => DynamicMcpMethod::Status,
            Self::Unload(_) => DynamicMcpMethod::Unload,
        }
    }

    pub fn policy_projection(&self) -> serde_json::Value {
        match self {
            Self::Load(value) => serde_json::to_value(value),
            Self::Status(value) => serde_json::to_value(value),
            Self::Unload(value) => serde_json::to_value(value),
        }
        .expect("canonical Dynamic MCP action serializes")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DynamicMcpOperationState {
    Starting,
    Authorizing,
    Connecting,
    Discovering,
    Ready,
    Revoking,
    Draining,
    Unloaded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DynamicMcpErrorCode {
    InvalidConfig,
    SecretNotFound,
    ConfigConflict,
    StartRejected,
    ConnectTimeout,
    AuthRequired,
    AuthFailed,
    InitializeFailed,
    ToolDiscoveryFailed,
    ToolNameConflict,
    TaskOwnerClosed,
    NotFound,
    ServerBusy,
    ShutdownIncomplete,
    Internal,
}

impl DynamicMcpErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidConfig => "INVALID_CONFIG",
            Self::SecretNotFound => "SECRET_NOT_FOUND",
            Self::ConfigConflict => "CONFIG_CONFLICT",
            Self::StartRejected => "START_REJECTED",
            Self::ConnectTimeout => "CONNECT_TIMEOUT",
            Self::AuthRequired => "AUTH_REQUIRED",
            Self::AuthFailed => "AUTH_FAILED",
            Self::InitializeFailed => "INITIALIZE_FAILED",
            Self::ToolDiscoveryFailed => "TOOL_DISCOVERY_FAILED",
            Self::ToolNameConflict => "TOOL_NAME_CONFLICT",
            Self::TaskOwnerClosed => "TASK_OWNER_CLOSED",
            Self::NotFound => "NOT_FOUND",
            Self::ServerBusy => "SERVER_BUSY",
            Self::ShutdownIncomplete => "SHUTDOWN_INCOMPLETE",
            Self::Internal => "INTERNAL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicMcpFailure {
    pub code: DynamicMcpErrorCode,
    pub phase: DynamicMcpOperationState,
    pub safe_summary: String,
}

impl DynamicMcpFailure {
    pub fn new(
        code: DynamicMcpErrorCode,
        phase: DynamicMcpOperationState,
        safe_summary: impl Into<String>,
    ) -> Self {
        Self {
            code,
            phase,
            safe_summary: safe_summary.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicMcpOperationStatus {
    pub operation_id: DynamicMcpOperationId,
    pub server: String,
    pub state: DynamicMcpOperationState,
    pub instance_key: DynamicMcpInstanceKey,
    pub config: DynamicMcpConfigSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<DynamicMcpFailure>,
    pub tool_count: usize,
    pub resource_count: usize,
    pub capability_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicMcpAccepted {
    pub operation_id: DynamicMcpOperationId,
    pub server: String,
    pub state: DynamicMcpOperationState,
    pub scope: String,
    pub idempotent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicMcpStatusResponse {
    pub operations: Vec<DynamicMcpOperationStatus>,
    pub capability_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DynamicMcpResponse {
    Accepted(DynamicMcpAccepted),
    Status(DynamicMcpStatusResponse),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicMcpServerProjection {
    pub instance_key: DynamicMcpInstanceKey,
    pub name: String,
    pub config: CanonicalDynamicMcpConfig,
    pub tool_count: usize,
    pub resource_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicMcpCatalogTool {
    pub name: String,
    pub aliases: Vec<String>,
    /// Static MCP entries from this server may be shadowed by the matching
    /// session dynamic server. `None` identifies core or non-MCP tools.
    pub static_mcp_server: Option<String>,
}

/// One dynamic tool together with the exact server incarnation that owns it.
/// Consumers must use `instance` as the identity fact source; tool names and
/// aliases are presentation data and must never be parsed to infer ownership.
#[derive(Clone)]
pub struct DynamicMcpToolCapability {
    pub instance: DynamicMcpInstanceKey,
    pub tool: Arc<dyn BaseTool>,
}

/// Runtime-only immutable capability snapshot. Tool objects are deliberately
/// excluded from serialization and debug projections.
#[derive(Clone, Default)]
pub struct SessionMcpCapabilitySnapshot {
    pub generation: u64,
    pub servers: BTreeMap<String, DynamicMcpServerProjection>,
    pub tools: BTreeMap<String, DynamicMcpToolCapability>,
}

impl SessionMcpCapabilitySnapshot {
    pub fn dynamic_tools(&self) -> Vec<DynamicMcpCatalogTool> {
        self.tools
            .iter()
            .map(|(name, capability)| DynamicMcpCatalogTool {
                name: name.clone(),
                aliases: capability
                    .tool
                    .aliases()
                    .iter()
                    .map(|alias| (*alias).to_string())
                    .collect(),
                static_mcp_server: None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicMcpNotification {
    pub session_id: String,
    pub instance_key: DynamicMcpInstanceKey,
    pub operation_id: DynamicMcpOperationId,
    pub state: DynamicMcpOperationState,
    pub safe_summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicMcpShutdownReport {
    Complete,
    Incomplete { unfinished_instances: usize },
}

#[cfg(test)]
#[path = "dynamic_mcp_test.rs"]
mod tests;
