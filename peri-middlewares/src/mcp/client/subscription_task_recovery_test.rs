use super::*;
use crate::mcp::client::output_store::tests::durable_invocation_fixture::DurableInvocationFixture;
use crate::mcp::client::output_store::tests::Wire;
use peri_acp_types::mcp::McpSubscriptionPort;
use peri_acp_types::session::{MessageQueue, QueuedPayload, SessionInbox};
use rmcp::{
    model::{CustomResult, GetTaskResult, Task, TaskStatus},
    service::{RequestContext, RoleServer},
    ErrorData, ServerHandler,
};

#[derive(Clone)]
struct RecoveryOwner {
    initiator: Option<String>,
    working: bool,
    catalog: Option<serde_json::Value>,
}

fn terminal_task() -> DetailedTask {
    DetailedTask::new(
        Task::new(
            "recovered-task",
            TaskStatus::Completed,
            "2026-10-05T00:00:00Z",
            "2026-10-05T00:00:01Z",
        ),
        TaskPayload::Completed {
            result: serde_json::Map::new(),
        },
    )
}

impl ServerHandler for RecoveryOwner {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        rmcp::model::ServerConfig::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tasks()
                .build(),
        )
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _: RequestContext<RoleServer>,
    ) -> Result<CustomResult, ErrorData> {
        if request.method == "workspace/invocationSnapshot" {
            return Ok(CustomResult(self.catalog.clone().expect("owner catalog")));
        }
        assert_eq!(request.method, "workspace/taskSnapshot");
        let task = if self.working {
            DetailedTask::new(
                Task::new(
                    "recovered-task",
                    TaskStatus::Working,
                    "2026-10-05T00:00:00Z",
                    "2026-10-05T00:00:00Z",
                ),
                TaskPayload::Working,
            )
        } else {
            terminal_task()
        };
        let mut row =
            json!({"task": task, "summary": "owner result", "terminalTransitionId": "terminal-1"});
        if let Some(initiator) = &self.initiator {
            row["initiatorSessionId"] = initiator.clone().into();
        }
        Ok(CustomResult(
            json!({"cursor": 1, "epoch": 0, "closing": false, "tasks": [row]}),
        ))
    }

    async fn get_task(
        &self,
        _: GetTaskParams,
        _: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        Ok(GetTaskResult::new(terminal_task()))
    }
}

fn bind_session(pool: &McpClientPool, session: &str) -> (Arc<dyn TaskManager>, SessionInbox) {
    let manager: Arc<dyn TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let inbox = SessionInbox::new(Arc::new(MessageQueue::new()));
    pool.bind_session_task_manager(session, &manager);
    pool.register_inbox(session, inbox.handle());
    (manager, inbox)
}

#[tokio::test]
async fn cold_recovery_rebuilds_child_catalog_without_parent_runtime() {
    for working in [false, true] {
        let fixture = DurableInvocationFixture::new(
            "child",
            "mcp__workspace__Bash",
            &[json!({"command":"fixture"})],
        )
        .await;
        let wire = Wire::connect(RecoveryOwner {
            initiator: Some("child".into()),
            working,
            catalog: Some(fixture.owner_catalog(0, "recovered-task")),
        })
        .await;
        let (mut owner, spawner) = crate::mcp::task_scope::McpTaskOwner::new();
        let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
        wire.install(&pool, "workspace", true);
        let (manager, inbox) = bind_session(&pool, "child");
        peri_acp_types::ports::McpPoolPort::bind_agent_session_resources(
            pool.as_ref(),
            "child",
            1,
            fixture.resources.clone(),
        )
        .unwrap();
        pool.recover_workspace_tasks("child").await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager
                .snapshot()
                .tasks
                .first()
                .is_none_or(|task| task.status != "completed")
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            panic!("recovery did not settle: {error}; {:?}", manager.snapshot())
        });
        assert_eq!(manager.snapshot().tasks.len(), 1);
        assert_eq!(
            manager.snapshot().tasks[0].initiator_session_id.as_deref(),
            Some("child")
        );
        let messages = inbox.queue().drain_all();
        assert_eq!(messages.len(), 1);
        let QueuedPayload::SystemReminder(reminder) = &messages[0].payload else {
            panic!("reminder")
        };
        assert_eq!(reminder.as_reminder().metadata["initiator"], "child");
        assert_eq!(reminder.as_reminder().metadata["delivery"], "initiator");
        owner.shutdown().await;
        pool.clients.write().clear();
        wire.close().await;
    }
}

#[tokio::test]
async fn unknown_or_conflicting_owner_origin_stays_unroutable() {
    for initiator in [None, Some("root".into()), Some(String::new())] {
        let wire = Wire::connect(RecoveryOwner {
            initiator,
            working: false,
            catalog: None,
        })
        .await;
        let pool = Arc::new(McpClientPool::new_empty());
        wire.install(&pool, "workspace", true);
        let (root_manager, root_inbox) = bind_session(&pool, "root");
        let (child_manager, child_inbox) = bind_session(&pool, "child");
        for _ in 0..2 {
            let error = pool.recover_workspace_tasks("child").await.unwrap_err();
            assert!(error.contains("Unroutable"), "{error}");
            assert!(root_manager.snapshot().tasks.is_empty());
            assert!(child_manager.snapshot().tasks.is_empty());
            assert!(root_inbox.queue().drain_all().is_empty());
            assert!(child_inbox.queue().drain_all().is_empty());
        }
        pool.clients.write().clear();
        wire.close().await;
    }
}

#[tokio::test]
async fn unloaded_child_terminal_retries_same_delivery_without_root_fallback() {
    let fixture = DurableInvocationFixture::new(
        "child",
        "mcp__workspace__Bash",
        &[json!({"command":"fixture"})],
    )
    .await;
    let wire = Wire::connect(RecoveryOwner {
        initiator: Some("child".into()),
        working: false,
        catalog: Some(fixture.owner_catalog(0, "recovered-task")),
    })
    .await;
    let pool = Arc::new(McpClientPool::new_empty());
    wire.install(&pool, "workspace", true);
    let (_, root_inbox) = bind_session(&pool, "root");
    let (manager, child_inbox) = bind_session(&pool, "child");
    peri_acp_types::ports::McpPoolPort::bind_agent_session_resources(
        pool.as_ref(),
        "child",
        1,
        fixture.resources.clone(),
    )
    .unwrap();
    pool.session_bindings.write().unregister("child");
    pool.recover_workspace_tasks("child").await.unwrap();
    assert!(root_inbox.queue().drain_all().is_empty());
    pool.register_inbox("child", child_inbox.handle());
    pool.recover_workspace_tasks("child").await.unwrap();
    pool.recover_workspace_tasks("child").await.unwrap();
    assert_eq!(manager.snapshot().tasks.len(), 1);
    assert_eq!(child_inbox.queue().drain_all().len(), 1);
    assert!(root_inbox.queue().drain_all().is_empty());
    pool.clients.write().clear();
    wire.close().await;
}
