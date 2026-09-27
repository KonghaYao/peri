//! 远程 adapter 的关闭语义（离线装配，不联网；夹具见 `recovery_fixture_test.rs`）。
//!
//! 关闭有两件事必须同时成立：**连接不再服务业务读**，以及**真实资源的关闭进度不被丢弃**。
//! 连接一开始关闭就离开业务路径（业务读一律失败、不重连），但直到真实关闭成功返回之前它都
//! 被保留在关闭句柄里——失败或在途关闭被丢弃都可重试，重试关的是同一条连接（不新建连接），
//! 只有成功才算确认关闭，确认之后幂等。
//!
//! SDK 事实（`turso_serverless` 0.1.3 源码）：`Connection::close` 恒返回 `Ok(())`——它只把
//! 本地会话流标记复位，远端 `StreamRequest::Close` 的错误被显式忽略。因此这里的「关闭成功」
//! **不能**证明服务端连接已释放，能证明的只有：本机传输面走完了关闭、且本次关闭被记为确认。

use std::sync::atomic::Ordering;
use std::time::Duration;

use peri_acp_types::session_resources::SessionResourceErrorKind;
use peri_acp_types::thread::ThreadId;

use super::mutation::StoreAccess;
use super::recovery_fixture_tests::{Harness, ABANDON_AFTER};
use crate::sessions::data::SessionDataPort;

#[tokio::test]
async fn a_closed_adapter_never_reconnects() {
    // 确认关闭：连接被真正关闭，之后的读取既失败也不再建新连接。
    let confirmed = Harness::open(StoreAccess::ReadWrite).await;
    let id: ThreadId = "session-under-test".to_owned();
    confirmed
        .adapter
        .close()
        .await
        .expect("closing the only connection");
    let closed_error = confirmed
        .adapter
        .load_binding(&id)
        .await
        .expect_err("a closed adapter has no connection to read with");
    assert!(matches!(
        closed_error.kind(),
        SessionResourceErrorKind::Internal { .. }
    ));
    assert_eq!(
        confirmed.backend.connections(),
        1,
        "closing is final: no replacement connection is built"
    );
    assert_eq!(
        confirmed.backend.close_attempts(),
        vec![1],
        "the real connection was closed exactly once"
    );
    confirmed
        .adapter
        .close()
        .await
        .expect("a confirmed close is idempotent");
    assert_eq!(
        confirmed.backend.close_attempts(),
        vec![1],
        "confirming again does not close the real connection a second time"
    );

    // 关闭失败（连接被保留在关闭句柄里、未确认）：业务读不复活，也没有重连。
    let failed = Harness::open(StoreAccess::ReadWrite).await;
    failed.backend.fail_close.store(true, Ordering::SeqCst);
    assert!(failed.adapter.close().await.is_err());
    assert!(matches!(
        failed
            .adapter
            .load_binding(&id)
            .await
            .expect_err("business reads are ruled out once closing started")
            .kind(),
        SessionResourceErrorKind::Internal { .. }
    ));
    assert_eq!(
        failed.backend.connections(),
        1,
        "a failed close never rebuilds a connection"
    );
    assert_eq!(failed.backend.close_attempts(), vec![1]);
    assert_eq!(failed.backend.close_successes(), 0);
}

/// 关闭失败一次之后，重试关的是**同一条被保留的连接**，成功才算确认。
#[tokio::test]
async fn a_failed_close_is_retried_on_the_retained_connection() {
    let harness = Harness::open(StoreAccess::ReadWrite).await;
    let id: ThreadId = "session-under-test".to_owned();

    harness.backend.fail_close.store(true, Ordering::SeqCst);
    assert!(
        harness.adapter.close().await.is_err(),
        "the real close failed: the close is not confirmed"
    );
    assert_eq!(harness.backend.close_attempts(), vec![1]);
    assert_eq!(harness.backend.close_successes(), 0);

    // 关闭中：业务读如实失败，而且不重连（连接不因关闭失败被丢掉，也不被重建）。
    assert!(matches!(
        harness
            .adapter
            .load_binding(&id)
            .await
            .expect_err("business reads are ruled out once closing started")
            .kind(),
        SessionResourceErrorKind::Internal { .. }
    ));
    assert_eq!(harness.backend.connections(), 1);

    // 远端这一次真的关掉了：重试在同一第 1 条连接上成功——这才是确认关闭。
    harness.backend.fail_close.store(false, Ordering::SeqCst);
    harness
        .adapter
        .close()
        .await
        .expect("the retained connection closes on retry");
    assert_eq!(
        harness.backend.close_attempts(),
        vec![1, 1],
        "the retry closes the same retained connection, not a new one"
    );
    assert_eq!(harness.backend.close_successes(), 1);
    assert_eq!(harness.backend.connections(), 1);

    harness
        .adapter
        .close()
        .await
        .expect("a confirmed close stays idempotent");
    assert_eq!(
        harness.backend.close_attempts(),
        vec![1, 1],
        "confirming again does not close anything twice"
    );
    assert!(harness.adapter.load_binding(&id).await.is_err());
    assert_eq!(harness.backend.connections(), 1);
}

/// 在途关闭被丢弃（超时/取消）不丢唯一句柄：重试继续关同一条连接，不新建连接。
#[tokio::test]
async fn a_cancelled_close_is_retried_on_the_retained_connection() {
    let harness = Harness::open(StoreAccess::ReadWrite).await;
    let id: ThreadId = "session-under-test".to_owned();

    // 在途关闭被丢弃：连接已经离开业务路径，但真实资源不能跟着 future 一起消失。
    harness.backend.hold_close.store(true, Ordering::SeqCst);
    let cancelled = tokio::time::timeout(ABANDON_AFTER, harness.adapter.close()).await;
    assert!(
        cancelled.is_err(),
        "the close was expected to be abandoned in flight"
    );
    assert_eq!(harness.backend.close_attempts(), vec![1]);
    assert_eq!(harness.backend.close_successes(), 0);

    // 关闭中：业务读如实失败，也没有为了「补一条可用连接」去重连。
    assert!(matches!(
        harness
            .adapter
            .load_binding(&id)
            .await
            .expect_err("business reads are ruled out once closing started")
            .kind(),
        SessionResourceErrorKind::Internal { .. }
    ));
    assert_eq!(harness.backend.connections(), 1);

    // 放行被丢弃的那次尝试之后重试：关的仍是第 1 条被保留的连接，且没有新建连接。
    harness.backend.hold_close.store(false, Ordering::SeqCst);
    harness.backend.close_release.notify_one();
    harness
        .adapter
        .close()
        .await
        .expect("the retry closes the retained connection");
    assert_eq!(harness.backend.close_attempts(), vec![1, 1]);
    assert_eq!(harness.backend.close_successes(), 1);
    assert_eq!(harness.backend.connections(), 1);

    harness
        .adapter
        .close()
        .await
        .expect("a confirmed close stays idempotent");
    assert_eq!(harness.backend.close_attempts(), vec![1, 1]);
    assert_eq!(harness.backend.connections(), 1);
}

/// 并发关闭：都走同一个关闭句柄，成功幂等，真实连接只被关一次；确认之后不重连。
#[tokio::test]
async fn concurrent_closes_confirm_once_and_close_the_connection_once() {
    let harness = Harness::open(StoreAccess::ReadWrite).await;
    let id: ThreadId = "session-under-test".to_owned();

    // 第一次真实关闭停在传输里：其余调用必须共享同一个句柄上的同一次真实关闭。
    harness.backend.hold_close.store(true, Ordering::SeqCst);
    let release = async {
        // 让其余调用先走到同一个关闭句柄上，再放行真实关闭。
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        harness.backend.hold_close.store(false, Ordering::SeqCst);
        harness.backend.close_release.notify_one();
    };
    let closes = async {
        tokio::join!(
            harness.adapter.close(),
            harness.adapter.close(),
            harness.adapter.close(),
            harness.adapter.close(),
            release,
        )
    };
    let (first, second, third, fourth, ()) = tokio::time::timeout(Duration::from_secs(5), closes)
        .await
        .expect("concurrent closes complete");
    for confirmed in [first, second, third, fourth] {
        confirmed.expect("every concurrent close reports the confirmed shutdown");
    }
    assert_eq!(
        harness.backend.close_attempts(),
        vec![1],
        "the real connection is closed exactly once under concurrency"
    );
    assert_eq!(harness.backend.close_successes(), 1);
    assert_eq!(harness.backend.connections(), 1);
    assert!(harness.adapter.load_binding(&id).await.is_err());
    assert_eq!(
        harness.backend.connections(),
        1,
        "a confirmed close never reconnects"
    );
}
