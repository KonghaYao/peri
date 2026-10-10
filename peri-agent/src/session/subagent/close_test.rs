use super::*;
use crate::agent::async_tasks::TaskManager;
use crate::session::{subagent::SubagentHost, FrozenContext};
use peri_acp_types::ports::{McpPoolPort, McpPoolShutdownReport};
use std::sync::atomic::{AtomicUsize, Ordering};

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
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
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
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
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
    settle_explicit_close(&session, false).await.unwrap();
    assert!(!session.config().cancel_token.is_cancelled());
    assert!(!session.subagent_host().unwrap().close_state.is_closing());
    manager
        .spawn_owned(Box::pin(async {}))
        .unwrap()
        .await
        .unwrap();
}

#[tokio::test]
async fn cancelled_token_is_not_resource_close_evidence() {
    let session = child(
        Arc::new(TaskManager::new()),
        Some(Arc::new(UnavailablePool)),
    );
    session.config().cancel_token.cancel();
    let result = settle_explicit_close(&session, true).await;
    assert!(result.unwrap_err().contains("Incomplete"));
    assert!(session.subagent_host().unwrap().close_state.is_closing());
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
    })).unwrap();
    let closing = tokio::spawn(close_subagent_session_scope(session.clone()));
    while !session.config().cancel_token.is_cancelled() {
        tokio::task::yield_now().await;
    }
    assert!(!closing.is_finished());
    assert!(!independent.is_cancelled());
    assert!(manager.spawn_owned(Box::pin(async {})).is_err());
    release.send(()).unwrap();
    owned.await.unwrap();
    closing.await.unwrap().unwrap();
}

#[tokio::test]
async fn abandoned_close_waiter_does_not_lose_resource_barrier() {
    let pool = Arc::new(GatedPool {
        entered: Arc::default(),
        release: Arc::default(),
        calls: AtomicUsize::new(0),
    });
    let session = child(Arc::new(TaskManager::new()), Some(pool.clone()));
    let first = tokio::spawn(close_subagent_session_scope(session.clone()));
    pool.entered.notified().await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let second = tokio::spawn(close_subagent_session_scope(session));
    tokio::task::yield_now().await;
    assert!(!second.is_finished());
    assert_eq!(pool.calls.load(Ordering::SeqCst), 1);
    pool.release.notify_one();
    second.await.unwrap().unwrap();
}
