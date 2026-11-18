use crate::agent::async_tasks::BackgroundTaskStatus;
use crate::agent::async_tasks::TaskManager;

use crate::session::subagent::tests::CancelGateLLM;
use crate::session::subagent::types::SubagentHost;
use crate::session::subagent::SessionFactory;
use crate::session::test_resources::mock::MockSessionResources;
use crate::session::FrozenContext;
use crate::session::Session;
use peri_acp_types::ports::{McpPoolPort, McpPoolShutdownReport};
use peri_acp_types::thread::AgentStatus;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct UnavailableClosePool {
    entered: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl McpPoolPort for UnavailableClosePool {
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
    crate::session::subagent::tests::preset_resumable_thread(&store, &thread_id, None).await;
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
    let mut config = crate::session::subagent::tests::resume_config_with(
        store.clone(),
        thread_id.clone(),
        crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(llm),
            "fixture-scripted",
        ),
        Some(manager.clone()),
        Some(token.clone()),
    );
    config.on_bg_complete = Some(Arc::new(move |result, _| {
        terminal
            .send(result.clone())
            .expect("terminal send test receiver must remain available for this delivery");
        Ok(())
    }));
    let spawned = SessionFactory::resume_subagent(Some(&parent), config)
        .await
        .expect("the resume subagent fixture result is required for this assertion");
    entered
        .await
        .expect("the entered fixture result is required for this assertion");
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
        store
            .load_meta(&thread_id)
            .await
            .expect("the load meta fixture result is required for this assertion")
            .agent_status,
        AgentStatus::Active
    );
    assert_eq!(manager.list_tasks_full().len(), 1);
    assert!(matches!(
        manager.list_tasks_full()[0].status,
        BackgroundTaskStatus::Running
    ));
}
