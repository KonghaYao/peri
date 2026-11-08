use super::McpClientPool;
use peri_acp_types::{
    event::{BackgroundTaskResult, ShellOutput},
    shell_task::{ShellTaskResultMeta, SHELL_TASK_RESULT_META_KEY},
};
use rmcp::model::{DetailedTask, MetaObject, Task, TaskPayload, TaskStatus};
use serde_json::json;

fn cancelled() -> DetailedTask {
    DetailedTask::new(
        Task::new(
            "shell-remote",
            TaskStatus::Cancelled,
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:01Z",
        ),
        TaskPayload::Cancelled,
    )
}

fn evidence() -> ShellTaskResultMeta {
    ShellTaskResultMeta::new(BackgroundTaskResult {
        task_id: "shell-remote".into(),
        agent_name: "bg-shell".into(),
        prompt_summary: "sleep 30".into(),
        success: false,
        output: "Shell command cancelled; read the output files as needed.".into(),
        tool_calls_count: 0,
        duration_ms: 321,
        timed_out: false,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: Some(Box::new(ShellOutput {
            stdout_path: Some("/tmp/actual.stdout.log".into()),
            stderr_path: Some("/tmp/actual.stderr.log".into()),
            complete: true,
            error: None,
            exit_code: None,
        })),
    })
}

fn meta(value: serde_json::Value) -> MetaObject {
    MetaObject(serde_json::Map::from_iter([(
        SHELL_TASK_RESULT_META_KEY.into(),
        value,
    )]))
}

#[test]
fn cancelled_workspace_shell_keeps_actual_result_and_public_task_identity() {
    let original = evidence();
    let metadata = meta(serde_json::to_value(&original).unwrap());
    let result = McpClientPool::map_task_result(
        "workspace",
        "mcp-public",
        &cancelled(),
        Some(&metadata),
        true,
    )
    .unwrap();
    let mut expected = original.result;
    expected.task_id = "mcp-public".into();
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    let reminder = result.to_notification();
    assert!(reminder.contains("state: cancelled"));
    assert!(reminder.contains("stdout: /tmp/actual.stdout.log"));
    assert!(reminder.contains("stderr: /tmp/actual.stderr.log"));
    assert!(reminder.contains("exit_code: unknown"));
    assert!(!reminder.contains("shell-remote"));
}

#[test]
fn workspace_cancellation_rejects_wrong_task_state_version_and_malformed_metadata() {
    let good = serde_json::to_value(evidence()).unwrap();
    let mut wrong_task = good.clone();
    wrong_task["result"]["task_id"] = json!("another-task");
    let mut wrong_state = good.clone();
    wrong_state["result"]["success"] = json!(true);
    let mut wrong_agent = good.clone();
    wrong_agent["result"]["agent_name"] = json!("mcp");
    let mut wrong_version = good.clone();
    wrong_version["version"] = json!(2);
    let mut timed_out = good.clone();
    timed_out["result"]["timed_out"] = json!(true);
    let mut unresolved = good.clone();
    unresolved["result"]["output"] = json!("abandoned as unresolved");
    let mut missing_output = good.clone();
    missing_output["result"]["shell_output"] = serde_json::Value::Null;
    for (value, error) in [
        (wrong_task, "identity mismatch"),
        (wrong_state, "incompatible executor result"),
        (wrong_agent, "incompatible executor result"),
        (timed_out, "incompatible executor result"),
        (unresolved, "incompatible executor result"),
        (missing_output, "lacks typed output evidence"),
        (wrong_version, "unsupported shell task result version"),
        (
            json!({"version": 1, "result": []}),
            "invalid workspace shell result metadata",
        ),
    ] {
        let metadata = meta(value);
        let failure = McpClientPool::map_task_result(
            "workspace",
            "mcp-public",
            &cancelled(),
            Some(&metadata),
            true,
        )
        .unwrap_err();
        assert!(failure.contains(error), "{failure}");
    }
}

#[tokio::test]
async fn legacy_workspace_cancel_without_metadata_settles_unresolved_once_and_closes() {
    use peri_acp_types::tasks::{
        BgTaskKind, ExternalTaskRegistration, TaskManager as TaskManagerPort, TaskShutdownReport,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let manager = peri_agent::agent::async_tasks::TaskManager::new();
    let deliveries = Arc::new(AtomicUsize::new(0));
    let observed = deliveries.clone();
    let ticket = manager
        .register_external(ExternalTaskRegistration {
            session_id: "session".into(),
            initiator_session_id: Some("session".into()),
            owner_identity: "workspace".into(),
            owner_task_id: "shell-remote".into(),
            kind: BgTaskKind::Shell,
            summary: "legacy cancellation".into(),
            started_at: Some("2026-01-01T00:00:00Z".into()),
            cancel: Arc::new(|| Box::pin(async { Ok(()) })),
            on_terminal: Arc::new(move |result, _| {
                assert_eq!(result.outcome_label(), "unresolved");
                assert!(result.output.contains("Remote task reported Cancelled"));
                assert!(result.output.contains(
                    "process cleanup, exit status, and output file references remain unconfirmed"
                ));
                assert!(result.shell_output.is_none());
                observed.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }),
        })
        .unwrap();
    let pool = McpClientPool::new_pending();
    for _ in 0..2 {
        pool.deliver_managed_task_status(
            "workspace",
            &ticket,
            "terminal",
            &cancelled(),
            None,
            true,
            None,
            &manager,
        )
        .await
        .unwrap();
    }
    assert_eq!(deliveries.load(Ordering::SeqCst), 1);
    assert_eq!(manager.active_count(), 0);
    assert!(!manager.has_unsettled_external());
    let close = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        TaskManagerPort::shutdown(&manager),
    )
    .await
    .unwrap();
    assert_eq!(close, TaskShutdownReport::Complete);
}

#[test]
fn cancelled_shell_retains_explicit_incomplete_output_evidence() {
    let mut original = evidence();
    original.result.shell_output.as_mut().unwrap().complete = false;
    original.result.shell_output.as_mut().unwrap().error =
        Some("stdout file creation failed".into());
    original.result.shell_output.as_mut().unwrap().stdout_path = None;
    let metadata = meta(serde_json::to_value(original).unwrap());
    let result = McpClientPool::map_task_result(
        "workspace",
        "mcp-public",
        &cancelled(),
        Some(&metadata),
        true,
    )
    .unwrap();
    let output = result.shell_output.unwrap();
    assert!(!output.complete);
    assert_eq!(output.error.as_deref(), Some("stdout file creation failed"));
    assert!(output.stdout_path.is_none());
}

#[test]
fn foreign_mcp_metadata_cannot_claim_workspace_shell_output() {
    let metadata = meta(serde_json::to_value(evidence()).unwrap());
    let with_metadata =
        McpClientPool::map_task_result("other", "mcp-public", &cancelled(), Some(&metadata), false)
            .unwrap();
    let without_metadata =
        McpClientPool::map_task_result("other", "mcp-public", &cancelled(), None, false).unwrap();
    assert_eq!(
        serde_json::to_value(with_metadata).unwrap(),
        serde_json::to_value(without_metadata).unwrap()
    );
}

#[test]
fn shell_evidence_is_not_consumed_for_a_non_cancelled_task() {
    let task = DetailedTask::new(
        cancelled().task,
        TaskPayload::Completed {
            result: serde_json::Map::new(),
        },
    );
    let metadata = meta(serde_json::to_value(evidence()).unwrap());
    let result =
        McpClientPool::map_task_result("workspace", "mcp-public", &task, Some(&metadata), true)
            .unwrap();
    assert!(result.success);
    assert!(result.shell_output.is_none());
}
