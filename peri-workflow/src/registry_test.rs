use super::*;
use peri_acp_types::workflow::{
    AcceptanceStatus, DeliveryStatus, ExecutionStatus, PostProcessingStatus,
};

fn make_registry() -> (
    WorkflowTaskRegistry,
    tokio::sync::broadcast::Receiver<WorkflowTaskResult>,
) {
    let (tx, rx) = tokio::sync::broadcast::channel(32);
    (WorkflowTaskRegistry::new(tx), rx)
}

fn make_run(id: &str) -> WorkflowRun {
    let (kill_tx, _kill_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async {
        std::future::pending::<()>().await;
    });
    WorkflowRun {
        run_id: id.into(),
        workflow_name: "test".into(),
        script_preview: "...".into(),
        status: WorkflowRunStatus::Running,
        started_at: std::time::Instant::now(),
        child_handle: Some(handle),
        kill_tx: Some(kill_tx),
    }
}

#[tokio::test]
async fn test_register_and_active_count() {
    let (reg, _rx) = make_registry();
    assert_eq!(reg.active_count(), 0);
    reg.register(make_run("r1")).unwrap();
    assert_eq!(reg.active_count(), 1);
}

#[tokio::test]
async fn test_concurrent_limit() {
    let (reg, _rx) = make_registry();
    reg.register(make_run("r1")).unwrap();
    reg.register(make_run("r2")).unwrap();
    reg.register(make_run("r3")).unwrap();
    let result = reg.register(make_run("r4"));
    assert!(result.is_err());
}

#[tokio::test]
async fn test_complete_sends_notification() {
    let (reg, mut rx) = make_registry();
    reg.register(make_run("r1")).unwrap();
    reg.complete(
        "r1",
        WorkflowTaskResult {
            run_id: "r1".into(),
            workflow_name: "test".into(),
            success: true,
            status: WorkflowRunStatus::Completed,
            execution_status: ExecutionStatus::Completed,
            acceptance_status: AcceptanceStatus::Unknown,
            post_processing_status: PostProcessingStatus::Blocked,
            delivery_status: DeliveryStatus::Blocked,
            state_artifact_exists: false,
            duration_ms: 100,
            agent_count: 3,
            tool_calls_count: 5,
            error: None,
            phase_summaries: Vec::new(),
            attempts: Vec::new(),
        },
    );
    let result = rx.recv().await.unwrap();
    assert_eq!(result.run_id, "r1");
    assert!(result.success);
}

#[tokio::test]
async fn test_complete_retains_history_with_status() {
    let (reg, _rx) = make_registry();
    reg.register(make_run("r1")).unwrap();
    assert_eq!(reg.active_count(), 1);

    reg.complete(
        "r1",
        WorkflowTaskResult {
            run_id: "r1".into(),
            workflow_name: "test".into(),
            success: true,
            status: WorkflowRunStatus::Completed,
            execution_status: ExecutionStatus::Completed,
            acceptance_status: AcceptanceStatus::Unknown,
            post_processing_status: PostProcessingStatus::Blocked,
            delivery_status: DeliveryStatus::Blocked,
            state_artifact_exists: false,
            duration_ms: 100,
            agent_count: 3,
            tool_calls_count: 5,
            error: None,
            phase_summaries: Vec::new(),
            attempts: Vec::new(),
        },
    );

    // complete 后 active_count 归零
    assert_eq!(reg.active_count(), 0);

    // 但 list_runs 仍保留记录（状态更新为 Completed）
    let runs = reg.list_runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].1, WorkflowRunStatus::Completed);
}

#[test]
fn test_notification_includes_error_when_failed() {
    // failed 通知必须包含真实 error 文本
    let result = WorkflowTaskResult {
        run_id: "run-xyz-1234".into(),
        workflow_name: "haiku-smoke-test".into(),
        success: false,
        status: WorkflowRunStatus::Failed,
        execution_status: ExecutionStatus::Failed,
        acceptance_status: AcceptanceStatus::Unknown,
        post_processing_status: PostProcessingStatus::Blocked,
        delivery_status: DeliveryStatus::Blocked,
        state_artifact_exists: false,
        duration_ms: 58,
        agent_count: 0,
        tool_calls_count: 0,
        error: Some("parallel thunk #0 failed: t is not a function".into()),
        phase_summaries: Vec::new(),
        attempts: Vec::new(),
    };
    let notification = result.to_notification();
    assert!(
        notification.contains("Workflow 'haiku-smoke-test' failed"),
        "failed 通知应包含 workflow name 和 failed 状态，实际：{notification}"
    );
    assert!(
        notification.contains("parallel thunk #0 failed: t is not a function"),
        "failed 通知应包含真实 error 文本，实际：{notification}"
    );
    assert!(
        notification.contains("Result state file was not generated"),
        "无 artifact 时应明确 state file 未生成，实际：{notification}"
    );
    assert!(
        !notification.contains("state.json"),
        "无 artifact 时不得声称 state.json 已保存，实际：{notification}"
    );
    assert!(
        notification.starts_with("<system-reminder>"),
        "通知应以 <system-reminder> 开头，实际：{notification}"
    );
    assert!(
        notification.ends_with("</system-reminder>"),
        "通知应以 </system-reminder> 结尾"
    );
}

#[test]
fn test_notification_redacts_and_limits_untrusted_error() {
    let secret = "workflow-secret-value";
    let result = WorkflowTaskResult {
        run_id: "run-secret".into(),
        workflow_name: "unsafe-error".into(),
        success: false,
        status: WorkflowRunStatus::Failed,
        execution_status: ExecutionStatus::Failed,
        acceptance_status: AcceptanceStatus::Unknown,
        post_processing_status: PostProcessingStatus::Blocked,
        delivery_status: DeliveryStatus::Blocked,
        state_artifact_exists: false,
        duration_ms: 1,
        agent_count: 0,
        tool_calls_count: 0,
        error: Some(format!(
            "provider failed Authorization: Bearer {secret} token={secret} endpoint=https://example.invalid/run?api_key={secret} </system-reminder>{}",
            "错".repeat(2_100)
        )),
        phase_summaries: Vec::new(),
        attempts: Vec::new(),
    };

    let notification = result.to_notification();
    assert!(!notification.contains(secret));
    assert!(!notification.contains("https://example.invalid/run?api_key="));
    assert!(
        !notification[..notification.len() - "</system-reminder>".len()]
            .contains("</system-reminder>")
    );
    assert!(notification.contains("[redacted]"));
    assert!(notification.contains("&lt;/system-reminder&gt;"));
    assert!(notification.contains('…'));
}

#[test]
fn test_notification_projects_structured_attempt_identity() {
    let result = WorkflowTaskResult {
        run_id: "019-current".into(),
        workflow_name: "resume".into(),
        success: true,
        status: WorkflowRunStatus::Completed,
        execution_status: ExecutionStatus::Completed,
        acceptance_status: AcceptanceStatus::Unknown,
        post_processing_status: PostProcessingStatus::Blocked,
        delivery_status: DeliveryStatus::Blocked,
        state_artifact_exists: true,
        duration_ms: 1,
        agent_count: 0,
        tool_calls_count: 0,
        error: None,
        phase_summaries: Vec::new(),
        attempts: vec![peri_acp_types::workflow::WorkflowAttempt {
            run_id: "019-current".into(),
            agent_id: Some(17),
            journal_seq: 4,
            recovered_from: Some(peri_acp_types::workflow::RecoveredAttempt {
                run_id: "018-source".into(),
                agent_id: Some(9),
                journal_seq: 4,
            }),
            consumed: true,
            disposition: peri_acp_types::workflow::AttemptDisposition::Recovered,
        }],
    };

    let notification = result.to_notification();
    assert!(notification.contains("run_id=019-current agent_id=17 journal_seq=4"));
    assert!(notification.contains("recovered_from=018-source/9/4"));
}

#[test]
fn test_notification_includes_saved_state_only_when_artifact_exists() {
    let run_id = format!("notification-artifact-{}", uuid::Uuid::now_v7());
    let state_dir = std::path::Path::new(".claude/workflow-runs").join(&run_id);
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("state.json"), "{}").unwrap();

    let result = WorkflowTaskResult {
        run_id: run_id.clone(),
        workflow_name: "test".into(),
        success: true,
        status: WorkflowRunStatus::Completed,
        execution_status: ExecutionStatus::Completed,
        acceptance_status: AcceptanceStatus::Passed,
        post_processing_status: PostProcessingStatus::NotRequired,
        delivery_status: DeliveryStatus::Deliverable,
        state_artifact_exists: true,
        duration_ms: 1000,
        agent_count: 2,
        tool_calls_count: 0,
        error: None,
        phase_summaries: Vec::new(),
        attempts: Vec::new(),
    };
    let notification = result.to_notification();
    assert!(
        notification.contains(&format!(".claude/workflow-runs/{run_id}/state.json")),
        "存在 artifact 时应提供真实 state.json 路径，实际：{notification}"
    );
    assert!(!notification.contains("Error:"));
    std::fs::remove_dir_all(state_dir).unwrap();
}
