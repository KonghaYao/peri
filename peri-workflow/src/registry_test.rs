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
    WorkflowRun {
        run_id: id.into(),
        workflow_name: "test".into(),
        script_preview: "...".into(),
        status: WorkflowRunStatus::Running,
        started_at: std::time::Instant::now(),
        child_handle: None,
        kill_tx: None,
    }
}

fn task_record(run_id: &str) -> TaskRecord {
    TaskRecord {
        task_id: run_id.into(),
        kind: peri_acp_types::tasks::BgTaskKind::Workflow,
        summary: "test workflow".into(),
        started_at: "2026-10-30T00:00:00Z".into(),
        status: "running".into(),
        duration_ms: 0,
        output_preview: None,
        initiator_session_id: None,
        child_thread_id: None,
    }
}

fn terminal_result(run_id: &str) -> WorkflowTaskResult {
    WorkflowTaskResult {
        run_id: run_id.into(),
        workflow_name: "test".into(),
        success: true,
        status: WorkflowRunStatus::Completed,
        execution_status: ExecutionStatus::Completed,
        acceptance_status: AcceptanceStatus::Unknown,
        post_processing_status: PostProcessingStatus::NotRequired,
        delivery_status: DeliveryStatus::Unknown,
        state_artifact_exists: false,
        duration_ms: 100,
        agent_count: 0,
        tool_calls_count: 0,
        error: None,
        phase_summaries: Vec::new(),
        attempts: Vec::new(),
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
    let ticket = reg.register(make_run("r1")).unwrap();
    reg.complete(
        &ticket,
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
        task_record("r1"),
    );
    let result = rx.recv().await.unwrap();
    assert_eq!(result.run_id, "r1");
    assert!(result.success);
    assert_eq!(
        reg.pending_completions().len(),
        1,
        "broadcast receipt must not acknowledge MCP delivery"
    );
    assert_eq!(reg.pending_completions()[0].run_id, "r1");
    reg.acknowledge_completion("unrelated");
    assert_eq!(reg.pending_completions().len(), 1);
    reg.acknowledge_completion("r1");
    reg.acknowledge_completion("r1");
    assert!(reg.pending_completions().is_empty());
    assert!(reg.list_runs().is_empty());
    assert!(reg.registrations.lock().is_empty());
    assert!(!reg.complete(&ticket, result, task_record("r1")));
    assert!(
        reg.pending_completions().is_empty(),
        "late completion cannot reopen an acknowledged source"
    );
}

#[tokio::test]
async fn test_complete_retains_unacknowledged_run_with_status() {
    let (reg, _rx) = make_registry();
    let ticket = reg.register(make_run("r1")).unwrap();
    assert_eq!(reg.active_count(), 1);

    reg.complete(
        &ticket,
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
        task_record("r1"),
    );

    // complete 后 active_count 归零
    assert_eq!(reg.active_count(), 0);

    // ACK 前仍保留状态与投递资格；展示历史由 progress/journal 提供。
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
        !notification.contains("<system-reminder") && !notification.contains("</system-reminder>"),
        "通知正文不应自行编码 system-reminder envelope，实际：{notification}"
    );
}

#[test]
fn test_notification_preserves_and_limits_error() {
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
    assert!(notification.contains(secret));
    assert!(notification.contains("https://example.invalid/run?api_key="));
    assert!(!notification.contains("</system-reminder>"));
    assert!(!notification.contains("<system-reminder"));
    assert!(!notification.contains("[redacted]"));
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

/// 测试期间的 run 目录守卫：断言 panic 时也要清理。
///
/// 本用例在进程 cwd（crate 根）下建 `.claude/workflow-runs/<run_id>/`；失败时
/// 原来的收尾 `remove_dir_all` 不执行，遗留目录会以未跟踪文件出现在 `git status`
/// （2026-10-11 复核发现）。
struct RunDirGuard(std::path::PathBuf);

impl Drop for RunDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        // 父目录只在空时删除成功（并行用例可能仍在用）；`workflow-runs` 删掉了才
        // 继续收 `.claude`，避免删掉其它内容所在的目录。
        if let Some(parent) = self.0.parent() {
            if std::fs::remove_dir(parent).is_ok() {
                if let Some(grandparent) = parent.parent() {
                    let _ = std::fs::remove_dir(grandparent);
                }
            }
        }
    }
}

#[test]
fn test_notification_includes_saved_state_only_when_artifact_exists() {
    let run_id = format!("notification-artifact-{}", uuid::Uuid::now_v7());
    let state_dir = std::path::Path::new(".claude/workflow-runs").join(&run_id);
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::write(state_dir.join("state.json"), "{}").unwrap();
    let _cleanup = RunDirGuard(state_dir.clone());

    let result = WorkflowTaskResult {
        run_id: run_id.clone(),
        workflow_name: "test".into(),
        success: true,
        status: WorkflowRunStatus::Completed,
        execution_status: ExecutionStatus::Completed,
        acceptance_status: AcceptanceStatus::Passed,
        post_processing_status: PostProcessingStatus::NotRequired,
        delivery_status: DeliveryStatus::Unknown,
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
    // 读取指引必须用当前 system direct 原名：逐字断言注册表的冻结字面量，
    // 不用查表派生期望值（同源派生会让「查询改坏」自洽通过）。
    assert!(
        notification.contains("Use `Read` to view full results"),
        "存在 artifact 时应以模型面名字引导读取，实际：{notification}"
    );
    // 长文本外置必须对消费方可见，且措辞与提取 gate 一致（对象字段）：不说明
    // `${label}` 是引用，读 state.json 的消费方会把 token 当结果、静默丢长文本
    // （2026-10-11 issue 缺陷 1）。阈值断言取常量展开值而非硬编码数字。
    //
    // 文案不得含尖括号：提醒正文在信封边界统一 HTML 转义，`>200` 会变成 `&gt;200`。
    assert!(
        notification.contains(&format!(
            "Long string fields in the result object (over {} bytes) are externalized",
            peri_acp_types::workflow::LONGTEXT_EXTRACT_THRESHOLD_BYTES
        )),
        "存在 artifact 时应说明长文本外置阈值与作用面，实际：{notification}"
    );
    assert!(
        notification.contains("`${label}` token in state.json is that field's path"),
        "存在 artifact 时应说明 `${{label}}` 是结果对象字段路径，实际：{notification}"
    );
    assert!(
        notification.contains("lives in `outputs/` under the same label with a `.txt` suffix"),
        "存在 artifact 时应说明外置文件位置（同一 label 加 .txt 后缀），实际：{notification}"
    );
    assert!(
        !notification.contains('>') && !notification.contains('<'),
        "提醒正文不得含尖括号（信封边界会转义成实体），实际：{notification}"
    );
    // 终态四维必须词汇化：`Debug` 小写把 `NotRequired` 渲染成 `notrequired`，
    // 与 `unknown` 并列时被读作「未验证」（2026-10-11 issue 缺陷 2）。
    assert!(
        notification.contains("post-condition: passed (read-only verified)"),
        "read_only 通过应读作已验证，实际：{notification}"
    );
    assert!(
        notification.contains("Acceptance is judged outside the engine"),
        "应说明验收不由内核判定，实际：{notification}"
    );
    assert!(
        !notification.contains("notrequired"),
        "终态四维不得再出现 Debug 小写形态，实际：{notification}"
    );
    assert!(!notification.contains("Error:"));
}

#[tokio::test]
async fn cancellation_before_child_attachment_keeps_cleanup_running() {
    let (registry, _) = make_registry();
    let (kill_tx, kill_rx) = tokio::sync::oneshot::channel();
    registry
        .reserve(WorkflowRun {
            run_id: "attaching".into(),
            workflow_name: "test".into(),
            script_preview: String::new(),
            status: WorkflowRunStatus::Running,
            started_at: std::time::Instant::now(),
            child_handle: None,
            kill_tx: Some(kill_tx),
        })
        .unwrap();
    registry.kill("attaching").unwrap();
    let (stopped, observed) = tokio::sync::oneshot::channel();
    let child = tokio::spawn(async move {
        kill_rx.await.unwrap();
        let _ = stopped.send(());
    });
    registry.attach_child("attaching", child);
    tokio::time::timeout(std::time::Duration::from_secs(1), observed)
        .await
        .unwrap()
        .expect("the removed row must not abort its cleanup owner");
}

#[tokio::test]
async fn background_start_waits_for_mcp_projection_receipt() {
    let (registry, _) = make_registry();
    let ticket = registry.register(make_run("run-a")).unwrap();
    let mut pending = Box::pin(ticket.wait_for_observation());
    assert!(futures::poll!(&mut pending).is_pending());
    registry.acknowledge_observation("other-run");
    assert!(futures::poll!(&mut pending).is_pending());
    registry.acknowledge_observation("run-a");
    pending.await;
    // Reconnect retries may observe an acknowledgement before starting their wait.
    ticket.wait_for_observation().await;
}

#[tokio::test]
async fn observation_wait_after_completion_ack_uses_the_original_receipt() {
    let (registry, _) = make_registry();
    let ticket = registry.register(make_run("fast-run")).unwrap();
    assert!(registry.complete(
        &ticket,
        terminal_result("fast-run"),
        task_record("fast-run")
    ));
    registry.acknowledge_completion("fast-run");
    assert!(registry.runs.lock().is_empty());
    assert!(registry.registrations.lock().is_empty());
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        ticket.wait_for_observation(),
    )
    .await
    .expect("ACK before the tool's wait must preserve observation evidence");
}

#[test]
fn acknowledging_many_completions_releases_source_state_and_capacity() {
    let (registry, _) = make_registry();
    for index in 0..600 {
        let run_id = format!("retire-{index}");
        let ticket = registry.register(make_run(&run_id)).unwrap();
        assert!(registry.complete(&ticket, terminal_result(&run_id), task_record(&run_id)));
    }
    assert_eq!(registry.pending_completion_records().len(), 600);
    for index in 0..600 {
        registry.acknowledge_completion(&format!("retire-{index}"));
    }
    assert!(registry.pending_completion_records().is_empty());
    assert!(registry.list_runs().is_empty());
    assert!(registry.registrations.lock().is_empty());
    assert!(registry.registrations.lock().capacity() <= 128);
    assert!(registry.runs.lock().capacity() <= 128);
    assert!(registry.completions.lock().capacity() <= 128);
}
