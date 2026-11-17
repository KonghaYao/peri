use crate::agent::async_tasks::TaskManager;
use crate::session::subagent::SubagentHost;
use crate::session::FrozenContext;
use crate::session::Session;
use peri_acp_types::ports::{McpPoolPort, McpPoolShutdownReport};
use peri_acp_types::tasks::TaskManager as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn child(manager: Arc<TaskManager>, pool: Option<Arc<dyn McpPoolPort>>) -> Arc<Session> {
    let session = Session::new(
        Arc::from("/tmp/child-close"),
        FrozenContext::builder().build(),
        Some("child".into()),
    );
    session.set_subagent_host(SubagentHost {
        task_manager: Some(manager),
        mcp_pool: pool,
        ..Default::default()
    });
    session
}

struct UnavailablePool;

#[async_trait::async_trait]
impl McpPoolPort for UnavailablePool {
    async fn shutdown(&self) -> McpPoolShutdownReport {
        panic!("child close must not shutdown shared pool")
    }
    fn snapshot(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}

struct GatedPool {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl McpPoolPort for GatedPool {
    async fn shutdown(&self) -> McpPoolShutdownReport {
        panic!("child close must not shutdown shared pool")
    }
    fn snapshot(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    async fn close_agent_session_scope(self: Arc<Self>, session_id: &str) -> Result<(), String> {
        assert_eq!(session_id, "child");
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn natural_completion_does_not_close_child_scope() {
    let manager = Arc::new(TaskManager::new());
    let session = child(manager.clone(), Some(Arc::new(UnavailablePool)));
    crate::session::subagent::close::settle_explicit_close(&session, false)
        .await
        .expect("the settle explicit close fixture result is required for this assertion");
    assert!(!session.config().cancel_token.is_cancelled());
    assert!(!session
        .subagent_host()
        .expect("the subagent host fixture result is required for this assertion")
        .close_state
        .is_closing());
    manager
        .spawn_owned(Box::pin(async {}))
        .expect("the spawn owned fixture result is required for this assertion")
        .await
        .expect("natural completion does not close child scope: the completed fixture operation must provide its result payload");
}

#[tokio::test]
async fn cancelled_token_is_not_resource_close_evidence() {
    let session = child(
        Arc::new(TaskManager::new()),
        Some(Arc::new(UnavailablePool)),
    );
    session.config().cancel_token.cancel();
    let result = crate::session::subagent::close::settle_explicit_close(&session, true).await;
    assert!(result.unwrap_err().contains("Incomplete"));
    assert!(session
        .subagent_host()
        .expect("the subagent host fixture result is required for this assertion")
        .close_state
        .is_closing());
}

#[tokio::test]
async fn explicit_close_waits_for_owned_work_without_cancelling_independent_delegate() {
    let manager = Arc::new(TaskManager::new());
    let session = child(manager.clone(), None);
    let independent = tokio_util::sync::CancellationToken::new();
    let observed = independent.clone();
    let (release, wait) = tokio::sync::oneshot::channel();
    let owned = manager.spawn_owned(Box::pin(async move {
        tokio::select! { _ = observed.cancelled() => panic!("independent delegate implicitly cancelled"), _ = wait => {} }
    })).expect("the spawn owned fixture result is required for this assertion");
    let closing = tokio::spawn(
        crate::session::subagent::close::close_subagent_session_scope(session.clone()),
    );
    while !session.config().cancel_token.is_cancelled() {
        tokio::task::yield_now().await;
    }
    assert!(!closing.is_finished());
    assert!(!independent.is_cancelled());
    assert!(manager.spawn_owned(Box::pin(async {})).is_err());
    release
        .send(())
        .expect("release send test receiver must remain available for this delivery");
    owned
        .await
        .expect("the owned fixture result is required for this assertion");
    closing.await.expect("the closing fixture result is required for this assertion").expect("explicit close waits for owned work without cancelling independent delegate: the completed fixture operation must provide its result payload");
}

#[tokio::test]
async fn abandoned_close_waiter_does_not_lose_resource_barrier() {
    let pool = Arc::new(GatedPool {
        entered: Arc::default(),
        release: Arc::default(),
        calls: AtomicUsize::new(0),
    });
    let session = child(Arc::new(TaskManager::new()), Some(pool.clone()));
    let first = tokio::spawn(
        crate::session::subagent::close::close_subagent_session_scope(session.clone()),
    );
    pool.entered.notified().await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let second =
        tokio::spawn(crate::session::subagent::close::close_subagent_session_scope(session));
    tokio::task::yield_now().await;
    assert!(!second.is_finished());
    assert_eq!(pool.calls.load(Ordering::SeqCst), 1);
    pool.release.notify_one();
    second.await.expect("the second fixture result is required for this assertion").expect("abandoned close waiter does not lose resource barrier: the completed fixture operation must provide its result payload");
}

#[tokio::test(start_paused = true)]
async fn close_waits_for_handoff_delivery_retry_and_preserves_failure_diagnostic() {
    let session = child(Arc::new(TaskManager::new()), None);
    let state = session
        .subagent_host()
        .expect("the subagent host fixture result is required for this assertion")
        .close_state
        .clone();
    let (retry_sender, retry_receiver) = tokio::sync::oneshot::channel();
    let worker_state = state.clone();
    let worker = state
        .spawn_sync_worker("child".into(), || {
            Ok(async move {
                worker_state.record_sync_delivery_failure("child", "queue rejected result");
                retry_receiver
                    .await
                    .expect("the retry receiver fixture result is required for this assertion");
                peri_time::sleep(std::time::Duration::from_secs(5)).await;
                worker_state.settle_sync_delivery("child");
            })
        })
        .expect("the spawn sync worker fixture result is required for this assertion");
    let closing = tokio::spawn(
        crate::session::subagent::close::close_subagent_session_scope(session.clone()),
    );
    let failure = closing
        .await
        .expect("the closing fixture result is required for this assertion")
        .unwrap_err();
    assert!(failure.contains("queue rejected result"));
    assert!(state.is_closing());
    retry_sender
        .send(())
        .expect("retry sender send test receiver must remain available for this delivery");
    tokio::task::yield_now().await;
    assert!(
        crate::session::subagent::close::close_subagent_session_scope(session.clone())
            .await
            .unwrap_err()
            .contains("queue rejected result")
    );
    tokio::time::advance(std::time::Duration::from_secs(4)).await;
    assert!(!worker.is_finished());
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    worker
        .await
        .expect("the worker fixture result is required for this assertion");
    crate::session::subagent::close::close_subagent_session_scope(session)
        .await
        .expect("the close subagent session scope fixture result is required for this assertion");
}

#[tokio::test]
async fn worker_delivery_failure_is_not_reported_as_successful_close() {
    let session = child(Arc::new(TaskManager::new()), None);
    let state = session
        .subagent_host()
        .expect("the subagent host fixture result is required for this assertion")
        .close_state
        .clone();
    let worker_state = state.clone();
    state
        .spawn_sync_worker("child".into(), || {
            Ok(async move {
                worker_state.record_sync_delivery_failure("child", "queue unavailable");
            })
        })
        .expect("the spawn sync worker fixture result is required for this assertion")
        .await
        .expect("worker delivery failure is not reported as successful close: the completed fixture operation must provide its result payload");
    let error = crate::session::subagent::close::close_subagent_session_scope(session)
        .await
        .unwrap_err();
    assert!(error.contains("child: Incomplete: sync child delivery failed: queue unavailable"));
}

#[tokio::test]
async fn worker_explicit_close_does_not_wait_for_itself_but_external_close_does() {
    let session = child(Arc::new(TaskManager::new()), None);
    let state = session
        .subagent_host()
        .expect("the subagent host fixture result is required for this assertion")
        .close_state
        .clone();
    let (entered_sender, entered_receiver) = tokio::sync::oneshot::channel();
    let (release_sender, release_receiver) = tokio::sync::oneshot::channel();
    let worker_session = session.clone();
    let worker_state = state.clone();
    let worker = state
        .spawn_sync_worker("child".into(), || {
            Ok(async move {
                crate::session::subagent::close::settle_explicit_close(&worker_session, true)
                    .await
                    .expect(
                        "the settle explicit close fixture result is required for this assertion",
                    );
                entered_sender.send(()).expect(
                    "entered sender send test receiver must remain available for this delivery",
                );
                release_receiver
                    .await
                    .expect("the release receiver fixture result is required for this assertion");
                worker_state.settle_sync_delivery("child");
            })
        })
        .expect("the spawn sync worker fixture result is required for this assertion");
    tokio::time::timeout(std::time::Duration::from_secs(2), entered_receiver)
        .await
        .expect("worker must not wait for itself")
        .expect("the expect fixture result is required for this assertion");
    let closing =
        tokio::spawn(crate::session::subagent::close::close_subagent_session_scope(session));
    tokio::task::yield_now().await;
    assert!(!closing.is_finished());
    release_sender
        .send(())
        .expect("release sender send test receiver must remain available for this delivery");
    worker
        .await
        .expect("the worker fixture result is required for this assertion");
    closing.await.expect("the closing fixture result is required for this assertion").expect("worker explicit close does not wait for itself but external close does: the completed fixture operation must provide its result payload");
}

#[tokio::test]
async fn close_rejects_late_worker_registration() {
    let session = child(Arc::new(TaskManager::new()), None);
    let state = session
        .subagent_host()
        .expect("the subagent host fixture result is required for this assertion")
        .close_state
        .clone();
    crate::session::subagent::close::close_subagent_session_scope(session)
        .await
        .expect("the close subagent session scope fixture result is required for this assertion");
    let error = state
        .spawn_sync_worker("late".into(), || Ok(async {}))
        .unwrap_err();
    assert!(error.contains("registered after session close began"));
}

#[test]
fn close_failure_classification_allows_only_unconfirmed_settlement() {
    assert!(
        crate::session::subagent::close::close_failure_allows_terminal_finalization(
            "Incomplete: child settlement is unconfirmed (x); resources closed"
        )
    );
    assert!(
        crate::session::subagent::close::close_failure_allows_terminal_finalization(
            "Incomplete: child handoff is unconfirmed"
        )
    );
    assert!(
        !crate::session::subagent::close::close_failure_allows_terminal_finalization(
            "Incomplete: child history persistence unconfirmed: append failed"
        )
    );
    assert!(
        !crate::session::subagent::close::close_failure_allows_terminal_finalization(
            "Incomplete: child MCP resource owner unavailable"
        )
    );
    assert!(
        !crate::session::subagent::close::close_failure_allows_terminal_finalization(
            "Incomplete: child resources remain unsettled"
        )
    );
}

#[path = "close_admission_test.rs"]
mod admission_tests;
