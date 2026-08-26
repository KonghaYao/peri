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

pub struct StagedMcpConnection {
    pub instance_key: DynamicMcpInstanceKey,
    pub handle: Arc<McpClientHandle>,
    pub gate: DynamicMcpAdmissionGate,
    service: Option<McpServiceWrapper>,
    cleanup_spawner: McpTaskSpawner,
    oauth: Option<(
        Arc<McpClientPool>,
        crate::mcp::client::McpConnectionKey,
        Arc<FileCredentialStore>,
        String,
    )>,
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
        if let Some((pool, connection, store, credential_key)) = &self.oauth {
            pool.revoke_oauth_connection(connection);
            let _ = store.clear_server(credential_key).await;
        }
        close_service(self.service.take()).await
    }
}

impl Drop for StagedMcpConnection {
    fn drop(&mut self) {
        let service = self.service.take();
        let oauth = self.oauth.take();
        if service.is_none() && oauth.is_none() {
            return;
        }
        if let Some((pool, connection, _, _)) = &oauth {
            pool.revoke_oauth_connection(connection);
        }
        let key = McpTaskKey::dynamic(DynamicMcpTaskKind::StagedCleanup, &self.instance_key);
        let _ = self.cleanup_spawner.spawn(key, async move {
            if let Some((_, _, store, credential_key)) = oauth {
                let _ = store.clear_server(&credential_key).await;
            }
            let _ = close_service(service).await;
        });
    }
}

pub struct ActiveMcpConnection {
    pub instance_key: DynamicMcpInstanceKey,
    pub handle: Arc<McpClientHandle>,
    pub gate: DynamicMcpAdmissionGate,
    service: tokio::sync::Mutex<Option<McpServiceWrapper>>,
    oauth: Option<(
        Arc<McpClientPool>,
        crate::mcp::client::McpConnectionKey,
        Arc<FileCredentialStore>,
        String,
    )>,
}

impl ActiveMcpConnection {
    pub async fn close(&self) -> Result<(), DynamicMcpFailure> {
        if let Some((pool, connection, store, credential_key)) = &self.oauth {
            pool.revoke_oauth_connection(connection);
            let _ = store.clear_server(credential_key).await;
        }
        close_service(self.service.lock().await.take()).await
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

fn spawn_dynamic_stdio_transport(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
    cwd: Option<&str>,
) -> std::io::Result<rmcp::transport::child_process::TokioChildProcess> {
    use std::process::Stdio;

    let mut command = tokio::process::Command::new(command);
    command
        .args(args)
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(Path::new(cwd));
    }
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
            serve_client_auto(transport, None, config.protocol_version.as_ref(), timeout).await
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
            let store = Arc::new(FileCredentialStore::new());
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
            oauth_lease = Some((Arc::clone(&oauth_pool), connection, store, credential_key));
            serve_client_auto(
                build_authed_transport(url, &headers, auth_manager),
                None,
                config.protocol_version.as_ref(),
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
    use crate::mcp::client::ControlledMcpService;

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
