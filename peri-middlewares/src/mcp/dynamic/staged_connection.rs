use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};

use peri_acp_types::{
    dynamic_mcp::{
        CanonicalDynamicMcpConfig, CanonicalDynamicMcpTransport, DynamicMcpErrorCode,
        DynamicMcpFailure, DynamicMcpHeaderValue, DynamicMcpInstanceKey, DynamicMcpOperationState,
        ResolvedSecret, SecretRef,
    },
    ports::{SecretResolveError, SecretResolverPort},
};
use rmcp::model::{Resource, Tool};

use super::{
    super::{
        auth_store::FileCredentialStore,
        client::{
            build_authed_transport, serve_client_auto, ClientStatus, McpClientHandle,
            McpClientPool, McpServiceWrapper, OAuthStatus, SHUTDOWN_TIMEOUT,
        },
        config::OAuthConfig,
        oauth_flow::{OAuthFlowEvent, OAuthFlowManager},
        task_scope::{DynamicMcpTaskKind, McpTaskKey, McpTaskSpawner},
    },
    admission::DynamicMcpAdmissionGate,
};

pub struct RejectingSecretResolver;

pub struct EnvironmentSecretResolver;

#[async_trait::async_trait]
impl SecretResolverPort for EnvironmentSecretResolver {
    async fn resolve(&self, reference: &SecretRef) -> Result<ResolvedSecret, SecretResolveError> {
        std::env::var(reference.as_str())
            .map(ResolvedSecret::new)
            .map_err(|error| match error {
                std::env::VarError::NotPresent => SecretResolveError::NotFound,
                std::env::VarError::NotUnicode(_) => SecretResolveError::Unavailable,
            })
    }
}

#[async_trait::async_trait]
impl SecretResolverPort for RejectingSecretResolver {
    async fn resolve(&self, _reference: &SecretRef) -> Result<ResolvedSecret, SecretResolveError> {
        Err(SecretResolveError::NotFound)
    }
}

pub struct DynamicOAuthCredentialGuard {
    pool: Arc<McpClientPool>,
    connection: crate::mcp::client::McpConnectionKey,
    store: Arc<FileCredentialStore>,
    credential_key: String,
}

impl DynamicOAuthCredentialGuard {
    fn new(
        pool: Arc<McpClientPool>,
        connection: crate::mcp::client::McpConnectionKey,
        store: Arc<FileCredentialStore>,
        credential_key: String,
    ) -> Self {
        Self {
            pool,
            connection,
            store,
            credential_key,
        }
    }

    fn cleanup(&self) -> Result<(), DynamicMcpFailure> {
        self.pool.revoke_oauth_connection(&self.connection);
        self.store
            .clear_server_blocking(&self.credential_key)
            .map_err(|_| {
                DynamicMcpFailure::new(
                    DynamicMcpErrorCode::ShutdownIncomplete,
                    DynamicMcpOperationState::Draining,
                    "Dynamic MCP OAuth credential cleanup did not complete",
                )
            })
    }
}

impl Drop for DynamicOAuthCredentialGuard {
    fn drop(&mut self) {
        self.pool.revoke_oauth_connection(&self.connection);
        if self
            .store
            .clear_server_blocking(&self.credential_key)
            .is_err()
        {
            tracing::error!("dynamic MCP OAuth credential rollback failed during drop");
        }
    }
}

pub struct StagedMcpConnection {
    pub instance_key: DynamicMcpInstanceKey,
    pub handle: Arc<McpClientHandle>,
    pub gate: DynamicMcpAdmissionGate,
    service: Option<McpServiceWrapper>,
    cleanup_spawner: McpTaskSpawner,
    oauth: Option<DynamicOAuthCredentialGuard>,
}

impl StagedMcpConnection {
    #[cfg(test)]
    pub(crate) fn without_service(
        instance_key: DynamicMcpInstanceKey,
        handle: Arc<McpClientHandle>,
    ) -> Self {
        Self {
            instance_key,
            handle,
            gate: DynamicMcpAdmissionGate::new(),
            service: None,
            cleanup_spawner: McpTaskSpawner::closed(),
            oauth: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_service(
        instance_key: DynamicMcpInstanceKey,
        handle: Arc<McpClientHandle>,
        service: McpServiceWrapper,
    ) -> Self {
        Self {
            instance_key,
            handle,
            gate: DynamicMcpAdmissionGate::new(),
            service: Some(service),
            cleanup_spawner: McpTaskSpawner::closed(),
            oauth: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_service_and_spawner(
        instance_key: DynamicMcpInstanceKey,
        handle: Arc<McpClientHandle>,
        service: McpServiceWrapper,
        cleanup_spawner: McpTaskSpawner,
    ) -> Self {
        Self {
            instance_key,
            handle,
            gate: DynamicMcpAdmissionGate::new(),
            service: Some(service),
            cleanup_spawner,
            oauth: None,
        }
    }

    pub fn commit(mut self) -> ActiveMcpConnection {
        ActiveMcpConnection {
            instance_key: self.instance_key.clone(),
            handle: Arc::clone(&self.handle),
            gate: self.gate.clone(),
            service: tokio::sync::Mutex::new(self.service.take()),
            oauth: self.oauth.take(),
        }
    }

    pub async fn cleanup(mut self) -> Result<(), DynamicMcpFailure> {
        let oauth_failure = self.oauth.take().and_then(|oauth| oauth.cleanup().err());
        let service_failure = close_service(self.service.take()).await.err();
        shutdown_result(oauth_failure, service_failure)
    }
}

impl Drop for StagedMcpConnection {
    fn drop(&mut self) {
        let service = self.service.take();
        let oauth = self.oauth.take();
        if service.is_none() && oauth.is_none() {
            return;
        }
        drop(oauth);
        let key = McpTaskKey::dynamic(DynamicMcpTaskKind::StagedCleanup, &self.instance_key);
        let _ = self.cleanup_spawner.spawn(key, async move {
            if close_service(service).await.is_err() {
                tracing::error!("dynamic MCP staged service cleanup did not complete");
            }
        });
    }
}

pub struct ActiveMcpConnection {
    pub instance_key: DynamicMcpInstanceKey,
    pub handle: Arc<McpClientHandle>,
    pub gate: DynamicMcpAdmissionGate,
    service: tokio::sync::Mutex<Option<McpServiceWrapper>>,
    oauth: Option<DynamicOAuthCredentialGuard>,
}

impl ActiveMcpConnection {
    pub async fn close(&self) -> Result<(), DynamicMcpFailure> {
        let oauth_failure = self.oauth.as_ref().and_then(|oauth| oauth.cleanup().err());
        let service_failure = close_service(self.service.lock().await.take()).await.err();
        shutdown_result(oauth_failure, service_failure)
    }
}

fn shutdown_result(
    oauth_failure: Option<DynamicMcpFailure>,
    service_failure: Option<DynamicMcpFailure>,
) -> Result<(), DynamicMcpFailure> {
    match (oauth_failure, service_failure) {
        (None, None) => Ok(()),
        (Some(failure), None) | (None, Some(failure)) => Err(failure),
        (Some(_), Some(_)) => Err(DynamicMcpFailure::new(
            DynamicMcpErrorCode::ShutdownIncomplete,
            DynamicMcpOperationState::Draining,
            "Dynamic MCP OAuth credential and service cleanup did not complete",
        )),
    }
}

async fn close_service(service: Option<McpServiceWrapper>) -> Result<(), DynamicMcpFailure> {
    let Some(mut service) = service else {
        return Ok(());
    };
    match tokio::time::timeout(
        SHUTDOWN_TIMEOUT + Duration::from_secs(1),
        service.close_with_timeout(SHUTDOWN_TIMEOUT),
    )
    .await
    {
        Ok(Ok(Some(_))) => Ok(()),
        Ok(Ok(None)) | Ok(Err(_)) | Err(_) => Err(DynamicMcpFailure::new(
            DynamicMcpErrorCode::ShutdownIncomplete,
            DynamicMcpOperationState::Draining,
            "Dynamic MCP service cleanup did not complete",
        )),
    }
}

fn secret_failure(error: SecretResolveError) -> DynamicMcpFailure {
    let summary = match error {
        SecretResolveError::NotFound => "A referenced secret was not found",
        SecretResolveError::Denied => "Access to a referenced secret was denied",
        SecretResolveError::Unavailable => "The secret resolver is unavailable",
    };
    DynamicMcpFailure::new(
        DynamicMcpErrorCode::SecretNotFound,
        DynamicMcpOperationState::Starting,
        summary,
    )
}

async fn resolve_secret_map(
    values: &std::collections::BTreeMap<String, SecretRef>,
    resolver: &dyn SecretResolverPort,
) -> Result<HashMap<String, String>, DynamicMcpFailure> {
    let mut resolved = HashMap::with_capacity(values.len());
    for (name, reference) in values {
        let value = resolver.resolve(reference).await.map_err(secret_failure)?;
        resolved.insert(name.clone(), value.expose().to_string());
    }
    Ok(resolved)
}

async fn resolve_headers(
    values: &std::collections::BTreeMap<String, DynamicMcpHeaderValue>,
    resolver: &dyn SecretResolverPort,
) -> Result<HashMap<String, String>, DynamicMcpFailure> {
    let mut resolved = HashMap::with_capacity(values.len());
    for (name, value) in values {
        let value = match value {
            DynamicMcpHeaderValue::Literal(value) => value.clone(),
            DynamicMcpHeaderValue::Secret(reference) => resolver
                .resolve(reference)
                .await
                .map_err(secret_failure)?
                .expose()
                .to_string(),
        };
        resolved.insert(name.clone(), value);
    }
    Ok(resolved)
}

fn dynamic_stdio_command(
    program: &str,
    args: &[String],
    env: &HashMap<String, String>,
    cwd: Option<&str>,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .env_clear()
        .envs(dynamic_stdio_runtime_environment())
        .envs(env)
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(Path::new(cwd));
    }
    command
}

#[cfg(windows)]
fn dynamic_stdio_runtime_environment() -> Vec<(String, String)> {
    let mut environment = dynamic_stdio_path_environment();
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Ok(value) = std::env::var(name) {
            environment.push((name.into(), value));
        }
    }
    environment
}

#[cfg(not(windows))]
fn dynamic_stdio_runtime_environment() -> Vec<(String, String)> {
    dynamic_stdio_path_environment()
}

fn dynamic_stdio_path_environment() -> Vec<(String, String)> {
    let path = std::env::var("PATH")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| dynamic_stdio_default_path().into());
    vec![("PATH".into(), path)]
}

#[cfg(windows)]
fn dynamic_stdio_default_path() -> &'static str {
    r"C:\Windows\System32;C:\Windows"
}

#[cfg(not(windows))]
fn dynamic_stdio_default_path() -> &'static str {
    "/usr/local/bin:/usr/bin:/bin"
}

fn spawn_dynamic_stdio_transport(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
    cwd: Option<&str>,
) -> std::io::Result<rmcp::transport::child_process::TokioChildProcess> {
    use std::process::Stdio;

    let mut command = dynamic_stdio_command(command, args, env, cwd);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    rmcp::transport::child_process::TokioChildProcess::new(command)
}

pub async fn prepare_single_server(
    instance_key: DynamicMcpInstanceKey,
    flow_id: peri_acp_types::dynamic_mcp::DynamicMcpOperationId,
    config: &CanonicalDynamicMcpConfig,
    resolver: &dyn SecretResolverPort,
    cleanup_spawner: McpTaskSpawner,
    oauth_pool: Arc<McpClientPool>,
    progress: Arc<dyn Fn(DynamicMcpOperationState) + Send + Sync>,
) -> Result<StagedMcpConnection, DynamicMcpFailure> {
    let timeout = Duration::from_millis(config.timeout_ms);
    let mut oauth_lease = None;
    let connect_result = match &config.transport {
        CanonicalDynamicMcpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let env = resolve_secret_map(env, resolver).await?;
            let transport = spawn_dynamic_stdio_transport(command, args, &env, cwd.as_deref())
                .map_err(|_| {
                    DynamicMcpFailure::new(
                        DynamicMcpErrorCode::StartRejected,
                        DynamicMcpOperationState::Starting,
                        "Dynamic MCP stdio process could not be started",
                    )
                })?;
            serve_client_auto(
                transport,
                None,
                config.protocol_version.as_ref(),
                &oauth_pool.capability_profile,
                timeout,
            )
            .await
        }
        CanonicalDynamicMcpTransport::StreamableHttp { url, headers } => {
            let headers = resolve_headers(headers, resolver).await?;
            let connection = crate::mcp::client::McpConnectionKey::dynamic(instance_key.clone());
            let flow_id = flow_id.to_string();
            let server_name = instance_key.logical.server_name.clone();
            let credential_key = format!(
                "dynamic:{}:{}:{}",
                instance_key.logical.session_id,
                instance_key.incarnation_id.as_str(),
                server_name
            );
            match oauth_pool.reserve_oauth_flow_scoped(connection.clone(), &flow_id) {
                crate::mcp::client::OAuthStartDisposition::Started => {}
                _ => {
                    return Err(DynamicMcpFailure::new(
                        DynamicMcpErrorCode::AuthFailed,
                        DynamicMcpOperationState::Authorizing,
                        "Dynamic MCP OAuth flow could not be admitted",
                    ));
                }
            }
            let store = Arc::new(FileCredentialStore::new());
            let guard = DynamicOAuthCredentialGuard::new(
                Arc::clone(&oauth_pool),
                connection.clone(),
                Arc::clone(&store),
                credential_key.clone(),
            );
            progress(DynamicMcpOperationState::Authorizing);
            let callback_pool = Arc::clone(&oauth_pool);
            let callback_instance = instance_key.clone();
            let callback_flow = flow_id.clone();
            let callback: Arc<dyn Fn(OAuthFlowEvent) + Send + Sync> =
                Arc::new(move |event| match event {
                    OAuthFlowEvent::AuthorizationNeeded {
                        flow_id,
                        server_name,
                        authorization_url,
                        callback_tx,
                    } if flow_id == callback_flow => {
                        let Some(host_callback) = callback_pool.oauth_event_callback() else {
                            return;
                        };
                        host_callback(OAuthFlowEvent::DynamicAuthorizationNeeded {
                            instance: callback_instance.clone(),
                            flow_id,
                            server_name,
                            authorization_url,
                            callback_tx,
                        });
                    }
                    OAuthFlowEvent::AuthorizationFailed { .. } => {}
                    _ => {}
                });
            let mut manager = OAuthFlowManager::new_with_arc(Arc::clone(&store), callback);
            let auth_result = manager
                .run_oauth_flow_with_id(&flow_id, &credential_key, url, &OAuthConfig::default())
                .await;
            oauth_pool.release_oauth_flow_scoped(&connection, &flow_id);
            auth_result.map_err(|_| {
                DynamicMcpFailure::new(
                    DynamicMcpErrorCode::AuthFailed,
                    DynamicMcpOperationState::Authorizing,
                    "Dynamic MCP OAuth authorization failed",
                )
            })?;
            let auth_manager = manager
                .get_authorization_manager(&credential_key)
                .ok_or_else(|| {
                    DynamicMcpFailure::new(
                        DynamicMcpErrorCode::AuthFailed,
                        DynamicMcpOperationState::Authorizing,
                        "Dynamic MCP OAuth authorization did not complete",
                    )
                })?;
            progress(DynamicMcpOperationState::Connecting);
            oauth_lease = Some(guard);
            serve_client_auto(
                build_authed_transport(url, &headers, auth_manager),
                None,
                config.protocol_version.as_ref(),
                &oauth_pool.capability_profile,
                timeout,
            )
            .await
        }
    };
    let service = match connect_result {
        Err(_) => {
            return Err(DynamicMcpFailure::new(
                DynamicMcpErrorCode::ConnectTimeout,
                DynamicMcpOperationState::Connecting,
                "Dynamic MCP connection timed out",
            ));
        }
        Ok(Err(_)) => {
            return Err(DynamicMcpFailure::new(
                DynamicMcpErrorCode::InitializeFailed,
                DynamicMcpOperationState::Connecting,
                "Dynamic MCP initialization failed",
            ));
        }
        Ok(Ok(service)) => service,
    };
    let peer = service.peer().clone();
    let discovery = tokio::time::timeout(timeout, async {
        let tools = peer.list_all_tools().await?;
        let resources = list_all_resources(&peer).await?;
        Ok::<_, rmcp::service::ServiceError>((tools, resources))
    })
    .await;
    let (tools, resources) = match discovery {
        Ok(Ok(discovered)) => discovered,
        Ok(Err(_)) | Err(_) => {
            let staged = StagedMcpConnection {
                instance_key,
                handle: Arc::new(empty_handle()),
                gate: DynamicMcpAdmissionGate::new(),
                service: Some(service),
                cleanup_spawner: cleanup_spawner.clone(),
                oauth: oauth_lease.take(),
            };
            staged.cleanup().await?;
            return Err(DynamicMcpFailure::new(
                DynamicMcpErrorCode::ToolDiscoveryFailed,
                DynamicMcpOperationState::Discovering,
                "Dynamic MCP capability discovery failed",
            ));
        }
    };
    let name = instance_key.logical.server_name.clone();
    Ok(StagedMcpConnection {
        instance_key,
        handle: Arc::new(McpClientHandle {
            name,
            version: peer
                .peer_info()
                .and_then(|info| info.server_info.as_ref().map(|value| value.version.clone())),
            cache_version: None,
            peer: Some(peer.clone()),
            tools,
            resources,
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::None,
            source: None,
            url: None,
            channel_capable: false,
            skills_capable: super::super::client::peer_declares_skills(&peer),
        }),
        gate: DynamicMcpAdmissionGate::new(),
        service: Some(service),
        cleanup_spawner,
        oauth: oauth_lease,
    })
}

fn empty_handle() -> McpClientHandle {
    McpClientHandle {
        name: String::new(),
        version: None,
        cache_version: None,
        peer: None,
        tools: Vec::<Tool>::new(),
        resources: Vec::<Resource>::new(),
        status: ClientStatus::Disconnected,
        oauth_status: OAuthStatus::None,
        source: None,
        url: None,
        channel_capable: false,
        skills_capable: false,
    }
}

async fn list_all_resources(
    peer: &rmcp::service::Peer<rmcp::service::RoleClient>,
) -> Result<Vec<Resource>, rmcp::service::ServiceError> {
    let mut resources = Vec::new();
    let mut cursor = None;
    loop {
        let params = Some(rmcp::model::PaginatedRequestParams::default().with_cursor(cursor));
        let result = peer.list_resources(params).await?;
        resources.extend(result.resources);
        cursor = result.next_cursor;
        if cursor.is_none() {
            return Ok(resources);
        }
    }
}

#[cfg(test)]
#[path = "staged_connection_test.rs"]
mod tests;
