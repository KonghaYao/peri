//! One in-process ACP host child: assemble, spawn, negotiate, prompt, cancel, load.
//!
//! This is the **only** module in `peri-swarm` that depends on `peri-acp`,
//! `peri-acp-types`, and `peri-resources` (`design §1.2`). The rest of the crate
//! sees [`ChildConfig`], [`ChildError`], and JSON-shaped replies.
//!
//! Lifecycle contract (`design §7.2-S4`):
//!
//! 1. [`AcpChild::spawn`] assembles a host with every optional capability
//!    closed (`design §0.4-5`: `bare = false` so a future remote `workspace`
//!    injection keeps the non-bare merge path, but builtin MCP, stdio MCP, cron,
//!    plugins, and settings hooks are all off), spawns it over
//!    `mpsc_transport_pair`, negotiates `initialize`, and creates the session
//!    with the caller-reserved root session id (S1).
//! 2. A single reader task drains host notifications into [`ItemLog`] for as
//!    long as the transport is open. Server→client requests are answered with an
//!    explicit refusal: S4 negotiates no client filesystem/terminal/permission
//!    surface, and a silent drop would hang the host.
//! 3. [`AcpChild::shutdown`] closes the client admission, joins the reader and
//!    then consumes [`peri_acp::host::AcpHostHandle::shutdown`]; dropping the
//!    handle would only detach the host task.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use peri_acp::host::assemble::{
    assemble_server_config_with_capabilities, HostAssemblyInput, HostCapabilities,
};
use peri_acp::host::{spawn_acp_server, AcpHostHandle};
use peri_acp::provider::{ConfigSource, LlmProvider};
use peri_acp::transport::mpsc::{mpsc_transport_pair, MpscClientTransport};
use peri_acp::transport::types::{AcpError, IncomingMessage};
use peri_acp::transport::AcpTransport;
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
pub use peri_acp_types::session_store::SessionStoreDeployment;
use peri_resources::RemoteWorkspaceEnvironment;
use peri_resources::Resources;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::reader::{ItemLog, PermissionStart};
use crate::domain::{escalation_servers, ApprovalDecision, McpToolSpec};
use crate::sandbox::SandboxMcpBinding;

/// `_meta` keys carrying the caller's OpenAI tool declaration on one ACP
/// `mcpServers` entry (S8a2).
///
/// Values are preserved verbatim. S8a2 injects the declaration; it does not
/// enforce approval or tool filtering, so the host currently reads only
/// `peri.subscriptions` from that `_meta` and ignores these keys.
const META_REQUIRE_APPROVAL: &str = "peri.openai.require_approval";
const META_ALLOWED_TOOLS: &str = "peri.openai.allowed_tools";
const META_EXTRA: &str = "peri.openai.extra";

/// Internal workspace MCP binding (S8a3a). ACP grants this name
/// `WorkspaceRemote`/system-server semantics, so the session waits for the
/// sandbox MCP before the first turn.
pub const WORKSPACE_MCP_SERVER_NAME: &str = "workspace";
/// Marks the internally injected entry; caller entries never carry it.
const META_SWARM_SANDBOX: &str = "peri.swarmSandbox";

/// Everything one child needs that the service already validated.
#[derive(Clone, Debug)]
pub struct ChildConfig {
    /// Canonical absolute working directory; also the host `startup_cwd`.
    pub cwd: PathBuf,
    /// Peri session store used by the ACP host (threads/history).
    pub session_store: SessionStoreDeployment,
    /// Stable machine UUID for a remote deployment. `Some` only with
    /// [`Self::workspace_root`]; a partial pair is an explicit error, never a
    /// silent fallback to the local SQLite store (S6b).
    pub machine_id: Option<Uuid>,
    /// Absolute, normalized virtual workspace root for the remote deployment.
    pub workspace_root: Option<PathBuf>,
    /// Trusted settings document (tests / future SDK). `None` loads the normal
    /// layered configuration from `cwd`.
    pub settings_json: Option<String>,
    /// Provider model for this session. `None` keeps the configured default.
    pub model: Option<String>,
    pub instructions: Option<String>,
    /// Caller-declared MCP tools (`agent.tools`). Each one becomes an HTTP entry
    /// in `session/new.mcpServers`; approval and tool filtering are recorded in
    /// the entry `_meta`, not enforced here (S8a2).
    pub mcp_tools: Vec<McpToolSpec>,
    /// Internal Bun workspace binding (S8a3a). When present it becomes the
    /// single `workspace` entry and a caller entry with the same name is
    /// rejected before assembly.
    pub workspace: Option<SandboxMcpBinding>,
    /// Reserved root session id (UUIDv7) handed to `session/new`.
    pub root_session_id: String,
}

/// One failure surface for child construction and RPC calls.
#[derive(Clone, Debug, PartialEq)]
pub enum ChildError {
    /// No resolvable provider/credential in the deployment configuration.
    ProviderUnavailable,
    /// Local resource construction failed (session store, configuration I/O).
    Local(String),
    /// A JSON-RPC call returned an error; the code is preserved for
    /// [`crate::domain::SwarmError::from_acp_error`].
    Rpc { code: i64, message: String },
    /// The transport is gone (host crashed or already shut down).
    Closed,
}

impl ChildError {
    fn rpc(error: AcpError) -> Self {
        Self::Rpc {
            code: error.code,
            message: error.message,
        }
    }
}

impl std::fmt::Display for ChildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProviderUnavailable => write!(formatter, "no LLM provider is configured"),
            Self::Local(message) => write!(formatter, "{message}"),
            Self::Rpc { code, message } => write!(formatter, "{message} (code {code})"),
            Self::Closed => write!(formatter, "ACP transport is closed"),
        }
    }
}

/// Session store for one child. Kept here so `peri-acp-types` stays inside the
/// runtime boundary (`design §1.2`).
#[must_use]
pub fn local_session_store(path: Option<PathBuf>) -> SessionStoreDeployment {
    match path {
        Some(path) => SessionStoreDeployment::local_path(path),
        None => SessionStoreDeployment::default_local(),
    }
}

/// Open the child's session store for the configured deployment.
///
/// - no remote identity ⇒ the existing local `Resources::open_deployment`
///   (S4/S5 default, unchanged);
/// - `machine_id` **and** `workspace_root` ⇒ the remote Loom store with a
///   virtual workspace identity (`design §4.1`);
/// - exactly one of the two ⇒ an explicit error. Falling back to the local
///   SQLite file here would write Swarm facts to a second database.
async fn open_session_resources(config: &ChildConfig) -> Result<Resources, ChildError> {
    match (config.machine_id, config.workspace_root.as_ref()) {
        (None, None) => Resources::open_deployment(&config.session_store).await,
        (Some(machine_id), Some(root)) => {
            let environment = RemoteWorkspaceEnvironment::virtual_workspace(
                &machine_id.to_string(),
                root.clone(),
            )
            .map_err(|error| {
                ChildError::Local(format!("invalid remote workspace environment: {error:#}"))
            })?;
            Resources::open_deployment_in_remote_environment(&config.session_store, environment)
                .await
        }
        _ => {
            return Err(ChildError::Local(
                "remote session store requires both machine_id and workspace_root; \
                 refusing to open the local store"
                    .to_owned(),
            ));
        }
    }
    .map_err(|error| ChildError::Local(format!("{error:#}")))
}

/// Stable shape of the host shutdown report, without re-exporting peri-acp types.
#[derive(Clone, Debug, PartialEq)]
pub struct ChildShutdown {
    pub session_id: String,
    pub complete: bool,
    pub report: &'static str,
}

/// Turn identity for a targeted `session/cancel` (`host/notify.rs`).
#[derive(Clone, Debug, PartialEq)]
pub struct CancelTarget {
    pub request_id: String,
    pub generation: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PromptReply {
    pub stop_reason: String,
    pub pending_tasks: u32,
}

/// One ACP `mcpServers` entry for a caller-declared MCP tool.
///
/// The transport is always HTTP (the OpenAI Agents API MCP tool shape has no
/// other transport). Headers are forwarded as the caller declared them; the
/// normalized OpenAI fields ride in `_meta` because ACP only defines transport
/// fields for a server entry.
fn mcp_server_entry(tool: &McpToolSpec) -> Value {
    let headers: Vec<Value> = tool
        .headers
        .iter()
        .map(|header| json!({ "name": header.name, "value": header.value }))
        .collect();
    let mut meta = serde_json::Map::new();
    if let Some(value) = &tool.require_approval {
        meta.insert(META_REQUIRE_APPROVAL.to_owned(), value.clone());
    }
    if let Some(value) = &tool.allowed_tools {
        meta.insert(META_ALLOWED_TOOLS.to_owned(), value.clone());
    }
    if !tool.extra.is_empty() {
        meta.insert(META_EXTRA.to_owned(), Value::Object(tool.extra.clone()));
    }
    let mut entry = json!({
        "type": "http",
        "name": tool.server_label,
        "url": tool.server_url,
        "headers": headers,
    });
    if !meta.is_empty() {
        entry["_meta"] = Value::Object(meta);
    }
    entry
}

/// One ACP `mcpServers` entry for the internal Bun workspace.
///
/// The URL is the Bun bridge proxy (`<bun>/sandboxes/<logicalEnvId>/mcp`); the
/// `Authorization` value is the deployment's outer Bun token and is only
/// assembled in memory. `_meta.peri.swarmSandbox` marks the entry as internal.
fn workspace_mcp_entry(binding: &SandboxMcpBinding) -> Value {
    let mut headers = Vec::new();
    if let Some(token) = binding.token.as_deref() {
        headers.push(json!({ "name": "Authorization", "value": format!("Bearer {token}") }));
    }
    let mut entry = json!({
        "type": "http",
        "name": WORKSPACE_MCP_SERVER_NAME,
        "url": binding.mcp_url,
        "headers": headers,
    });
    entry["_meta"] = json!({ META_SWARM_SANDBOX: true });
    entry
}

/// `session/new` parameters for one child.
///
/// The internal workspace entry (if any) comes first, then the caller's
/// declarations in their normalized order. No declaration and no workspace
/// keeps the request byte-identical to S4: the key is omitted rather than sent
/// as an empty array.
fn session_new_params(
    cwd: &str,
    meta: Value,
    workspace: Option<&SandboxMcpBinding>,
    tools: &[McpToolSpec],
) -> Value {
    let mut params = json!({ "cwd": cwd, "_meta": meta });
    let servers = mcp_servers(workspace, tools);
    if !servers.is_empty() {
        params["mcpServers"] = Value::Array(servers);
    }
    params
}

/// `session/load` parameters for a persisted session (S9a4b reattach).
///
/// Initial load is the only ACP entry that restores an existing session into a
/// fresh host; `cwd` and `mcpServers` are the setup fields that make the
/// restored environment executable again (`peri-acp-protocol §2.1`). The
/// session id is the persisted root id, not a new one: reattach must not mint
/// a second identity.
fn session_load_params(
    cwd: &str,
    root_session_id: &str,
    workspace: Option<&SandboxMcpBinding>,
    tools: &[McpToolSpec],
) -> Value {
    let mut params = json!({ "sessionId": root_session_id, "cwd": cwd });
    let servers = mcp_servers(workspace, tools);
    if !servers.is_empty() {
        params["mcpServers"] = Value::Array(servers);
    }
    params
}

fn mcp_servers(workspace: Option<&SandboxMcpBinding>, tools: &[McpToolSpec]) -> Vec<Value> {
    let mut servers: Vec<Value> =
        Vec::with_capacity(tools.len() + usize::from(workspace.is_some()));
    if let Some(binding) = workspace {
        servers.push(workspace_mcp_entry(binding));
    }
    servers.extend(tools.iter().map(mcp_server_entry));
    servers
}

/// One spawned host + negotiated session + its reader task.
pub struct AcpChild {
    client: Arc<MpscClientTransport>,
    host: AsyncMutex<Option<AcpHostHandle>>,
    reader: AsyncMutex<Option<JoinHandle<()>>>,
    shutdown: AsyncMutex<Option<ChildShutdown>>,
    session_id: String,
}

impl std::fmt::Debug for AcpChild {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpChild")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl AcpChild {
    /// Test-only child around an in-memory transport.
    ///
    /// Actor-service tests need the ACP delivery boundary (open, closed, or a
    /// server that never answers) without spawning a real host. The child keeps
    /// no host handle; only [`Self::respond_permission`] is exercised.
    #[cfg(test)]
    pub(crate) fn for_test(
        client: Arc<MpscClientTransport>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            client,
            host: AsyncMutex::new(None),
            reader: AsyncMutex::new(None),
            shutdown: AsyncMutex::new(None),
            session_id: session_id.into(),
        }
    }

    /// Assemble the host, spawn it, negotiate `initialize`, and create the session.
    ///
    /// `root_session_id` is reserved by the caller and passed through
    /// `_meta["peri.rootSessionId"]`; the host therefore owns the identity the
    /// Swarm row was written with (S1).
    pub async fn spawn(config: ChildConfig, log: Arc<ItemLog>) -> Result<Self, ChildError> {
        Self::build(config, log, false).await
    }

    /// Assemble a host and attach it to the **persisted** session id (S9a4b).
    ///
    /// This is `initialize` → `session/load` with the stored root id, `cwd` and
    /// `mcpServers`. Load restores the session and replays history; it does not
    /// run a turn, execute a queued input, or mint a new identity. Replay
    /// notifications are dropped by [`super::projection`], so `swarm_items`
    /// stays the single record.
    pub async fn attach(config: ChildConfig, log: Arc<ItemLog>) -> Result<Self, ChildError> {
        Self::build(config, log, true).await
    }

    async fn build(
        config: ChildConfig,
        log: Arc<ItemLog>,
        attach: bool,
    ) -> Result<Self, ChildError> {
        let cwd = config.cwd.to_string_lossy().into_owned();
        let source = build_config_source(&config)?;
        let peri_config = source.loaded_merged();
        let provider = LlmProvider::from_source(&source).ok_or(ChildError::ProviderUnavailable)?;
        let provider = match config.model.as_deref() {
            Some(model) if model != provider.model_name() => {
                provider.with_model_name(model.to_owned())
            }
            _ => provider,
        };

        let resources = open_session_resources(&config).await?;
        let (session_resources, session_store_shutdown) = resources.into_parts();

        // The session-wide mode stays `Bypass`; only the declared MCP bindings
        // are scoped into the approval channel. An empty set is observably the
        // pre-approval behavior.
        let permission_mode = SharedPermissionMode::new(PermissionMode::Bypass);
        permission_mode.set_mcp_approval_servers(escalation_servers(&config.mcp_tools));

        let host_config = assemble_server_config_with_capabilities(
            HostAssemblyInput {
                provider,
                peri_config: Arc::new(RwLock::new(peri_config)),
                config_source: source,
                permission_mode,
                session_resources,
                workspace_id: None,
                session_store_shutdown: Some(Box::new(session_store_shutdown)),
                cwd: cwd.clone(),
                // `design §0.4-5`: bare would only take over `workspace`, which
                // blocks the future remote workspace injection. The host is
                // observably bare because every optional capability below is off.
                bare: false,
                // S9a3a: the Bun cold proxy only declares the tool surface at
                // startup. This is the only deployment that opts into deferring
                // the remote workspace probes until the first real tool call.
                lazy_workspace: true,
                drive_cron_tick: false,
                builtin_assembly_mode: None,
                #[cfg(not(target_os = "emscripten"))]
                workspace_input: None,
                #[cfg(not(target_os = "emscripten"))]
                workspace_resources: None,
                #[cfg(not(target_os = "emscripten"))]
                workflow_input: None,
                builtin_closed: Default::default(),
                skills_face_closed: false,
                plugin_face_closed: false,
                prepared_plugins: None,
                session_mcp_servers: None,
                // S1 deployment opt-in: this host is trusted by the Swarm gateway.
                reserved_root_session_id: true,
            },
            HostCapabilities {
                builtin_mcp: false,
                stdio_mcp: false,
                cron: false,
                plugins: false,
                settings_hooks: false,
            },
        )
        .await;

        let (client_transport, server_transport) = mpsc_transport_pair();
        let client = Arc::new(client_transport);
        let host = spawn_acp_server(Arc::new(server_transport), host_config);

        let reader_client = Arc::clone(&client);
        let reader = tokio::spawn(async move {
            read_loop(reader_client, log).await;
        });

        let child = Self {
            client: Arc::clone(&client),
            host: AsyncMutex::new(Some(host)),
            reader: AsyncMutex::new(Some(reader)),
            shutdown: AsyncMutex::new(None),
            session_id: String::new(),
        };

        if let Err(error) = child.initialize().await {
            let shutdown = child.finish(Err(error)).await;
            return Err(shutdown);
        }

        let (method, params) = if attach {
            (
                "session/load",
                session_load_params(
                    &cwd,
                    &config.root_session_id,
                    config.workspace.as_ref(),
                    &config.mcp_tools,
                ),
            )
        } else {
            let mut meta = json!({ "peri.rootSessionId": config.root_session_id });
            if let Some(instructions) = config.instructions.as_deref() {
                meta["peri.instructions"] = json!(instructions);
            }
            (
                "session/new",
                session_new_params(&cwd, meta, config.workspace.as_ref(), &config.mcp_tools),
            )
        };
        let response = match client.send_request(method, params).await {
            Ok(response) => response,
            Err(error) => return Err(child.finish(Err(ChildError::rpc(error))).await),
        };
        let session_id = match response.get("sessionId").and_then(Value::as_str) {
            Some(session_id) => session_id.to_owned(),
            None if attach => config.root_session_id.clone(),
            None => {
                return Err(child
                    .finish(Err(ChildError::Local(format!(
                        "{method} returned no sessionId"
                    ))))
                    .await)
            }
        };
        if attach && session_id != config.root_session_id {
            return Err(child
                .finish(Err(ChildError::Local(format!(
                    "{method} returned session id {session_id} instead of the persisted {}",
                    config.root_session_id
                ))))
                .await);
        }
        let mut child = child;
        child.session_id = session_id;
        Ok(child)
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    async fn initialize(&self) -> Result<(), ChildError> {
        let mut caps = peri_acp_types::PeriCaps::all_enabled();
        // Activity/system-reminder streams have no Swarm projection; requesting
        // them only adds transport traffic (design §6.1 mapping table).
        caps.agent_activity = false;
        caps.system_reminder = false;
        caps.token_stats = false;
        let response = self
            .client
            .send_request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {
                        "fs": { "readTextFile": false, "writeTextFile": false },
                        "terminal": false,
                        "_meta": caps.to_agent_meta(),
                    },
                }),
            )
            .await
            .map_err(ChildError::rpc)?;
        if response
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .is_none()
        {
            return Err(ChildError::Local(
                "initialize returned no protocolVersion".to_owned(),
            ));
        }
        Ok(())
    }

    /// Run one `session/prompt` turn. `request_id` is echoed by the host in
    /// `peri/agent_event_done`, which is what binds the SSE terminal to this call.
    pub async fn prompt(&self, text: &str, request_id: &str) -> Result<PromptReply, ChildError> {
        let response = self
            .client
            .send_request(
                "session/prompt",
                json!({
                    "sessionId": self.session_id,
                    "requestId": request_id,
                    "prompt": [{ "type": "text", "text": text }],
                }),
            )
            .await
            .map_err(ChildError::rpc)?;
        let stop_reason = response
            .get("stopReason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn")
            .to_owned();
        let pending_tasks = response
            .get("_meta")
            .and_then(|meta| meta.get("peri"))
            .and_then(|peri| peri.get("pendingTasks"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        Ok(PromptReply {
            stop_reason,
            pending_tasks,
        })
    }

    /// Answer the still-open `session/request_permission` with its original
    /// JSON-RPC id. The id never leaves the child boundary; the public approval
    /// id is independent of it.
    pub async fn respond_permission(
        &self,
        acp_request_id: &Value,
        decision: ApprovalDecision,
    ) -> Result<(), ChildError> {
        let id: peri_acp::transport::types::RequestId =
            serde_json::from_value(acp_request_id.clone()).map_err(|error| {
                ChildError::Local(format!("approval request id is not a JSON-RPC id: {error}"))
            })?;
        self.client
            .send_response(id, Ok(decision.acp_response()))
            .await
            .map_err(ChildError::rpc)
    }

    /// `session/cancel` notification. Targeted when the turn identity is known:
    /// the host ignores a cancel whose `(requestId, generation)` was superseded
    /// (`host/notify.rs`), so a late cancel cannot stop the next turn.
    pub async fn cancel(&self, target: Option<&CancelTarget>) -> Result<(), ChildError> {
        let mut params = json!({ "sessionId": self.session_id });
        if let Some(target) = target {
            params["requestId"] = json!(target.request_id);
            params["generation"] = json!(target.generation);
        }
        self.client
            .send_notification("session/cancel", params)
            .await
            .map_err(ChildError::rpc)
    }

    /// `session/input/snapshot`: the queue generation used by enqueue/dispatch.
    pub async fn snapshot(&self) -> Result<Value, ChildError> {
        self.client
            .send_request(
                "session/input/snapshot",
                json!({ "sessionId": self.session_id }),
            )
            .await
            .map_err(ChildError::rpc)
    }

    /// `session/input/enqueue` for one user input (`inputId` is a UUID).
    pub async fn enqueue_input(
        &self,
        input_id: &str,
        generation: &str,
        command_id: &str,
        text: &str,
    ) -> Result<Value, ChildError> {
        self.client
            .send_request(
                "session/input/enqueue",
                json!({
                    "sessionId": self.session_id,
                    "generation": generation,
                    "commandId": command_id,
                    "inputId": input_id,
                    "content": text,
                    "originalDraft": text,
                }),
            )
            .await
            .map_err(ChildError::rpc)
    }

    /// `session/input/dispatch` for the inputs already enqueued.
    pub async fn dispatch_inputs(
        &self,
        generation: &str,
        command_id: &str,
        input_ids: &[String],
    ) -> Result<Value, ChildError> {
        self.client
            .send_request(
                "session/input/dispatch",
                json!({
                    "sessionId": self.session_id,
                    "generation": generation,
                    "commandId": command_id,
                    "inputIds": input_ids,
                }),
            )
            .await
            .map_err(ChildError::rpc)
    }

    /// Initial `session/load` (no cursor). Returns the raw host response, which
    /// carries `snapshotToken` and — when archived history exists — `nextCursor`.
    /// `limit` is forwarded as-is: the host owns the rule that it is only valid
    /// together with a cursor, and answers `-32602` otherwise.
    pub async fn load_initial(&self, limit: Option<u32>) -> Result<Value, ChildError> {
        let mut params = json!({ "sessionId": self.session_id });
        if let Some(limit) = limit {
            params["limit"] = json!(limit);
        }
        self.client
            .send_request("session/load", params)
            .await
            .map_err(ChildError::rpc)
    }

    /// Read-only history page. Requires the connection-negotiated
    /// `peri.historyPagination` capability; the host rejects it otherwise.
    pub async fn load_page(&self, cursor: &str, limit: Option<u32>) -> Result<Value, ChildError> {
        let mut params = json!({ "sessionId": self.session_id, "cursor": cursor });
        if let Some(limit) = limit {
            params["limit"] = json!(limit);
        }
        self.client
            .send_request("session/load", params)
            .await
            .map_err(ChildError::rpc)
    }

    /// Close client admission, join the reader, and consume the host shutdown.
    /// Repeated calls return the cached first report (the host report itself is
    /// stable after its own terminal observation).
    pub async fn shutdown(&self) -> ChildShutdown {
        let session_id = self.session_id.clone();
        let mut cached = self.shutdown.lock().await;
        if let Some(report) = cached.as_ref() {
            return report.clone();
        }
        let report = self.close_and_drain().await;
        let report = ChildShutdown {
            session_id,
            ..report
        };
        *cached = Some(report.clone());
        report
    }

    async fn close_and_drain(&self) -> ChildShutdown {
        self.client.close();
        if let Some(reader) = self.reader.lock().await.take() {
            if tokio::time::timeout(Duration::from_secs(5), reader)
                .await
                .is_err()
            {
                tracing::warn!("swarm ACP reader did not stop within 5s");
            }
        }
        let mut host = self.host.lock().await;
        let Some(mut handle) = host.take() else {
            return ChildShutdown {
                session_id: self.session_id.clone(),
                complete: false,
                report: "already_shutdown",
            };
        };
        let report = handle.shutdown().await;
        let (complete, tag) = describe_shutdown(&report);
        ChildShutdown {
            session_id: self.session_id.clone(),
            complete,
            report: tag,
        }
    }

    /// On a construction failure: drain the host and return the original error.
    async fn finish(&self, error: Result<(), ChildError>) -> ChildError {
        let report = self.shutdown().await;
        if !report.complete {
            tracing::warn!(
                session_id = %self.session_id,
                report = report.report,
                "swarm ACP child shutdown was incomplete after a construction failure"
            );
        }
        match error {
            Err(error) => error,
            Ok(()) => ChildError::Local("child construction failed".to_owned()),
        }
    }
}

fn describe_shutdown(report: &peri_acp::host::AcpHostShutdownReport) -> (bool, &'static str) {
    use peri_acp::host::AcpHostShutdownReport;
    match report {
        AcpHostShutdownReport::Complete => (true, "complete"),
        AcpHostShutdownReport::Incomplete => (false, "incomplete"),
        AcpHostShutdownReport::TelemetryFailed(_) => (false, "telemetry_failed"),
        AcpHostShutdownReport::TaskFailed { .. } => (false, "task_failed"),
    }
}

fn build_config_source(config: &ChildConfig) -> Result<Arc<ConfigSource>, ChildError> {
    match config.settings_json.as_deref() {
        Some(settings) => ConfigSource::load_injected_at(
            &config.cwd,
            peri_acp::provider::config_path(),
            settings.to_owned(),
        )
        .map(Arc::new)
        .map_err(|error| ChildError::Local(format!("injected settings rejected: {error}"))),
        None => ConfigSource::load_at(&config.cwd, peri_acp::provider::config_path())
            .map(Arc::new)
            .map_err(|error| ChildError::Local(format!("configuration load failed: {error}"))),
    }
}

/// Drain host notifications into the item log.
///
/// Server→client requests are refused explicitly: S4 advertises no filesystem,
/// terminal, or permission surface (`initialize` sets `fs.*=false`,
/// `terminal=false`), so a request here means a host path outside the S4
/// contract — answering keeps the host from blocking forever.
async fn read_loop(client: Arc<MpscClientTransport>, log: Arc<ItemLog>) {
    while let Some(message) = client.recv().await {
        match message {
            IncomingMessage::Notification { method, params } => {
                log.ingest(&method, &params).await;
            }
            IncomingMessage::Request { id, method, params } => {
                if method == "session/request_permission" {
                    let request_id = serde_json::to_value(&id).unwrap_or(Value::Null);
                    match log.begin_permission(&request_id, &params).await {
                        Ok(PermissionStart::Pending) => {
                            // Answered later through `AcpChild::respond_permission`
                            // with this same JSON-RPC id; the reader never blocks.
                        }
                        Ok(PermissionStart::Refuse) => {
                            tracing::warn!(
                                "swarm approval request has no open turn; refusing the call"
                            );
                            let refusal = Ok(json!({ "outcome": { "outcome": "cancelled" } }));
                            if let Err(error) = client.send_response(id, refusal).await {
                                tracing::warn!(%error, "swarm approval refusal delivery failed");
                            }
                        }
                        Ok(PermissionStart::AutoApprove) => {
                            if let Err(error) = client
                                .send_response(id, Ok(ApprovalDecision::Approve.acp_response()))
                                .await
                            {
                                tracing::warn!(%error, "swarm auto-approval delivery failed");
                            }
                        }
                        Err(error) => {
                            // The approval fact could not be persisted. Refuse the
                            // call explicitly instead of leaving the host waiting;
                            // this is a gateway failure, not a user rejection.
                            tracing::warn!(
                                %error,
                                "swarm approval could not be recorded; refusing the call"
                            );
                            let refusal = Ok(json!({ "outcome": { "outcome": "cancelled" } }));
                            if let Err(error) = client.send_response(id, refusal).await {
                                tracing::warn!(%error, "swarm approval refusal delivery failed");
                            }
                        }
                    }
                    continue;
                }
                let refusal = Err(AcpError::new(
                    -32601,
                    format!("{method} is not supported by the Swarm gateway"),
                ));
                if let Err(error) = client.send_response(id, refusal).await {
                    tracing::warn!(%error, "swarm ACP refusal delivery failed");
                }
            }
            IncomingMessage::Response { .. } => {}
        }
    }
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;
