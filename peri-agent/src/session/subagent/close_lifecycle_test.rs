use super::*;
use peri_acp_types::ports::{McpPoolPort, McpPoolShutdownReport};

#[derive(Default)]
struct UnavailableClosePool {
    entered: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl McpPoolPort for UnavailableClosePool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    async fn shutdown(&self) -> McpPoolShutdownReport {
        panic!("shared pool cannot be closed by child")
    }
    fn snapshot(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    async fn close_agent_session_scope(self: Arc<Self>, _: &str) -> Result<(), String> {
        self.entered.notify_one();
        Err("Incomplete: resource owner unavailable".into())
    }
}

#[tokio::test]
async fn background_close_resource_failure_keeps_delegation_unfinished() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let manager = Arc::new(TaskManager::new());
    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder().build(),
        Some("parent".into()),
    );
    parent.set_subagent_host(SubagentHost {
        mcp_pool: Some(Arc::new(UnavailableClosePool::default())),
        ..Default::default()
    });
    let token = CancellationToken::new();
    let (llm, entered) = CancelGateLLM::new();
    let (terminal, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(llm),
        SubagentRunMode::Background,
        Some(manager.clone()),
        Some(token.clone()),
    );
    config.on_bg_complete = Some(Arc::new(move |result, _| {
        terminal.send(result.clone()).unwrap();
        Ok(())
    }));
    let spawned = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .unwrap();
    entered.await.unwrap();
    spawned.cancel_token.cancel();
    assert!(
        crate::session::subagent::close_subagent_session_scope(spawned.session.clone())
            .await
            .unwrap_err()
            .contains("Incomplete")
    );
    tokio::task::yield_now().await;
    assert!(received.try_recv().is_err());
    assert_eq!(
        store.load_meta(&thread_id).await.unwrap().agent_status,
        AgentStatus::Active
    );
    assert_eq!(manager.list_tasks_full().len(), 1);
    assert!(matches!(
        manager.list_tasks_full()[0].status,
        BackgroundTaskStatus::Running
    ));
}

#[tokio::test]
async fn running_caller_drop_resource_failure_keeps_close_and_claim_unfinished() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let pool = Arc::new(UnavailableClosePool::default());
    let parent = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder().build(),
        Some("parent".into()),
    );
    parent.set_subagent_host(SubagentHost {
        mcp_pool: Some(pool.clone()),
        ..Default::default()
    });
    let (llm, entered) = CancelGateLLM::new();
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(llm),
        SubagentRunMode::Sync,
        None,
        None,
    );
    let running =
        tokio::spawn(async move { SessionFactory::resume_subagent(Some(&parent), config).await });
    entered.await.unwrap();
    running.abort();
    assert!(matches!(running.await, Err(error) if error.is_cancelled()));
    tokio::time::timeout(std::time::Duration::from_secs(2), pool.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        store.load_meta(&thread_id).await.unwrap().agent_status,
        AgentStatus::Active
    );
    assert!(
        SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
            .await
            .is_err()
    );
}
