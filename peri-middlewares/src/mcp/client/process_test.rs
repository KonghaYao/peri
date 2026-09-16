use super::*;
use std::time::Duration;

async fn wait_for_marker(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn leader_exit_before_pool_close_still_drains_descendants_and_stderr() {
    let cwd = tempfile::tempdir().unwrap();
    let pool = super::super::McpClientPool::new_pending();
    let mut command = tokio::process::Command::new("bash");
    command.current_dir(cwd.path()).args([
        "-c",
        "(while :; do printf x >> marker; sleep 0.02; done) </dev/null >/dev/null & exit 0",
    ]);
    let transport = pool
        .spawn_process_command(command, Some("fixture"))
        .unwrap();
    let owner = transport.process_owner();
    wait_for_marker(&cwd.path().join("marker")).await;
    owner.child.lock().await.wait().await.unwrap();
    assert!(
        !owner.tree.is_stopped(),
        "leader exit must leave the live descendant owned"
    );
    let report = tokio::time::timeout(Duration::from_secs(5), pool.shutdown())
        .await
        .unwrap();
    assert!(report.is_complete());
    assert!(owner.tree.is_stopped());
    assert!(
        owner.stderr.lock().await.is_none(),
        "Complete must join stderr even when inherited by a descendant"
    );
    drop(transport);
}

#[tokio::test]
async fn cancelled_handshake_keeps_process_owner_until_pool_cleanup() {
    let cwd = tempfile::tempdir().unwrap();
    let pool = super::super::McpClientPool::new_pending();
    let mut command = tokio::process::Command::new("bash");
    command.current_dir(cwd.path()).args([
        "-c",
        "(while :; do printf x >> marker; sleep 0.02; done) </dev/null >/dev/null & wait",
    ]);
    let transport = pool
        .spawn_process_command(command, Some("fixture"))
        .unwrap();
    let owner = transport.process_owner();
    wait_for_marker(&cwd.path().join("marker")).await;
    let result = super::super::transport::serve_client_auto(
        transport,
        None,
        None,
        &pool.capability_profile,
        Duration::from_millis(20),
    )
    .await;
    assert!(result.is_err());
    assert!(
        !owner.is_stopped(),
        "handshake timeout cannot discard actual drain ownership"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(5), pool.shutdown())
            .await
            .unwrap()
            .is_complete()
    );
    assert!(owner.is_stopped());
    assert!(owner.tree.is_stopped());
}
