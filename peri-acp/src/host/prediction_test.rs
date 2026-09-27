use std::{sync::Arc, time::Duration};

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::lock_or_shutdown;

/// [回归测试] 关停方持 sessions 锁等待任务收摊时，预测任务必须立刻放弃等锁。
///
/// 旧行为：任务继续排队等同一把锁；关停方（`session/close` →
/// `SessionEnvironment::shutdown`）在同一段代码里等它结束，双方互等，只能耗完
/// 协作宽限（5s）被强杀——Windows 的 print 用例因此顶穿预算。
#[tokio::test]
async fn test_lock_is_abandoned_once_shutdown_begins() {
    let sessions = Arc::new(Mutex::new(0usize));
    // 关停路径：先持锁，再取消 scope（`HostTaskOwner::begin_shutdown`），然后等任务。
    let _holder = sessions.lock().await;
    let shutdown = CancellationToken::new();
    let waiter = {
        let sessions = Arc::clone(&sessions);
        let shutdown = shutdown.clone();
        tokio::spawn(async move { lock_or_shutdown(&sessions, &shutdown).await.is_some() })
    };
    tokio::task::yield_now().await;
    shutdown.cancel();

    let acquired = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("关停开始后预测任务不得继续等 sessions 锁（会与关停方互等到强杀）");
    assert!(!acquired.unwrap(), "关停开始即放弃本轮预测");
}

/// 未关停时锁照常取到：放弃只针对关停，不是把所有预测都丢掉。
#[tokio::test]
async fn test_lock_is_taken_while_scope_is_open() {
    let sessions = Mutex::new(7usize);
    let guard = lock_or_shutdown(&sessions, &CancellationToken::new()).await;
    assert_eq!(*guard.expect("未关停时应取到锁"), 7);
}
