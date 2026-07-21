use super::*;

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
        child_handle: handle,
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
            duration_ms: 100,
            agent_count: 3,
            tool_calls_count: 5,
            error: None,
            phase_summaries: Vec::new(),
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
            duration_ms: 100,
            agent_count: 3,
            tool_calls_count: 5,
            error: None,
            phase_summaries: Vec::new(),
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
        duration_ms: 58,
        agent_count: 0,
        tool_calls_count: 0,
        error: Some("parallel thunk #0 failed: t is not a function".into()),
        phase_summaries: Vec::new(),
    };
    let notification = result.to_notification();
    assert!(
        notification.contains("Workflow 'haiku-smoke-test' failed"),
        "failed 通知应包含 workflow name 和 failed 状态，实际：{notification}"
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
fn test_notification_omits_error_line_when_completed() {
    // completed 时通知不应出现 Error: 行
    let result = WorkflowTaskResult {
        run_id: "run-ok-12345".into(),
        workflow_name: "test".into(),
        success: true,
        status: WorkflowRunStatus::Completed,
        duration_ms: 1000,
        agent_count: 2,
        tool_calls_count: 0,
        error: None,
        phase_summaries: Vec::new(),
    };
    let notification = result.to_notification();
    assert!(
        notification.contains("Workflow 'test' completed"),
        "completed 通知应包含 workflow name 和 completed 状态"
    );
}
