use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use peri_acp_types::{
    dynamic_mcp::{DynamicMcpIncarnationId, DynamicMcpLogicalKey},
    ports::SecretResolveError,
};

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
    let (guard, store, _dir, _pool, _connection, credential_key) = credential_guard(key.clone());
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
    let (guard, store, _dir, _pool, _connection, credential_key) = credential_guard(key.clone());
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
