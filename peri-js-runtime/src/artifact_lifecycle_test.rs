use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tokio::sync::Notify;

struct PendingInstaller {
    started: Notify,
    exited: AtomicBool,
    staging: Mutex<Option<PathBuf>>,
}

#[async_trait::async_trait]
impl Installer for PendingInstaller {
    async fn install(&self, staging: &Path, cancel: &CancellationToken) -> std::io::Result<bool> {
        *self.staging.lock().unwrap() = Some(staging.to_path_buf());
        self.started.notify_one();
        cancel.cancelled().await;
        self.exited.store(true, Ordering::SeqCst);
        Err(std::io::ErrorKind::Interrupted.into())
    }
}

#[tokio::test]
async fn test_cancelled_install_drains_installer_removes_staging_and_does_not_fallback() {
    let home = tempfile::tempdir().unwrap();
    let installer = Arc::new(PendingInstaller {
        started: Notify::new(),
        exited: AtomicBool::new(false),
        staging: Mutex::new(None),
    });
    let cancel = CancellationToken::new();
    let task = tokio::spawn({
        let home = home.path().to_owned();
        let installer = installer.clone();
        let cancel = cancel.clone();
        async move { launch_in("missing-node", &home, installer.as_ref(), true, &cancel).await }
    });
    tokio::time::timeout(Duration::from_secs(1), installer.started.notified())
        .await
        .unwrap();
    let staging = installer.staging.lock().unwrap().clone().unwrap();
    assert!(staging.is_dir(), "installer启动前必须持有staging目录");
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(result, Err(JsRuntimeError::Cancelled)),
        "用户取消不能降级到npx"
    );
    assert!(
        installer.exited.load(Ordering::SeqCst),
        "返回前installer必须已退出"
    );
    assert!(!staging.exists(), "返回前staging必须清理");
    assert!(!prefix(home.path()).exists(), "取消不得发布不完整target");
    let lock = acquire_lock(
        prefix(home.path()).parent().unwrap(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    drop(lock);
}

#[tokio::test]
async fn test_cancelled_lock_waiter_exits_while_other_owner_keeps_lock() {
    let home = tempfile::tempdir().unwrap();
    let parent = home.path().join(".peri/ptc");
    let owner = acquire_lock(&parent, &CancellationToken::new())
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let waiting = acquire_lock(&parent, &cancel);
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut waiting)
            .await
            .is_err(),
        "第二个owner不能越过已持有的文件锁"
    );
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), &mut waiting)
        .await
        .expect("取消不能遗留等待lock_exclusive的blocking worker");
    assert!(matches!(result, Err(JsRuntimeError::Cancelled)));
    drop(owner);
    let next = acquire_lock(&parent, &CancellationToken::new())
        .await
        .unwrap();
    drop(next);
}
