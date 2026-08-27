use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use peri_acp_types::tools::EffectiveToolDispatcher;

use async_trait::async_trait;
use serde_json::{Map, Value};
use thiserror::Error;

pub const MCP_APPS_VERSION: &str = "2026-01-26";
pub const MCP_APP_MIME_TYPE: &str = "text/html;profile=mcp-app";
pub const MCP_UI_EXTENSION: &str = "io.modelcontextprotocol/ui";
const BINDING_LEASE_TTL: Duration = Duration::from_secs(300);
const RAW_RESULT_TTL: Duration = Duration::from_secs(120);
const MAX_PENDING_LEASES: usize = 1024;
const MAX_RAW_RESULTS: usize = 1024;

#[derive(Clone)]
pub struct McpAppBindingLease {
    pub owner_session_id: String,
    pub owner_connection_id: Option<String>,
    pub turn_generation: String,
    pub server_id: String,
    pub server_generation: u64,
    pub resource_uri: String,
    pub instantiating_tool: String,
    pub invocation_token: String,
    pub allowed_tools: HashMap<String, String>,
    pub dispatcher: Arc<dyn EffectiveToolDispatcher>,
    pub cancellation: tokio_util::sync::CancellationToken,
    expires_at: Instant,
}

impl McpAppBindingLease {
    // Identity, routing, dispatcher and revocation fields are all mandatory security inputs.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner_session_id: String,
        turn_generation: String,
        server_id: String,
        server_generation: u64,
        resource_uri: String,
        instantiating_tool: String,
        invocation_token: String,
        allowed_tools: HashMap<String, String>,
        dispatcher: Arc<dyn EffectiveToolDispatcher>,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            owner_session_id,
            owner_connection_id: None,
            turn_generation,
            server_id,
            server_generation,
            resource_uri,
            instantiating_tool,
            invocation_token,
            allowed_tools,
            dispatcher,
            cancellation,
            expires_at: Instant::now() + BINDING_LEASE_TTL,
        }
    }

    pub fn is_valid(&self) -> bool {
        Instant::now() < self.expires_at && !self.cancellation.is_cancelled()
    }
}

#[derive(Default)]
pub struct McpAppBindingLeaseRegistry {
    leases: Mutex<HashMap<(String, String), Vec<McpAppBindingLease>>>,
    current_turns: Mutex<HashMap<String, String>>,
    raw_results: Mutex<HashMap<String, (Instant, RawCallToolResult)>>,
}

impl McpAppBindingLeaseRegistry {
    fn cleanup(&self) {
        let mut leases = self.leases.lock();
        leases.retain(|_, values| {
            values.retain(McpAppBindingLease::is_valid);
            !values.is_empty()
        });
        while leases.values().map(Vec::len).sum::<usize>() > MAX_PENDING_LEASES {
            let Some(key) = leases.keys().next().cloned() else {
                break;
            };
            if let Some(values) = leases.get_mut(&key) {
                values.remove(0);
                if values.is_empty() {
                    leases.remove(&key);
                }
            }
        }
        drop(leases);

        let now = Instant::now();
        let mut raw_results = self.raw_results.lock();
        raw_results.retain(|_, (expires_at, _)| now < *expires_at);
        while raw_results.len() > MAX_RAW_RESULTS {
            let Some(key) = raw_results.keys().next().cloned() else {
                break;
            };
            raw_results.remove(&key);
        }
    }

    pub fn begin_session_turn(&self, owner_session_id: &str) {
        self.revoke_session(owner_session_id);
    }

    pub fn revoke_session(&self, owner_session_id: &str) {
        self.current_turns.lock().remove(owner_session_id);
        let mut leases = self.leases.lock();
        leases.retain(|_, values| {
            values.retain(|lease| {
                if lease.owner_session_id == owner_session_id {
                    lease.cancellation.cancel();
                    false
                } else {
                    lease.is_valid()
                }
            });
            !values.is_empty()
        });
    }

    pub fn issue(&self, mut lease: McpAppBindingLease) {
        self.cleanup();
        lease.expires_at = Instant::now() + BINDING_LEASE_TTL;
        self.current_turns.lock().insert(
            lease.owner_session_id.clone(),
            lease.turn_generation.clone(),
        );
        let key = (lease.server_id.clone(), lease.instantiating_tool.clone());
        self.leases.lock().entry(key).or_default().push(lease);
        self.cleanup();
    }

    pub fn is_current_turn(&self, lease: &McpAppBindingLease) -> bool {
        lease.is_valid()
            && self
                .current_turns
                .lock()
                .get(&lease.owner_session_id)
                .is_some_and(|turn| turn == &lease.turn_generation)
    }

    pub fn record_raw_result(&self, invocation_id: &str, result: RawCallToolResult) {
        self.cleanup();
        self.raw_results.lock().insert(
            invocation_id.to_string(),
            (Instant::now() + RAW_RESULT_TTL, result),
        );
        self.cleanup();
    }

    pub fn take_raw_result(&self, invocation_id: &str) -> Option<RawCallToolResult> {
        self.cleanup();
        self.raw_results
            .lock()
            .remove(invocation_id)
            .map(|(_, result)| result)
    }

    pub fn purge_raw_results_for_connection(&self, owner_connection_id: &str) {
        let prefix = format!("mcp-app:{owner_connection_id}:");
        self.raw_results
            .lock()
            .retain(|invocation_id, _| !invocation_id.starts_with(&prefix));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn consume(
        &self,
        server_id: &str,
        tool_name: &str,
        generation: u64,
        resource_uri: &str,
        owner_session_id: &str,
        invocation_token: &str,
        owner_connection_id: &str,
    ) -> Option<McpAppBindingLease> {
        self.cleanup();
        let key = (server_id.to_string(), tool_name.to_string());
        let current_turns = self.current_turns.lock().clone();
        let mut leases = self.leases.lock();
        let values = leases.get_mut(&key)?;
        let index = values.iter().position(|lease| {
            lease.is_valid()
                && lease.owner_session_id == owner_session_id
                && lease.invocation_token == invocation_token
                && current_turns
                    .get(&lease.owner_session_id)
                    .is_some_and(|turn| turn == &lease.turn_generation)
                && lease.server_generation == generation
                && lease.resource_uri == resource_uri
        })?;
        let mut lease = values.remove(index);
        lease.owner_connection_id = Some(owner_connection_id.to_string());
        if values.is_empty() {
            leases.remove(&key);
        }
        Some(lease)
    }
}

pub const MCP_APPS_ENV: &str = "PERI_MCP_APPS";

pub fn deployment_profile(apps_enabled: bool) -> McpCapabilityProfile {
    if apps_enabled {
        McpCapabilityProfile::negotiated([MCP_APP_MIME_TYPE])
    } else {
        McpCapabilityProfile::disabled()
    }
}

/// 由 ACP connection 注入的不可变 MCP capability profile。
///
/// 默认值关闭 Apps；只有显式协商出的受支持 MIME 才会进入 profile。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpCapabilityProfile {
    apps_mime_types: BTreeSet<String>,
}

impl McpCapabilityProfile {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn negotiated<'a>(mime_types: impl IntoIterator<Item = &'a str>) -> Self {
        let apps_mime_types = mime_types
            .into_iter()
            .filter(|mime| *mime == MCP_APP_MIME_TYPE)
            .map(str::to_owned)
            .collect();
        Self { apps_mime_types }
    }

    pub fn apps_enabled(&self) -> bool {
        !self.apps_mime_types.is_empty()
    }

    pub fn apps_mime_types(&self) -> impl Iterator<Item = &str> {
        self.apps_mime_types.iter().map(String::as_str)
    }

    pub(crate) fn ui_extension(&self) -> Option<Map<String, Value>> {
        self.apps_enabled().then(|| {
            Map::from_iter([(
                "mimeTypes".to_string(),
                Value::Array(
                    self.apps_mime_types()
                        .map(|mime| Value::String(mime.to_string()))
                        .collect(),
                ),
            )])
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolVisibility {
    pub model: bool,
    pub app: bool,
}

impl ToolVisibility {
    /// 缺失时兼容 model + app；任何 malformed、空数组或未知值均关闭两面。
    pub fn from_tool_meta(meta: Option<&Value>) -> Self {
        let Some(meta) = meta else {
            return Self {
                model: true,
                app: true,
            };
        };
        let Some(meta) = meta.as_object() else {
            return Self {
                model: false,
                app: false,
            };
        };
        let Some(ui) = meta.get("ui") else {
            return Self {
                model: true,
                app: true,
            };
        };
        let Some(ui) = ui.as_object() else {
            return Self {
                model: false,
                app: false,
            };
        };
        let Some(visibility) = ui.get("visibility") else {
            return Self {
                model: true,
                app: true,
            };
        };
        let Some(values) = visibility.as_array() else {
            return Self {
                model: false,
                app: false,
            };
        };
        if values.is_empty()
            || values
                .iter()
                .any(|value| !matches!(value.as_str(), Some("model") | Some("app")))
        {
            return Self {
                model: false,
                app: false,
            };
        }
        Self {
            model: values.iter().any(|value| value == "model"),
            app: values.iter().any(|value| value == "app"),
        }
    }
}

/// 读取并 canonicalize `_meta.ui.resourceUri`；legacy key 仅作兼容输入。
/// canonical 与 legacy 冲突或值类型错误时 fail closed。
pub fn canonical_resource_uri(meta: Option<&Value>) -> Option<String> {
    let meta = meta?.as_object()?;
    let canonical = meta
        .get("ui")
        .and_then(Value::as_object)
        .and_then(|ui| ui.get("resourceUri"));
    let legacy = meta.get("ui/resourceUri");
    match (canonical, legacy) {
        (Some(left), Some(right)) if left != right => None,
        (Some(value), _) | (_, Some(value)) => value
            .as_str()
            .filter(|uri| uri.starts_with("ui://"))
            .map(str::to_owned),
        (None, None) => None,
    }
}

pub fn tool_visibility(tool: &rmcp::model::Tool) -> ToolVisibility {
    let value = serde_json::to_value(tool).unwrap_or(Value::Null);
    ToolVisibility::from_tool_meta(value.get("_meta"))
}

pub fn tool_resource_uri(tool: &rmcp::model::Tool) -> Option<String> {
    let value = serde_json::to_value(tool).ok()?;
    canonical_resource_uri(value.get("_meta"))
}

pub fn raw_tool(tool: &rmcp::model::Tool) -> RawMcpTool {
    serde_json::to_value(tool).unwrap_or(Value::Null)
}

pub fn raw_resource(resource: &rmcp::model::Resource) -> RawMcpResource {
    serde_json::to_value(resource).unwrap_or(Value::Null)
}

/// Raw MCP payload stays as JSON until the legacy model projection boundary.
pub type RawMcpTool = Value;
pub type RawMcpResource = Value;
pub type RawCallToolResult = Value;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum McpAppsInvocationError {
    #[error("MCP Apps canonical invocation seam is unavailable")]
    Unavailable,
    #[error("MCP App tool is not visible to this session")]
    Forbidden,
    #[error("MCP App tool invocation failed")]
    InvocationFailed,
}

/// 只能由 session-local canonical dispatcher/HITL 实现；不得以 MCP peer 实现此 seam。
#[async_trait]
pub trait McpAppsInvocationSeam: Send + Sync {
    async fn call_tool(
        &self,
        effective_tool_name: &str,
        arguments: Value,
    ) -> Result<RawCallToolResult, McpAppsInvocationError>;
}

#[derive(Clone, Default)]
pub struct McpAppsInvoker {
    seam: Option<Arc<dyn McpAppsInvocationSeam>>,
}

impl McpAppsInvoker {
    pub fn unavailable() -> Self {
        Self::default()
    }

    pub fn with_seam(seam: Arc<dyn McpAppsInvocationSeam>) -> Self {
        Self { seam: Some(seam) }
    }

    pub async fn call_tool(
        &self,
        effective_tool_name: &str,
        arguments: Value,
    ) -> Result<RawCallToolResult, McpAppsInvocationError> {
        let seam = self
            .seam
            .as_ref()
            .ok_or(McpAppsInvocationError::Unavailable)?;
        seam.call_tool(effective_tool_name, arguments).await
    }
}

#[cfg(test)]
#[path = "apps_test.rs"]
mod tests;
