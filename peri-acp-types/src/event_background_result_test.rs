use super::{BackgroundTaskResult, ShellOutput};

fn shell_result(
    task_id: &str,
    command: &str,
    success: bool,
    timed_out: bool,
    exit_code: Option<i32>,
    stdout_path: Option<&str>,
    stderr_path: Option<&str>,
) -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: task_id.into(),
        agent_name: "bg-shell".into(),
        prompt_summary: command.into(),
        success,
        output: "Shell command failed; read the output files as needed.".into(),
        tool_calls_count: 0,
        duration_ms: 3,
        timed_out,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: Some(Box::new(ShellOutput {
            stdout_path: stdout_path.map(str::to_string),
            stderr_path: stderr_path.map(str::to_string),
            complete: true,
            error: None,
            exit_code,
        })),
    }
}

#[test]
fn background_failure_notification_contains_safe_model_facts() {
    let failure = crate::error::SafeSubagentFailure::new(
        "child-123",
        crate::error::SafeModelErrorDiagnostic::from_model(
            peri_model::ModelError::http_status(429, "provider.example", Some("req-123"))
                .diagnostic(),
        ),
    )
    .expect("safe child failure");
    let result = BackgroundTaskResult {
        task_id: "task-123".into(),
        agent_name: "worker".into(),
        prompt_summary: "prompt summary".into(),
        success: false,
        output: "An LLM API error occurred (HTTP 429). Please try again.".into(),
        tool_calls_count: 1,
        duration_ms: 1,
        timed_out: false,
        child_thread_id: Some("child-123".into()),
        subagent_failure: Some(failure),
        shell_output: None,
    };
    let notification = result.to_notification();
    assert!(notification.contains("model_error_status: 429"));
    assert!(notification.contains("model_error_request_id: req-123"));
    assert!(!notification.contains("prompt summary"));
}

#[test]
fn shell_notification_is_short_and_serde_compatible() {
    let result = shell_result(
        "shell-123456789",
        "printf hello",
        true,
        false,
        Some(0),
        Some("/tmp/stdout.log"),
        Some("/tmp/stderr.log"),
    );
    let mut result = result;
    result.output = "x".repeat(200_000);
    let notification = result.to_notification();
    assert!(notification.len() < 2_000);
    assert!(!notification.contains(&"x".repeat(1_000)));
    // 自足关联字段：完整公开 id、命令、终态、退出码与输出引用都必须内联。
    assert!(notification.contains("task_id: shell-123456789"));
    assert!(notification.contains("command: printf hello"));
    assert!(notification.contains("state: succeeded"));
    assert!(notification.contains("exit_code: 0"));
    assert!(notification.contains("stdout: /tmp/stdout.log"));
    assert!(notification.contains("stderr: /tmp/stderr.log"));
    // 读取指引必须指**模型面名字**（裸名已无提供面）：逐字断言冻结字面量，
    // 不用查表派生期望值（与实现同源派生会在「归一改坏」时自洽通过）。
    assert!(
        notification.contains("请使用 `Read` 工具按需读取"),
        "通知必须用模型面名字引导读取: {notification}"
    );
    let mut legacy = serde_json::to_value(&result).expect("serialize");
    legacy
        .as_object_mut()
        .expect("object")
        .remove("shell_output");
    let decoded: BackgroundTaskResult = serde_json::from_value(legacy).expect("legacy decode");
    assert!(decoded.shell_output.is_none());
}

// T16：两个前 8 位相同的任务 id 必须各自按完整 id 关联命令与输出路径，
// 不得用短 id 导致通知互相串线。
#[test]
fn async_result_notification_keeps_full_task_identity_for_similar_ids() {
    let first = shell_result(
        "shell-01a2b3c4d5e6f7a8",
        "printf alpha",
        true,
        false,
        Some(0),
        Some("/tmp/alpha.stdout.log"),
        Some("/tmp/alpha.stderr.log"),
    );
    let second = shell_result(
        "shell-01a2b3c4d5e6f7a9",
        "printf beta",
        false,
        false,
        Some(1),
        Some("/tmp/beta.stdout.log"),
        Some("/tmp/beta.stderr.log"),
    );
    assert_eq!(
        first.task_id.chars().take(8).collect::<String>(),
        second.task_id.chars().take(8).collect::<String>(),
        "夹具必须让短 id 冲突，才能覆盖回归"
    );

    let first_notification = first.to_notification();
    assert!(first_notification.contains("task_id: shell-01a2b3c4d5e6f7a8"));
    assert!(!first_notification.contains("shell-01a2b3c4d5e6f7a9"));
    assert!(first_notification.contains("command: printf alpha"));
    assert!(first_notification.contains("stdout: /tmp/alpha.stdout.log"));
    assert!(!first_notification.contains("/tmp/beta.stdout.log"));

    let second_notification = second.to_notification();
    assert!(second_notification.contains("task_id: shell-01a2b3c4d5e6f7a9"));
    assert!(!second_notification.contains("shell-01a2b3c4d5e6f7a8"));
    assert!(second_notification.contains("command: printf beta"));
    assert!(second_notification.contains("stdout: /tmp/beta.stdout.log"));
    assert!(!second_notification.contains("/tmp/alpha.stdout.log"));
}

// T17：状态、exit_code 与输出引用必须准确；失败/超时不得投影为成功，
// 未知退出码不得猜值。
#[test]
fn async_result_notification_maps_terminal_state_and_evidence() {
    let succeeded = shell_result(
        "shell-ok",
        "printf ok",
        true,
        false,
        Some(0),
        Some("/tmp/ok.stdout.log"),
        Some("/tmp/ok.stderr.log"),
    )
    .to_notification();
    assert!(succeeded.contains("state: succeeded"));
    assert!(succeeded.contains("exit_code: 0"));

    let failed = shell_result(
        "shell-failed",
        "false",
        false,
        false,
        Some(7),
        Some("/tmp/failed.stdout.log"),
        Some("/tmp/failed.stderr.log"),
    )
    .to_notification();
    assert!(failed.contains("state: failed"));
    assert!(!failed.contains("state: succeeded"));
    assert!(failed.contains("exit_code: 7"));
    assert!(failed.contains("stdout: /tmp/failed.stdout.log"));
    assert!(failed.contains("stderr: /tmp/failed.stderr.log"));

    let timed_out = shell_result(
        "shell-timeout",
        "sleep 60",
        false,
        true,
        None,
        Some("/tmp/timeout.stdout.log"),
        None,
    )
    .to_notification();
    assert!(timed_out.contains("state: timed_out"));
    assert!(!timed_out.contains("state: succeeded"));
    assert!(timed_out.contains("exit_code: unknown"));
    assert!(timed_out.contains("stderr: unknown"));

    // shell 结果缺失（无文件证据）也不得猜退出码。
    let no_evidence = BackgroundTaskResult {
        task_id: "shell-no-evidence".into(),
        agent_name: "bg-shell".into(),
        prompt_summary: "missing evidence".into(),
        success: false,
        output: "Shell command failed; read the output files as needed.".into(),
        tool_calls_count: 0,
        duration_ms: 1,
        timed_out: false,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: None,
    }
    .to_notification();
    assert!(no_evidence.contains("state: failed"));
    assert!(no_evidence.contains("exit_code: unknown"));
    assert!(no_evidence.contains("stdout: unknown"));
    assert!(no_evidence.contains("stderr: unknown"));
}

// 超长命令安全截断并标注；完整 id 与状态不截断。
#[test]
fn async_result_notification_bounds_command_and_marks_truncation() {
    let notification = shell_result(
        "shell-bounded-command-0123456789",
        &"x".repeat(5_000),
        true,
        false,
        Some(0),
        Some("/tmp/bounded.stdout.log"),
        None,
    )
    .to_notification();
    assert!(notification.len() < 2_000);
    assert!(notification.contains("…[truncated]"));
    assert!(notification.contains("task_id: shell-bounded-command-0123456789"));
    assert!(notification.contains("state: succeeded"));
    assert!(notification.contains("stdout: /tmp/bounded.stdout.log"));
}

// unresolved 不得伪装成 timed_out/failed，也不得声称远端已停止。
#[test]
fn async_result_notification_marks_unresolved_without_claiming_stop() {
    let notification = BackgroundTaskResult {
        task_id: "mcp-01a2b3c4d5e6f7a8".into(),
        agent_name: "mcp".into(),
        prompt_summary: "unobservable task owner".into(),
        success: false,
        output: "The task owner stayed unobservable for the whole bounded retry window and \
                 the task was abandoned as unresolved. Its remote side effects are unknown; \
                 check the owner directly before assuming it stopped."
            .into(),
        tool_calls_count: 0,
        duration_ms: 0,
        timed_out: true,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: None,
    }
    .to_notification();
    assert!(notification.contains("task_id: mcp-01a2b3c4d5e6f7a8"));
    assert!(notification.contains("state: unresolved"));
    assert!(notification.contains("结果未知"));
    assert!(notification.contains("副作用未知"));
    assert!(!notification.contains("state: timed_out"));
    assert!(!notification.contains("state: failed"));
    assert!(!notification.contains("进程组已终止"));
}

// 取消是独立终态，不能被归为普通失败。
#[test]
fn async_result_notification_marks_cancelled_distinct_from_failure() {
    let mut result = shell_result(
        "shell-cancelled",
        "sleep 60",
        false,
        false,
        None,
        Some("/tmp/cancelled.stdout.log"),
        None,
    );
    result.output = "Shell command cancelled; read the output files as needed.".into();
    let notification = result.to_notification();
    assert!(notification.contains("state: cancelled"));
    assert!(notification.contains("任务已取消"));
    assert!(!notification.contains("state: failed"));

    let subagent = BackgroundTaskResult {
        task_id: "task-interrupted".into(),
        agent_name: "worker".into(),
        prompt_summary: "prompt".into(),
        success: false,
        output: "Background sub-agent was interrupted".into(),
        tool_calls_count: 0,
        duration_ms: 1,
        timed_out: false,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: None,
    }
    .to_notification();
    assert!(subagent.contains("state: cancelled"));
    assert!(!subagent.contains("state: failed"));
    assert!(!subagent.contains("state: succeeded"));
}
