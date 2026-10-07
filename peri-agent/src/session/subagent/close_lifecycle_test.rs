use super::*;
use peri_acp_types::ports::{McpPoolPort, McpPoolShutdownReport};
use peri_acp_types::session_resources::{
    ControlAction, ControlCommand, ControlDecision, ControlStatus, SessionResources,
};
use peri_acp_types::tasks::TaskManager as _;

pub(super) async fn reopen_closed_child_fixture(
    store: &MockSessionResources,
    session_id: &ThreadId,
) {
    let current = store.load_session_control(session_id).await.unwrap();
    assert_eq!(current.status, ControlStatus::Closed);
    assert!(current.attempt.is_none());
    let receipt = store
        .apply_session_control(&ControlCommand {
            session_id: session_id.clone(),
            command_id: format!(
                "fixture-explicit-reopen:{}:{}",
                session_id, current.lifecycle
            ),
            expected_lifecycle: current.lifecycle,
            expected_revision: current.revision,
            expected_control_generation: current.control_generation,
            action: ControlAction::Reopen,
        })
        .await
        .unwrap();
    assert_eq!(receipt.decision, ControlDecision::Accepted);
    assert_eq!(receipt.state.status, ControlStatus::Active);
    assert_eq!(receipt.state.lifecycle, current.lifecycle + 1);
    store.initialize_historical_fixture(session_id).await;
}

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
    async fn agent_session_binding_for_lifecycle(
        self: Arc<Self>,
        _: &str,
        lifecycle: u64,
    ) -> Result<
        Option<(
            peri_acp_types::session::InboxHandle,
            Arc<dyn peri_acp_types::tasks::TaskManager>,
        )>,
        String,
    > {
        assert_eq!(lifecycle, 1);
        Ok(None)
    }
    fn bind_agent_session_for_lifecycle(
        &self,
        _: &str,
        lifecycle: u64,
        _: peri_acp_types::session::InboxHandle,
        _: Arc<dyn peri_acp_types::tasks::TaskManager>,
    ) -> Result<(), String> {
        assert_eq!(lifecycle, 1);
        Ok(())
    }
    async fn close_agent_session_scope(self: Arc<Self>, _: &str) -> Result<(), String> {
        self.entered.notify_one();
        Err("Incomplete: resource owner unavailable".into())
    }
    fn bind_agent_session_resources(
        &self,
        _: &str,
        lifecycle: u64,
        resources: Arc<dyn SessionResources>,
    ) -> Result<(), String> {
        assert_eq!(lifecycle, 1);
        drop(resources);
        Ok(())
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
        SubagentLlmSource::prebuilt(Box::new(llm)),
        SubagentRunMode::Background,
        Some(manager.clone()),
        Some(token.clone()),
    );
    config.on_bg_complete = Some(Arc::new(move |result, _| {
        terminal.send(result.clone()).unwrap();
        Ok(())
    }));
    let spawned = AdmittedSessionFactory::resume_subagent(Some(&parent), config)
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
    assert_eq!(
        store.load_session_control(&thread_id).await.unwrap().status,
        ControlStatus::Closing
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
        SubagentLlmSource::prebuilt(Box::new(llm)),
        SubagentRunMode::Sync,
        None,
        None,
    );
    let running = tokio::spawn(async move {
        AdmittedSessionFactory::resume_subagent(Some(&parent), config).await
    });
    entered.await.unwrap();
    let observed = store.load_session_control(&thread_id).await.unwrap();
    assert!(observed.attempt.is_some());
    running.abort();
    assert!(matches!(running.await, Err(error) if error.is_cancelled()));
    tokio::time::timeout(std::time::Duration::from_secs(2), pool.entered.notified())
        .await
        .unwrap();
    let unfinished = store.load_session_control(&thread_id).await.unwrap();
    assert_eq!(unfinished.status, ControlStatus::Closing);
    assert_eq!(unfinished.attempt, observed.attempt);
    assert_eq!(
        store.load_meta(&thread_id).await.unwrap().agent_status,
        AgentStatus::Active
    );
    assert!(AdmittedSessionFactory::resume_subagent(
        None,
        resume_config(store.clone(), thread_id.clone())
    )
    .await
    .is_err());
}

#[tokio::test]
async fn close_intent_failure_does_not_claim_resources_closed_or_mutate_runtime() {
    let store = MockSessionResources::new();
    let manager = Arc::new(TaskManager::new());
    let session = Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder().build(),
        Some("child".into()),
    );
    session.set_subagent_host(SubagentHost {
        session_resources: Some(store.clone()),
        task_manager: Some(manager.clone()),
        ..Default::default()
    });
    store.restrict_to_history_read_only();
    let result = crate::session::subagent::close_subagent_session_scope(session.clone()).await;
    assert!(result.unwrap_err().contains("Incomplete"));
    assert_eq!(
        store
            .load_session_control(&"child".into())
            .await
            .unwrap()
            .status,
        ControlStatus::Active
    );
    assert!(!session.config().cancel_token.is_cancelled());
    manager
        .spawn_owned(Box::pin(async {}))
        .unwrap()
        .await
        .unwrap();
}
