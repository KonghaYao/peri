use std::io::BufRead;

use super::*;

#[tokio::test]
async fn account_named_host_has_independent_storage_and_persistent_host_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("credentials"), "host".into()).unwrap();
    let cancellation = CancellationToken::new();
    let host_id = store.host_id(&cancellation).await.unwrap();
    let lease = store.lease(&cancellation).await.unwrap();
    let mut record = StoredAccount::new(host_id.clone());
    record.subject = Some("synthetic-subject".into());
    lease.save(record).await.unwrap();
    drop(lease);
    let restarted = Store::new(dir.path().join("credentials"), "host".into()).unwrap();
    assert_eq!(restarted.host_id(&cancellation).await.unwrap(), host_id);
    let loaded = restarted
        .lease(&cancellation)
        .await
        .unwrap()
        .read()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.subject.as_deref(), Some("synthetic-subject"));
}

#[tokio::test]
async fn waiting_for_refresh_owner_is_cancelable() {
    let dir = tempfile::tempdir().unwrap();
    let first = Store::new(dir.path().join("credentials"), "one".into()).unwrap();
    let second = first.clone();
    let cancellation = CancellationToken::new();
    let _lease = first.lease(&CancellationToken::new()).await.unwrap();
    cancellation.cancel();
    assert!(matches!(
        second.lease(&cancellation).await,
        Err(AuthError::Cancelled)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn unsafe_permissions_and_symlink_records_fail_closed() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("credentials"), "account".into()).unwrap();
    let lease = store.lease(&CancellationToken::new()).await.unwrap();
    let target = dir.path().join("target");
    std::fs::write(&target, b"{}").unwrap();
    symlink(&target, &lease.path).unwrap();
    assert!(matches!(lease.read().await, Err(AuthError::Storage)));
    std::fs::remove_file(&lease.path).unwrap();
    std::fs::write(&lease.path, b"{}").unwrap();
    std::fs::set_permissions(&lease.path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(lease.read().await, Err(AuthError::Storage)));
}

#[test]
fn native_lock_excludes_a_separate_process_and_releases_on_exit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cross-process.lock");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "model_auth::store::tests::cross_process_lock_helper",
            "--nocapture",
        ])
        .env("PERI_SYNTHETIC_AUTH_LOCK", &path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = std::io::BufReader::new(stdout).lines();
    assert!(lines.any(|line| line.unwrap().contains("LOCK_ACQUIRED")));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    assert!(matches!(
        file.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    file.try_lock().unwrap();
}

#[test]
fn cross_process_lock_helper() {
    let Some(path) = std::env::var_os("PERI_SYNTHETIC_AUTH_LOCK") else {
        return;
    };
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    file.lock().unwrap();
    let mut output = std::io::stdout();
    output.write_all(b"LOCK_ACQUIRED\n").unwrap();
    output.flush().unwrap();
    let _ = std::io::stdin().read(&mut [0_u8; 1]);
}
