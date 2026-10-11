use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use peri_acp_types::{
    dynamic_mcp::{DynamicMcpIncarnationId, DynamicMcpLogicalKey},
    oauth_credentials::{OAuthCredentialError, OAuthCredentialPort, OAuthCredentialResult},
    ports::SecretResolveError,
};

struct GatedCredentialClearPort {
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    calls: AtomicUsize,
    reject_clear: bool,
}

#[async_trait::async_trait]
impl OAuthCredentialPort for GatedCredentialClearPort {
    async fn load(&self, _: &str) -> OAuthCredentialResult<Option<String>> {
        Ok(None)
    }

    async fn save(&self, _: &str, _: &str) -> OAuthCredentialResult<()> {
        Ok(())
    }

    async fn clear(&self, _: &str) -> OAuthCredentialResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.add_permits(1);
        self.release.acquire().await.unwrap().forget();
        if self.reject_clear {
            Err(OAuthCredentialError::Unavailable)
        } else {
            Ok(())
        }
    }

    async fn clear_all(&self) -> OAuthCredentialResult<()> {
        Ok(())
    }

    async fn list(&self) -> OAuthCredentialResult<Vec<String>> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn explicit_oauth_cleanup_yields_and_does_not_repeat_clear_on_drop() {
    for (active_close, reject_clear) in [(false, false), (true, false), (false, true)] {
        let port = Arc::new(GatedCredentialClearPort {
            entered: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
            calls: AtomicUsize::new(0),
            reject_clear,
        });
        let client = OAuthCredentialClient::new(port.clone()).unwrap();
        let pool = Arc::new(McpClientPool::new_pending());
        pool.inject_oauth_credentials(client).unwrap();
        let key = instance();
        let connection = McpConnectionKey::dynamic(key.clone());
        assert_eq!(
            pool.reserve_oauth_flow_scoped(connection.clone(), "flow"),
            OAuthStartDisposition::Started,
        );
        let guard = DynamicOAuthCredentialGuard::new(
            pool.clone(),
            connection.clone(),
            pool.oauth_credentials().unwrap(),
            "credential".into(),
        );
        let mut staged = StagedMcpConnection::without_service(key, Arc::new(empty_handle()));
        staged.oauth = Some(guard);
        let cleanup = tokio::spawn(async move {
            if active_close {
                let active = staged.commit();
                let result = active.close().await;
                drop(active);
                result
            } else {
                staged.cleanup().await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), port.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        assert!(pool.active_oauth_flow_scoped(&connection).is_none());
        assert!(!cleanup.is_finished());
        port.release.add_permits(1);
        let result = tokio::time::timeout(Duration::from_secs(1), cleanup)
            .await
            .unwrap()
            .unwrap();
        if reject_clear {
            assert_eq!(
                result.unwrap_err().code,
                DynamicMcpErrorCode::ShutdownIncomplete
            );
        } else {
            result.unwrap();
        }
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    }
}

struct LeakyResolver;

#[async_trait::async_trait]
impl SecretResolverPort for LeakyResolver {
    async fn resolve(&self, _reference: &SecretRef) -> Result<ResolvedSecret, SecretResolveError> {
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
async fn fixture_output(program: &str, env: &HashMap<String, String>, cwd: Option<&str>) -> String {
    let output = dynamic_stdio_command(program, &[], env, cwd)
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[cfg(unix)]
const ENV_TEST_CHILD: &str = "PERI_DYNAMIC_STDIO_ENV_TEST_CHILD";

#[cfg(unix)]
fn is_env_test_child(name: &str) -> bool {
    std::env::var(ENV_TEST_CHILD).as_deref() == Ok(name)
}

/// 环境覆盖仅传给精确筛选的测试子进程；父测试进程的 PATH/HOME 不变。
/// serial_test 与 process_env 文件锁不是同一把锁，不能保护其他并行 spawn。
#[cfg(unix)]
fn run_env_test(name: &str, key: &str, value: Option<&std::ffi::OsStr>) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg(format!("mcp::dynamic::staged_connection::tests::{name}"))
        .args(["--nocapture", "--test-threads=1"])
        .env(ENV_TEST_CHILD, name)
        .env_remove("PERI_DYNAMIC_SENTINEL");
    match value {
        Some(value) => {
            command.env(key, value);
        }
        None => {
            command.env_remove(key);
        }
    }
    let output = command.output().expect("应能启动环境隔离测试子进程");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "环境测试子进程失败: {stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("1 passed;"),
        "精确筛选必须实际执行一个测试: {stdout}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn relative_fixture_starts_via_parent_path_after_environment_clear() {
    const NAME: &str = "relative_fixture_starts_via_parent_path_after_environment_clear";
    if is_env_test_child(NAME) {
        let output = fixture_output("dynamic-fixture", &HashMap::new(), None).await;
        assert_eq!(output, "unset");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path(), "dynamic-fixture");
    run_env_test(NAME, "PATH", Some(dir.path().as_os_str()));
}

#[derive(Default)]
struct MemoryOAuthCredentialPort {
    records: parking_lot::Mutex<std::collections::HashMap<String, String>>,
    reject_clear: bool,
}

#[async_trait::async_trait]
impl OAuthCredentialPort for MemoryOAuthCredentialPort {
    async fn load(&self, key: &str) -> OAuthCredentialResult<Option<String>> {
        Ok(self.records.lock().get(key).cloned())
    }

    async fn save(&self, key: &str, credentials: &str) -> OAuthCredentialResult<()> {
        self.records
            .lock()
            .insert(key.to_string(), credentials.to_string());
        Ok(())
    }

    async fn clear(&self, key: &str) -> OAuthCredentialResult<()> {
        if self.reject_clear {
            return Err(OAuthCredentialError::Unavailable);
        }
        self.records.lock().remove(key);
        Ok(())
    }

    async fn clear_all(&self) -> OAuthCredentialResult<()> {
        self.records.lock().clear();
        Ok(())
    }

    async fn list(&self) -> OAuthCredentialResult<Vec<String>> {
        Ok(self.records.lock().keys().cloned().collect())
    }
}

fn memory_client() -> crate::mcp::auth_store::OAuthCredentialClient {
    crate::mcp::auth_store::OAuthCredentialClient::new(std::sync::Arc::new(
        MemoryOAuthCredentialPort::default(),
    ))
    .unwrap()
}

fn failing_clear_client() -> crate::mcp::auth_store::OAuthCredentialClient {
    crate::mcp::auth_store::OAuthCredentialClient::new(std::sync::Arc::new(
        MemoryOAuthCredentialPort {
            reject_clear: true,
            ..Default::default()
        },
    ))
    .unwrap()
}

#[cfg(unix)]
#[test]
fn missing_or_empty_parent_path_uses_fixed_fallback() {
    const NAME: &str = "missing_or_empty_parent_path_uses_fixed_fallback";
    if is_env_test_child(NAME) {
        assert_eq!(
            dynamic_stdio_path_environment(),
            vec![("PATH".into(), dynamic_stdio_default_path().into())]
        );
        return;
    }
    run_env_test(NAME, "PATH", Some(std::ffi::OsStr::new("")));
    run_env_test(NAME, "PATH", None);
}

#[cfg(unix)]
#[tokio::test]
async fn unapproved_parent_sentinel_is_not_inherited() {
    const NAME: &str = "unapproved_parent_sentinel_is_not_inherited";
    if !is_env_test_child(NAME) {
        run_env_test(
            NAME,
            "PERI_DYNAMIC_SENTINEL",
            Some(std::ffi::OsStr::new("parent")),
        );
        return;
    }
    assert_eq!(std::env::var("PERI_DYNAMIC_SENTINEL").unwrap(), "parent");
    let dir = tempfile::tempdir().unwrap();
    let fixture = write_fixture(dir.path(), "dynamic-fixture");
    let output = fixture_output(fixture.to_str().unwrap(), &HashMap::new(), None).await;
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
    OAuthCredentialClient,
    Arc<McpClientPool>,
    McpConnectionKey,
    String,
) {
    let store = memory_client();
    let pool = Arc::new(McpClientPool::new_pending());
    pool.inject_oauth_credentials(store).unwrap();
    let store = pool.oauth_credentials().unwrap();
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
            store.clone(),
            key.clone(),
        ),
        store,
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
) {
    let store = failing_clear_client();
    let pool = Arc::new(McpClientPool::new_pending());
    pool.inject_oauth_credentials(store).unwrap();
    let store = pool.oauth_credentials().unwrap();
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

/// 动态连接 handle 的排障 URL：HTTP 记录配置 URL，stdio 保持 None
/// （回归：HTTP 成功路径曾填 None，ACP/TUI 详情 URL 恒显示空占位符）。
#[test]
fn configured_transport_url_keeps_http_url_and_skips_stdio() {
    let http = CanonicalDynamicMcpConfig {
        transport: CanonicalDynamicMcpTransport::StreamableHttp {
            url: "https://example/mcp".into(),
            headers: Default::default(),
        },
        timeout_ms: 1_000,
        subscriptions: None,
    };
    assert_eq!(
        configured_transport_url(&http).as_deref(),
        Some("https://example/mcp")
    );
    let stdio = CanonicalDynamicMcpConfig {
        transport: CanonicalDynamicMcpTransport::Stdio {
            command: "node".into(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        },
        timeout_ms: 1_000,
        subscriptions: None,
    };
    assert_eq!(configured_transport_url(&stdio), None);
}

#[tokio::test]
async fn dynamic_oauth_without_injection_fails_before_flow_admission() {
    for closing in [false, true] {
        let pool = Arc::new(McpClientPool::new_pending());
        if closing {
            pool.begin_shutdown();
        }
        let key = instance();
        let connection = McpConnectionKey::dynamic(key.clone());
        let config = CanonicalDynamicMcpConfig {
            transport: CanonicalDynamicMcpTransport::StreamableHttp {
                url: "https://example/mcp".into(),
                headers: Default::default(),
            },
            timeout_ms: 1_000,
            subscriptions: None,
        };
        let result = prepare_single_server(
            key,
            Default::default(),
            &config,
            &RejectingSecretResolver,
            McpTaskSpawner::closed(),
            pool.clone(),
            Arc::new(|_| panic!("missing credentials must fail before authorization")),
        )
        .await;
        let failure = match result {
            Ok(_) => panic!("missing credentials unexpectedly succeeded"),
            Err(failure) => failure,
        };
        assert_eq!(failure.code, DynamicMcpErrorCode::AuthFailed);
        assert!(failure.safe_summary.contains("not injected"));
        assert!(pool.active_oauth_flow_scoped(&connection).is_none());
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
    let (guard, store, pool, connection, l1_key) = credential_guard(l1);
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
    let (guard, store, _pool, _connection, credential_key) = credential_guard(key.clone());
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
async fn staged_drop_clears_injected_credential() {
    let key = instance();
    let (guard, store, _pool, _connection, credential_key) = credential_guard(key.clone());
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
    let (guard, pool, connection) = failing_credential_guard(key.clone());
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
async fn active_close_aggregates_failures_and_retries_the_original_service() {
    let key = instance();
    let (guard, pool, connection) = failing_credential_guard(key.clone());
    let close_count = Arc::new(AtomicUsize::new(0));
    let service =
        McpServiceWrapper::Controlled(ControlledMcpService::timing_out(Arc::clone(&close_count)));
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
    assert_eq!(close_count.load(Ordering::SeqCst), 2);
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
    let staged = StagedMcpConnection::with_service(instance(), Arc::new(empty_handle()), service);
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

#[cfg(unix)]
const PROCESS_FIXTURE: &str = r#"
const fs = require('node:fs');
const child = require('node:child_process').spawn(process.execPath, ['-e', `
  const fs = require('node:fs');
  fs.writeFileSync('child-started', process.cwd());
  setInterval(() => fs.appendFileSync('heartbeat', 'x'), 5);
`], { stdio: 'ignore' });
const readline = require('node:readline').createInterface({ input: process.stdin });
readline.on('line', line => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  // Legacy servers must reject discovery so Auto can fall back to initialize.
  if (!['initialize', 'tools/list', 'resources/list', 'ping'].includes(request.method)) {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id,
      error: { code: -32601, message: 'Method not found' } }) + '\n');
    return;
  }
  let result = {};
  if (request.method === 'initialize') result = {
    protocolVersion: '2025-11-25', capabilities: {},
    serverInfo: { name: 'dynamic-process-fixture', version: '1' },
  };
  if (request.method === 'tools/list') result = { tools: [] };
  if (request.method === 'resources/list') result = { resources: [] };
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
});
"#;

#[cfg(unix)]
#[tokio::test]
async fn dynamic_process_uses_session_cwd_and_close_drains_its_descendant() {
    let fixture = tempfile::tempdir().unwrap();
    for name in ["worktree a", "worktree b"] {
        let cwd = fixture.path().join(name);
        std::fs::create_dir(&cwd).unwrap();
        std::fs::write(cwd.join("server.js"), PROCESS_FIXTURE).unwrap();
        let (mut tasks, spawner) = crate::mcp::McpTaskOwner::new();
        let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner.clone()));
        pool.bind_execution_cwd(&cwd).unwrap();
        let config = CanonicalDynamicMcpConfig {
            transport: CanonicalDynamicMcpTransport::Stdio {
                command: "node".into(),
                args: vec!["server.js".into()],
                env: Default::default(),
                cwd: None,
            },
            timeout_ms: 5_000,
            subscriptions: None,
        };
        let staged = prepare_single_server(
            instance(),
            Default::default(),
            &config,
            &RejectingSecretResolver,
            spawner,
            pool.clone(),
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
        let active = staged.commit();
        assert!(active.handle.url.is_none());
        tokio::time::timeout(Duration::from_secs(5), async {
            while !cwd.join("heartbeat").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join("child-started")).unwrap(),
            std::fs::canonicalize(&cwd).unwrap().to_str().unwrap()
        );
        assert!(!active.process.as_ref().unwrap().is_stopped());
        tokio::time::timeout(Duration::from_secs(5), active.close())
            .await
            .unwrap()
            .unwrap();
        assert!(active.process.as_ref().unwrap().is_stopped());
        assert!(active.service.lock().await.is_none());
        assert!(pool.shutdown().await.is_complete());
        tasks.shutdown().await;
    }
}

/// 最小动态 HTTP MCP fixture：POST /mcp 应答 initialize / tools/list /
/// resources/list，拒绝 server/discover 让 Auto 回退 legacy initialize；
/// 其余请求（OAuth 端点探测与 well-known 发现）一律 404，使 rmcp 的
/// 元数据发现回退到 legacy endpoint 推导，不发起真实授权。
async fn serve_dynamic_http_fixture(listener: tokio::net::TcpListener) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut request = Vec::new();
        let mut header_end = 0;
        loop {
            let mut chunk = [0_u8; 4096];
            let Ok(size) = socket.read(&mut chunk).await else {
                break;
            };
            if size == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..size]);
            if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                header_end = end + 4;
                break;
            }
        }
        if header_end == 0 {
            continue;
        }
        let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
        if !headers.starts_with("POST /mcp ") {
            let _ = socket
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let content_length: usize = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse().ok())
            })
            .unwrap_or(0);
        while request.len() < header_end + content_length {
            let mut chunk = [0_u8; 4096];
            let Ok(size) = socket.read(&mut chunk).await else {
                break;
            };
            if size == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..size]);
        }
        if request.len() < header_end + content_length {
            continue;
        }
        let Ok(message) = serde_json::from_slice::<serde_json::Value>(
            &request[header_end..header_end + content_length],
        ) else {
            continue;
        };
        let method = message
            .get("method")
            .and_then(|method| method.as_str())
            .unwrap_or_default();
        let body = if message.get("id").is_none() {
            None
        } else {
            let payload = match method {
                "initialize" => serde_json::json!({
                    "jsonrpc": "2.0", "id": message["id"],
                    "result": {
                        "protocolVersion": "2025-03-26",
                        "capabilities": {"tools": {}, "resources": {}},
                        "serverInfo": {"name": "dynamic-http-fixture", "version": "1.0.0"},
                    },
                }),
                "tools/list" => serde_json::json!({
                    "jsonrpc": "2.0", "id": message["id"],
                    "result": {"tools": [{
                        "name": "fixture-tool",
                        "description": "Dynamic HTTP fixture tool",
                        "inputSchema": {"type": "object", "properties": {}},
                    }]},
                }),
                "resources/list" => serde_json::json!({
                    "jsonrpc": "2.0", "id": message["id"],
                    "result": {"resources": []},
                }),
                "ping" => serde_json::json!({"jsonrpc": "2.0", "id": message["id"], "result": {}}),
                other => serde_json::json!({
                    "jsonrpc": "2.0", "id": message["id"],
                    "error": {"code": -32601, "message": format!("Method not found: {other}")},
                }),
            };
            Some(payload.to_string())
        };
        let response = match &body {
            None => {
                "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
            }
            Some(body) => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        };
        let _ = socket.write_all(response.as_bytes()).await;
    }
}

/// 动态 HTTP 成功路径的 URL 投影：初始化完成后 committed handle 记录配置 URL
/// （回归：HTTP 成功路径曾填 None，ACP/TUI 详情 URL 恒显示空占位符）。
#[tokio::test]
async fn dynamic_http_success_projects_configured_url_into_committed_handle() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let fixture = tokio::spawn(serve_dynamic_http_fixture(listener));

    let (mut tasks, spawner) = crate::mcp::McpTaskOwner::new();
    let key = instance();
    let client = memory_client();
    let credentials_key = format!(
        "dynamic:{}:{}:{}",
        key.logical.session_id,
        key.incarnation_id.as_str(),
        key.logical.server_name
    );
    // 预置凭证走 initialize_from_store 快速路径，跳过交互授权直达连接阶段。
    let token: rmcp::transport::auth::OAuthTokenResponse =
        serde_json::from_value(serde_json::json!({
            "access_token": "fixture-access-token",
            "token_type": "bearer",
        }))
        .unwrap();
    client
        .save_server(
            &credentials_key,
            rmcp::transport::auth::StoredCredentials::new(
                "fixture-client".into(),
                Some(token),
                vec![],
                None,
            ),
        )
        .await
        .unwrap();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner.clone()));
    pool.inject_oauth_credentials(client).unwrap();
    let config = CanonicalDynamicMcpConfig {
        transport: CanonicalDynamicMcpTransport::StreamableHttp {
            url: url.clone(),
            headers: Default::default(),
        },
        timeout_ms: 5_000,
        subscriptions: None,
    };
    let staged = tokio::time::timeout(
        Duration::from_secs(10),
        prepare_single_server(
            key,
            Default::default(),
            &config,
            &RejectingSecretResolver,
            spawner,
            pool.clone(),
            Arc::new(|_| {}),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let active = staged.commit();
    assert!(matches!(active.handle.status, ClientStatus::Connected));
    assert_eq!(active.handle.url.as_deref(), Some(url.as_str()));
    tokio::time::timeout(Duration::from_secs(5), active.close())
        .await
        .unwrap()
        .unwrap();
    fixture.abort();
    tokio::time::timeout(Duration::from_secs(5), pool.shutdown())
        .await
        .unwrap();
    tasks.shutdown().await;
}
