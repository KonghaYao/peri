use super::McpClientPool;
use crate::mcp::{client::output_store::tests::Wire, McpTaskOwner};
use peri_acp_types::{
    mcp::McpSubscriptionPort,
    session::{MessageQueue, QueuedPayload, SessionInbox},
    shell_task::{ShellTaskResultMeta, SHELL_TASK_RESULT_META_KEY},
    tasks::{BgTaskKind, TaskManager, TaskShutdownReport},
};
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, GetTaskParams, GetTaskResult,
        ServerCapabilities, ServerConfig, TaskStatus,
    },
    service::{RequestContext, RoleServer},
    ServerHandler,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

/// Produce the evidence through the actual Workspace shell executor before
/// changing only its wire metadata. No invented completion or output paths.
struct CancelledShell {
    snapshot: GetTaskResult,
    directory: tempfile::TempDir,
    output_paths: Vec<PathBuf>,
}

impl Drop for CancelledShell {
    fn drop(&mut self) {
        for path in &self.output_paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn actual_cancelled_shell() -> CancelledShell {
    let directory = tempfile::tempdir().unwrap();
    let server = peri_mcp_workspace::WorkspaceMcpServer::standalone(
        directory.path().to_string_lossy(),
        false,
    );
    let wire = Wire::connect(server.clone()).await;
    let request = CallToolRequestParams::new("Bash").with_arguments(
        serde_json::json!({
            "command": "printf 'SHELL_CANCEL_STDOUT\\n'; printf 'SHELL_CANCEL_STDERR\\n' >&2; printf ready > ready; sleep 30",
            "run_in_background": true
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let CallToolResponse::Task(created) = wire.client.peer().call_tool_once(request).await.unwrap()
    else {
        panic!("actual background shell must return a task");
    };
    let id = &created.task.task_id;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !directory.path().join("ready").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real shell must write before cancellation");
    wire.client
        .peer()
        .cancel_task(CancelTaskParams::new(id))
        .await
        .unwrap();
    let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let observed = wire
                .client
                .peer()
                .get_task(GetTaskParams::new(id))
                .await
                .unwrap();
            if observed.task.status().is_terminal() {
                break observed;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual executor cancellation must finish");
    assert_eq!(snapshot.task.status(), TaskStatus::Cancelled);
    let evidence: ShellTaskResultMeta =
        serde_json::from_value(snapshot.meta.as_ref().unwrap()[SHELL_TASK_RESULT_META_KEY].clone())
            .unwrap();
    let output = evidence.result.shell_output.as_ref().unwrap();
    let output_paths = [output.stdout_path.as_ref(), output.stderr_path.as_ref()]
        .into_iter()
        .flatten()
        .map(PathBuf::from)
        .collect();
    assert_eq!(
        std::fs::read_to_string(output.stdout_path.as_ref().unwrap()).unwrap(),
        "SHELL_CANCEL_STDOUT\n"
    );
    assert_eq!(
        std::fs::read_to_string(output.stderr_path.as_ref().unwrap()).unwrap(),
        "SHELL_CANCEL_STDERR\n"
    );
    assert_eq!(
        server.shutdown_shell_tasks().await,
        Some(TaskShutdownReport::Complete)
    );
    wire.close().await;
    CancelledShell {
        snapshot,
        directory,
        output_paths,
    }
}

/// The first read simulates an incompatible generation. A later read can
/// return the unchanged actual v1 executor result, proving that it is fetched.
struct ChangingShellServer {
    actual: GetTaskResult,
    wrong_task_first: Option<GetTaskResult>,
    invalid_forever: bool,
    polls: Arc<AtomicUsize>,
}

impl ServerHandler for ChangingShellServer {
    fn get_info(&self) -> ServerConfig {
        let mut config = ServerConfig::default();
        config.capabilities = ServerCapabilities::builder().enable_tasks().build();
        config
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        _: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, rmcp::ErrorData> {
        assert_eq!(request.task_id, self.actual.task.task.task_id);
        let poll = self.polls.fetch_add(1, Ordering::SeqCst);
        if poll == 0 {
            if let Some(wrong_task) = &self.wrong_task_first {
                return Ok(wrong_task.clone());
            }
        }
        let mut snapshot = self.actual.clone();
        if poll == 0 || self.invalid_forever {
            snapshot.meta.as_mut().unwrap()[SHELL_TASK_RESULT_META_KEY]["version"] =
                serde_json::json!(2);
        }
        Ok(snapshot)
    }
}

async fn exercise_validation(invalid_forever: bool, wrong_task_first: bool) {
    let actual = actual_cancelled_shell().await;
    let other = if wrong_task_first {
        Some(actual_cancelled_shell().await)
    } else {
        None
    };
    let raw_task_id = actual.snapshot.task.task.task_id.clone();
    let polls = Arc::new(AtomicUsize::new(0));
    let wire = Wire::connect(ChangingShellServer {
        actual: actual.snapshot.clone(),
        wrong_task_first: other.as_ref().map(|other| other.snapshot.clone()),
        invalid_forever,
        polls: polls.clone(),
    })
    .await;
    let (mut owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    wire.install(&pool, "workspace", true);
    let queue = Arc::new(MessageQueue::new());
    pool.register_inbox("session", SessionInbox::new(queue.clone()).handle());
    let manager = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let bound: Arc<dyn TaskManager> = manager.clone();
    pool.bind_session_task_manager("session", &bound);
    let ticket = pool
        .register_external_task(
            "session",
            Some("session"),
            None,
            "workspace",
            &raw_task_id,
            BgTaskKind::Shell,
            "actual cancelled shell",
            false,
            "2026-11-01T00:00:00Z",
        )
        .unwrap();
    let public_id = ticket.task_id().to_owned();
    pool.spawn_managed_task_subscription(
        "workspace".into(),
        "session".into(),
        raw_task_id.clone(),
        ticket,
        false,
        true,
        wire.client.peer().clone(),
    )
    .unwrap();
    let settled = tokio::time::timeout(Duration::from_secs(10), async {
        while manager.has_unsettled_external() || owner.active_count() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok();
    let messages = queue.drain_all_sync();
    let active = manager.active_count();
    let unsettled = manager.has_unsettled_external();
    let reads = polls.load(Ordering::SeqCst);
    // Cleanup precedes assertions, including on the old implementation that
    // caches the invalid first response forever.
    pool.clients.write().clear();
    wire.close().await;
    owner.begin_shutdown();
    let _ = owner.shutdown().await;
    drop(pool);
    assert!(settled, "terminal validation did not converge: reads={reads}, active={active}, unsettled={unsettled}");
    assert_eq!(active, 0);
    assert!(!unsettled);
    assert_eq!(messages.len(), 1, "exactly one canonical terminal reminder");
    let QueuedPayload::SystemReminder(reminder) = &messages[0].payload else {
        panic!("terminal must use a canonical reminder");
    };
    let body = &reminder.as_reminder().body;
    assert!(body.contains(&public_id), "{body}");
    if invalid_forever {
        assert_eq!(reads, 3, "invalid evidence has its own finite retry budget");
        assert!(body.contains("state: unresolved"), "{body}");
        assert!(!body.contains("state: cancelled"), "{body}");
        for path in &actual.output_paths {
            assert!(
                !body.contains(path.to_str().unwrap()),
                "invalid result must not expose unverified files: {body}"
            );
        }
    } else {
        assert_eq!(
            reads, 2,
            "corrected evidence must be re-fetched exactly once"
        );
        assert!(body.contains("state: cancelled"), "{body}");
        for path in &actual.output_paths {
            assert!(body.contains(path.to_str().unwrap()), "{body}");
        }
        if let Some(other) = &other {
            assert_ne!(other.snapshot.task.task.task_id, raw_task_id);
            for path in &other.output_paths {
                assert!(
                    !body.contains(path.to_str().unwrap()),
                    "another task's internally valid cancellation must never settle this owner: {body}"
                );
            }
        }
        assert_eq!(
            std::fs::read_to_string(actual.directory.path().join("ready")).unwrap(),
            "ready"
        );
    }
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            TaskManager::shutdown(manager.as_ref())
        )
        .await
        .unwrap(),
        TaskShutdownReport::Complete
    );
}

#[tokio::test]
async fn invalid_first_shell_terminal_is_refetched_and_preserves_actual_cancelled_result() {
    exercise_validation(false, false).await;
}

#[tokio::test]
async fn permanently_invalid_shell_terminal_settles_unresolved_once_and_closes() {
    exercise_validation(true, false).await;
}

#[tokio::test]
async fn internally_valid_other_shell_result_cannot_settle_the_requested_task() {
    exercise_validation(false, true).await;
}
