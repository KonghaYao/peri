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
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[cfg(unix)]
    use serial_test::serial;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use peri_acp_types::{
        dynamic_mcp::{DynamicMcpIncarnationId, DynamicMcpLogicalKey},
        ports::SecretResolveError,
    };

    struct LeakyResolver;

    #[async_trait::async_trait]
    impl SecretResolverPort for LeakyResolver {
        async fn resolve(
            &self,
            _reference: &SecretRef,
        ) -> Result<ResolvedSecret, SecretResolveError> {
            Err(SecretResolveError::Unavailable)
        }
    }

    use super::*;
    use crate::mcp::client::{ControlledMcpService, McpConnectionKey, OAuthStartDisposition};

    #[cfg(unix)]
    fn write_fixture(dir: &Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '%s' \"${PERI_DYNAMIC_SENTINEL-unset}\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[cfg(unix)]
    async fn fixture_output(
        program: &str,
        env: &HashMap<String, String>,
        cwd: Option<&str>,
    ) -> String {
        let output = dynamic_stdio_command(program, &[], env, cwd)
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn relative_fixture_starts_via_parent_path_after_environment_clear() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture(dir.path(), "dynamic-fixture");
        let original = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());
        let output = fixture_output("dynamic-fixture", &HashMap::new(), None).await;
        match original {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
        assert_eq!(output, "unset");
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn missing_or_empty_parent_path_uses_fixed_fallback() {
        let original = std::env::var_os("PATH");
        std::env::set_var("PATH", "");
        assert_eq!(
            dynamic_stdio_path_environment(),
            vec![("PATH".into(), dynamic_stdio_default_path().into())]
        );
        match original {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn unapproved_parent_sentinel_is_not_inherited() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = write_fixture(dir.path(), "dynamic-fixture");
        let original = std::env::var_os("PERI_DYNAMIC_SENTINEL");
        std::env::set_var("PERI_DYNAMIC_SENTINEL", "parent");
        let output = fixture_output(fixture.to_str().unwrap(), &HashMap::new(), None).await;
        match original {
            Some(value) => std::env::set_var("PERI_DYNAMIC_SENTINEL", value),
            None => std::env::remove_var("PERI_DYNAMIC_SENTINEL"),
        }
        assert_eq!(output, "unset");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn approved_path_overrides_runtime_path() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture(dir.path(), "dynamic-fixture");
        let env = HashMap::from([(
            "PATH".to_string(),
            dir.path().to_string_lossy().into_owned(),
        )]);
        assert_eq!(fixture_output("dynamic-fixture", &env, None).await, "unset");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn absolute_fixture_still_starts_with_cleared_environment() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = write_fixture(dir.path(), "dynamic-fixture");
        assert_eq!(
            fixture_output(fixture.to_str().unwrap(), &HashMap::new(), None).await,
            "unset"
        );
    }

    fn credential() -> rmcp::transport::auth::StoredCredentials {
        rmcp::transport::auth::StoredCredentials::new("client".into(), None, vec![], None)
    }

    fn credential_guard(
        instance: DynamicMcpInstanceKey,
    ) -> (
        DynamicOAuthCredentialGuard,
        Arc<FileCredentialStore>,
        tempfile::TempDir,
        Arc<McpClientPool>,
        McpConnectionKey,
        String,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileCredentialStore::with_path(
            dir.path().join("oauth_tokens.json"),
        ));
        let pool = Arc::new(McpClientPool::new_pending());
        let connection = McpConnectionKey::dynamic(instance.clone());
        let key = format!(
            "dynamic:{}:{}:{}",
            instance.logical.session_id,
            instance.incarnation_id.as_str(),
            instance.logical.server_name
        );
        assert_eq!(
            pool.reserve_oauth_flow_scoped(connection.clone(), "flow"),
            OAuthStartDisposition::Started
        );
        (
            DynamicOAuthCredentialGuard::new(
                Arc::clone(&pool),
                connection.clone(),
                Arc::clone(&store),
                key.clone(),
            ),
            store,
            dir,
            pool,
            connection,
            key,
        )
    }

    fn failing_credential_guard(
        instance: DynamicMcpInstanceKey,
    ) -> (
        DynamicOAuthCredentialGuard,
        Arc<McpClientPool>,
        McpConnectionKey,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FileCredentialStore::with_path(dir.path().to_path_buf()));
        let pool = Arc::new(McpClientPool::new_pending());
        let connection = McpConnectionKey::dynamic(instance);
        assert_eq!(
            pool.reserve_oauth_flow_scoped(connection.clone(), "flow"),
            OAuthStartDisposition::Started
        );
        (
            DynamicOAuthCredentialGuard::new(
                Arc::clone(&pool),
                connection.clone(),
                store,
                "credential".to_string(),
            ),
            pool,
            connection,
            dir,
        )
    }

    fn instance() -> DynamicMcpInstanceKey {
        DynamicMcpInstanceKey {
            logical: DynamicMcpLogicalKey {
                session_id: "session-a".to_string(),
                server_name: "example".to_string(),
            },
            incarnation_id: DynamicMcpIncarnationId::from_string("mcpinc_test"),
        }
    }

    #[tokio::test]
    async fn missing_and_unavailable_secrets_are_safely_redacted() {
        let secret = SecretRef::new("super-secret-reference").unwrap();
        for resolver in [
            &RejectingSecretResolver as &dyn SecretResolverPort,
            &LeakyResolver as &dyn SecretResolverPort,
        ] {
            let error = match resolver.resolve(&secret).await {
                Ok(_) => panic!("resolver unexpectedly returned a secret"),
                Err(error) => error,
            };
            let failure = secret_failure(error);
            let serialized = serde_json::to_string(&failure).unwrap();
            assert_eq!(failure.code, DynamicMcpErrorCode::SecretNotFound);
            assert!(!serialized.contains("super-secret-reference"));
            assert!(!serialized.contains("secret value"));
        }
    }

    #[tokio::test]
    async fn credential_guard_drop_clears_exact_instance_without_deleting_l2() {
        let l1 = instance();
        let mut l2 = l1.clone();
        l2.incarnation_id = DynamicMcpIncarnationId::from_string("mcpinc_l2");
        let (guard, store, _dir, pool, connection, l1_key) = credential_guard(l1);
        let l2_key = format!(
            "dynamic:{}:{}:{}",
            l2.logical.session_id,
            l2.incarnation_id.as_str(),
            l2.logical.server_name
        );
        store.save_server(&l1_key, credential()).await.unwrap();
        store.save_server(&l2_key, credential()).await.unwrap();

        drop(guard);

        assert!(store.load_server(&l1_key).await.unwrap().is_none());
        assert!(store.load_server(&l2_key).await.unwrap().is_some());
        assert!(pool.active_oauth_flow_scoped(&connection).is_none());
    }

    #[tokio::test]
    async fn committed_credential_guard_is_owned_until_active_close() {
        let key = instance();
        let (guard, store, _dir, _pool, _connection, credential_key) =
            credential_guard(key.clone());
        store
            .save_server(&credential_key, credential())
            .await
            .unwrap();
        let mut staged = StagedMcpConnection::without_service(key, Arc::new(empty_handle()));
        staged.oauth = Some(guard);
        let active = staged.commit();
        assert!(store.load_server(&credential_key).await.unwrap().is_some());

        active.close().await.unwrap();

        assert!(store.load_server(&credential_key).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn staged_drop_clears_real_file_credential() {
        let key = instance();
        let (guard, store, _dir, _pool, _connection, credential_key) =
            credential_guard(key.clone());
        store
            .save_server(&credential_key, credential())
            .await
            .unwrap();
        let mut staged = StagedMcpConnection::without_service(key, Arc::new(empty_handle()));
        staged.oauth = Some(guard);

        drop(staged);

        assert!(store.load_server(&credential_key).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn failed_credential_cleanup_still_revokes_flow_and_closes_service() {
        let key = instance();
        let (guard, pool, connection, _dir) = failing_credential_guard(key.clone());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let close_count = Arc::new(AtomicUsize::new(0));
        let service = McpServiceWrapper::Controlled(ControlledMcpService::new(
            entered_tx,
            Arc::clone(&release),
            Arc::clone(&close_count),
        ));
        let mut staged = StagedMcpConnection::with_service(key, Arc::new(empty_handle()), service);
        staged.oauth = Some(guard);
        let cleanup = tokio::spawn(async move { staged.cleanup().await });

        entered_rx.await.unwrap();
        assert!(pool.active_oauth_flow_scoped(&connection).is_none());
        assert_eq!(close_count.load(Ordering::SeqCst), 1);
        release.notify_waiters();
        let failure = cleanup.await.unwrap().unwrap_err();
        assert_eq!(failure.code, DynamicMcpErrorCode::ShutdownIncomplete);
    }

    #[tokio::test]
    async fn active_close_aggregates_credential_and_service_failures_and_is_idempotent() {
        let key = instance();
        let (guard, pool, connection, _dir) = failing_credential_guard(key.clone());
        let close_count = Arc::new(AtomicUsize::new(0));
        let service = McpServiceWrapper::Controlled(ControlledMcpService::timing_out(Arc::clone(
            &close_count,
        )));
        let mut staged = StagedMcpConnection::with_service(key, Arc::new(empty_handle()), service);
        staged.oauth = Some(guard);
        let active = staged.commit();

        let failure = active.close().await.unwrap_err();
        assert_eq!(failure.code, DynamicMcpErrorCode::ShutdownIncomplete);
        assert!(failure.safe_summary.contains("credential and service"));
        assert!(pool.active_oauth_flow_scoped(&connection).is_none());
        assert_eq!(close_count.load(Ordering::SeqCst), 1);

        let repeated = active.close().await.unwrap_err();
        assert_eq!(repeated.code, DynamicMcpErrorCode::ShutdownIncomplete);
        assert_eq!(close_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn staged_cleanup_closes_owned_service_before_failure_can_publish() {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let close_count = Arc::new(AtomicUsize::new(0));
        let service = McpServiceWrapper::Controlled(ControlledMcpService::new(
            entered_tx,
            Arc::clone(&release),
            Arc::clone(&close_count),
        ));
        let staged =
            StagedMcpConnection::with_service(instance(), Arc::new(empty_handle()), service);
        let cleanup = tokio::spawn(async move { staged.cleanup().await });
        entered_rx.await.unwrap();
        assert_eq!(close_count.load(Ordering::SeqCst), 1);
        release.notify_waiters();
        cleanup.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn dropped_staged_connection_cleanup_is_owner_tracked() {
        let (mut owner, spawner) = crate::mcp::McpTaskOwner::new();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let close_count = Arc::new(AtomicUsize::new(0));
        let service = McpServiceWrapper::Controlled(ControlledMcpService::new(
            entered_tx,
            Arc::clone(&release),
            Arc::clone(&close_count),
        ));
        let staged = StagedMcpConnection::with_service_and_spawner(
            instance(),
            Arc::new(empty_handle()),
            service,
            spawner,
        );

        drop(staged);
        entered_rx.await.unwrap();
        assert_eq!(owner.active_count(), 1);
        release.notify_waiters();
        owner.shutdown().await;
        assert_eq!(close_count.load(Ordering::SeqCst), 1);
    }
}
