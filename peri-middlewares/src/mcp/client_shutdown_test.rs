use super::*;

#[tokio::test]
async fn test_shutdown_drops_pending_oauth_callbacks() {
    let pool = McpClientPool::new_empty();
    assert_eq!(
        pool.reserve_oauth_flow("server", "flow"),
        OAuthStartDisposition::Started
    );
    let (tx, rx) = tokio::sync::oneshot::channel();
    assert!(pool.register_oauth_callback("server", "flow", tx));

    pool.shutdown().await;

    assert!(
        rx.await.is_err(),
        "shutdown must cancel the callback waiter"
    );
    assert!(pool.active_oauth_flow("server").is_none());
}

#[tokio::test]
async fn test_shutdown_is_idempotent_and_closes_admission() {
    let pool = McpClientPool::new_empty();
    pool.shutdown().await;
    pool.shutdown().await;

    let (tx, _rx) = tokio::sync::oneshot::channel();
    assert!(!pool.register_oauth_callback("server", "flow", tx));
    assert!(!pool.is_open());
    assert!(pool.services.lock().is_empty());
}

#[tokio::test]
async fn test_cancelled_shutdown_waiter_keeps_single_service_transaction_owned() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pool = Arc::new(McpClientPool::new_empty());
    let first_release = Arc::new(tokio::sync::Notify::new());
    let second_release = Arc::new(tokio::sync::Notify::new());
    let first_count = Arc::new(AtomicUsize::new(0));
    let second_count = Arc::new(AtomicUsize::new(0));
    let (first_entered_tx, mut first_entered_rx) = tokio::sync::oneshot::channel();
    let (second_entered_tx, mut second_entered_rx) = tokio::sync::oneshot::channel();
    pool.services.lock().insert(
        "first".to_string(),
        controlled_service(
            first_entered_tx,
            Arc::clone(&first_release),
            Arc::clone(&first_count),
        ),
    );
    pool.services.lock().insert(
        "second".to_string(),
        controlled_service(
            second_entered_tx,
            Arc::clone(&second_release),
            Arc::clone(&second_count),
        ),
    );

    let first_pool = Arc::clone(&pool);
    let first_waiter = tokio::spawn(async move { first_pool.shutdown().await });
    let first_service_started = tokio::select! {
        result = &mut first_entered_rx => {
            result.expect("first close signal must arrive");
            true
        }
        result = &mut second_entered_rx => {
            result.expect("second close signal must arrive");
            false
        }
    };
    first_waiter.abort();
    first_waiter
        .await
        .expect_err("caller cancellation must win");

    let retry_pool = Arc::clone(&pool);
    let mut retry = tokio::spawn(async move { retry_pool.shutdown().await });
    if first_service_started {
        first_release.notify_one();
        tokio::select! {
            result = &mut second_entered_rx => {
                result.expect("retry must observe the same transaction reaching service two");
            }
            result = &mut retry => {
                result.expect("retry waiter must not panic");
                panic!("retry returned before the remaining service was explicitly closed");
            }
        }
    } else {
        second_release.notify_one();
        tokio::select! {
            result = &mut first_entered_rx => {
                result.expect("retry must observe the same transaction reaching service one");
            }
            result = &mut retry => {
                result.expect("retry waiter must not panic");
                panic!("retry returned before the remaining service was explicitly closed");
            }
        }
    }
    assert_eq!(first_count.load(Ordering::SeqCst), 1);
    assert_eq!(second_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        pool.lifecycle.load(Ordering::Acquire),
        1,
        "pool cannot publish Closed before every service settles"
    );
    if first_service_started {
        second_release.notify_one();
    } else {
        first_release.notify_one();
    }
    let report = retry.await.expect("retry waiter must complete");

    assert_eq!(first_count.load(Ordering::SeqCst), 1);
    assert_eq!(second_count.load(Ordering::SeqCst), 1);
    assert_eq!(pool.lifecycle.load(Ordering::Acquire), 2);
    assert_eq!(
        report,
        McpPoolShutdownReport::Complete {
            settled_services: 2,
            failed_services: 0,
        }
    );
}

#[tokio::test]
async fn test_concurrent_and_repeated_shutdown_share_one_service_transaction() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pool = Arc::new(McpClientPool::new_empty());
    let release = Arc::new(tokio::sync::Notify::new());
    let close_count = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    pool.services.lock().insert(
        "only".to_string(),
        controlled_service(entered_tx, Arc::clone(&release), Arc::clone(&close_count)),
    );

    let first_pool = Arc::clone(&pool);
    let second_pool = Arc::clone(&pool);
    let first = tokio::spawn(async move { first_pool.shutdown().await });
    entered_rx.await.expect("service close must start");
    let second = tokio::spawn(async move { second_pool.shutdown().await });
    release.notify_one();
    let first_report = first.await.unwrap();
    let second_report = second.await.unwrap();
    let repeated_report = pool.shutdown().await;

    assert_eq!(close_count.load(Ordering::SeqCst), 1);
    assert_eq!(pool.lifecycle.load(Ordering::Acquire), 2);
    assert_eq!(first_report, second_report);
    assert_eq!(second_report, repeated_report);
}

#[tokio::test]
async fn test_timed_out_service_is_recorded_incomplete_and_never_publishes_closed() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pool = McpClientPool::new_empty();
    let close_count = Arc::new(AtomicUsize::new(0));
    pool.services.lock().insert(
        "unfinished".to_string(),
        timing_out_service(Arc::clone(&close_count)),
    );

    let first = pool.shutdown().await;
    let repeated = pool.shutdown().await;

    assert_eq!(
        first,
        McpPoolShutdownReport::Incomplete {
            settled_services: 0,
            unfinished_services: 1,
            failed_services: 0,
        }
    );
    assert_eq!(repeated, first);
    assert_eq!(
        close_count.load(Ordering::SeqCst),
        2,
        "retry must retain and revisit the original service owner"
    );
    assert_eq!(
        pool.lifecycle.load(Ordering::Acquire),
        1,
        "an rmcp timeout is degraded evidence, not Closed"
    );
}

#[tokio::test]
async fn test_shutdown_clears_notifier_capture() {
    let pool = McpClientPool::new_empty();
    let captured = Arc::new(());
    let weak = Arc::downgrade(&captured);
    pool.set_notifier(Box::new(move |_| {
        let _ = &captured;
    }));

    pool.shutdown().await;

    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn test_callback_cloned_before_begin_shutdown_cannot_repopulate_pending_flow() {
    let pool = Arc::new(McpClientPool::new_empty());
    let weak_pool = Arc::downgrade(&pool);
    pool.set_oauth_event_callback(move |event| {
        let OAuthFlowEvent::AuthorizationNeeded {
            flow_id,
            server_name,
            callback_tx,
            ..
        } = event
        else {
            return;
        };
        if let Some(pool) = weak_pool.upgrade() {
            let _ = pool.register_oauth_callback(&server_name, &flow_id, callback_tx);
        }
    });
    let callback = pool
        .oauth_event_callback()
        .expect("callback must be cloned before shutdown");
    let (callback_tx, callback_rx) = tokio::sync::oneshot::channel();

    pool.begin_shutdown();
    callback(OAuthFlowEvent::AuthorizationNeeded {
        flow_id: "flow-cloned".to_string(),
        server_name: "docs".to_string(),
        authorization_url: "https://example.test/authorize".to_string(),
        callback_tx,
    });

    assert!(
        callback_rx.await.is_err(),
        "late callback sender must be dropped"
    );
    assert!(pool.active_oauth_flow("docs").is_none());
    assert!(pool
        .deliver_oauth_callback("docs", "code".into(), "state".into())
        .is_err());
}

#[tokio::test]
async fn test_blocked_oauth_task_crossing_shutdown_cannot_commit_callback_or_service() {
    use crate::mcp::{McpTaskKey, McpTaskOwner, McpTaskShutdownReport};

    let (mut owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    let weak_pool = Arc::downgrade(&pool);
    pool.set_oauth_event_callback(move |event| {
        let OAuthFlowEvent::AuthorizationNeeded {
            flow_id,
            server_name,
            callback_tx,
            ..
        } = event
        else {
            return;
        };
        if let Some(pool) = weak_pool.upgrade() {
            let _ = pool.register_oauth_callback(&server_name, &flow_id, callback_tx);
        }
    });
    let cloned_callback = pool.oauth_event_callback().unwrap();
    let task_pool = Arc::clone(&pool);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let (callback_tx, callback_rx) = tokio::sync::oneshot::channel();
    let (commit_tx, commit_rx) = tokio::sync::oneshot::channel();
    let close_release = Arc::new(tokio::sync::Notify::new());
    close_release.notify_one();
    let close_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (close_entered_tx, _close_entered_rx) = tokio::sync::oneshot::channel();
    let service = controlled_service(close_entered_tx, close_release, close_count);
    pool.spawn_background(McpTaskKey::OAuth("flow-blocked".into()), async move {
        let _ = started_tx.send(());
        let _ = release_rx.await;
        cloned_callback(OAuthFlowEvent::AuthorizationNeeded {
            flow_id: "flow-blocked".to_string(),
            server_name: "docs".to_string(),
            authorization_url: "https://example.test/authorize".to_string(),
            callback_tx,
        });
        let committed = match task_pool.try_commit_connection(
            "docs".to_string(),
            connected_test_handle("docs"),
            service,
        ) {
            Ok(()) => true,
            Err(mut service) => {
                let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
                false
            }
        };
        let _ = commit_tx.send(committed);
    })
    .unwrap();

    started_rx.await.unwrap();
    pool.begin_shutdown();
    release_tx.send(()).unwrap();
    assert!(
        !commit_rx.await.unwrap(),
        "Closing must reject the service commit"
    );
    assert!(
        callback_rx.await.is_err(),
        "Closing must reject late callback state"
    );
    owner.begin_shutdown();
    assert_eq!(owner.shutdown().await, McpTaskShutdownReport::Complete);
    assert!(pool.services.lock().is_empty());
    assert!(pool.clients.read().is_empty());
    assert!(pool.active_oauth_flow("docs").is_none());
    assert!(pool.shutdown().await.is_complete());
}

#[tokio::test]
async fn test_concurrent_terminal_shutdown_settles_pending_task_subscription_and_service_once() {
    use crate::mcp::{McpTaskKey, McpTaskOwner, McpTaskShutdownReport};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (mut owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    assert_eq!(
        pool.reserve_oauth_flow("docs", "flow-terminal"),
        OAuthStartDisposition::Started
    );
    let (pending_tx, pending_rx) = tokio::sync::oneshot::channel();
    assert!(pool.register_oauth_callback("docs", "flow-terminal", pending_tx));
    let (oauth_drop_tx, oauth_drop_rx) = tokio::sync::oneshot::channel();
    let (subscription_drop_tx, subscription_drop_rx) = tokio::sync::oneshot::channel();
    let (oauth_started_tx, oauth_started_rx) = tokio::sync::oneshot::channel();
    let (subscription_started_tx, subscription_started_rx) = tokio::sync::oneshot::channel();
    pool.spawn_background(McpTaskKey::OAuth("flow-terminal".into()), async move {
        let _guard = TestDropSignal(Some(oauth_drop_tx));
        let _ = oauth_started_tx.send(());
        std::future::pending::<()>().await;
    })
    .unwrap();
    pool.spawn_background(McpTaskKey::Subscription("docs".into()), async move {
        let _guard = TestDropSignal(Some(subscription_drop_tx));
        let _ = subscription_started_tx.send(());
        std::future::pending::<()>().await;
    })
    .unwrap();
    oauth_started_rx.await.unwrap();
    subscription_started_rx.await.unwrap();

    let release = Arc::new(tokio::sync::Notify::new());
    let close_count = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    pool.services.lock().insert(
        "docs".to_string(),
        controlled_service(entered_tx, Arc::clone(&release), Arc::clone(&close_count)),
    );

    pool.begin_shutdown();
    owner.begin_shutdown();
    assert_eq!(owner.shutdown().await, McpTaskShutdownReport::Complete);
    oauth_drop_rx.await.unwrap();
    subscription_drop_rx.await.unwrap();
    assert!(pending_rx.await.is_err());

    let first_pool = Arc::clone(&pool);
    let second_pool = Arc::clone(&pool);
    let first = tokio::spawn(async move { first_pool.shutdown().await });
    entered_rx.await.unwrap();
    let second = tokio::spawn(async move { second_pool.shutdown().await });
    release.notify_one();
    let first_report = first.await.unwrap();
    let second_report = second.await.unwrap();
    let repeated_report = pool.shutdown().await;

    assert!(first_report.is_complete());
    assert_eq!(first_report, second_report);
    assert_eq!(second_report, repeated_report);
    assert_eq!(close_count.load(Ordering::SeqCst), 1);
    assert!(pool.active_oauth_flow("docs").is_none());
}
