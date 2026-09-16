//! `Bash` 工具层投影：三字段 → 前台/后台文本、提升文案与结构化输出（FC-BASH-02/03/04）。
//!
//! 工具层经**唯一任务注册表**驱动真实进程（单进程形态下没有可注入的 RPC 替身），
//! 因此这里断言的是面向调用方的文本与 `structuredContent` 在真实任务上的取值；
//! 进程组与信号细节见 `tests/bash_lifecycle.rs`。

use std::sync::Arc;
use std::time::Duration;

use local_mcp_server::tasks::log::{DirOutputPersist, OutputPersist};
use local_mcp_server::tasks::registry::{task_resource_uri, TaskRegistry, TaskRegistryConfig};
use local_mcp_server::tasks::{BashTaskConfig, BashTasks};
use local_mcp_server::tools::bash::{BashTool, HOST_OUTPUT_CHAR_LIMIT};
use local_mcp_server::wire::{RequestContext, TaskStatus};
use serde_json::json;

struct Harness {
    tool: BashTool,
    _workspace: tempfile::TempDir,
    _logs: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        Self::with_limit(5)
    }

    fn with_limit(shell_limit: usize) -> Self {
        let workspace = tempfile::TempDir::new().expect("workspace");
        let logs = tempfile::TempDir::new().expect("logs");
        let persist: Arc<dyn OutputPersist> = Arc::new(DirOutputPersist::new(logs.path()));
        let mut config = TaskRegistryConfig::new(persist);
        config.poll_interval = None;
        config.shell_limit = shell_limit;
        let bash = Arc::new(BashTasks::new(BashTaskConfig::new(
            workspace.path(),
            logs.path(),
        )));
        let registry =
            TaskRegistry::new(bash, Arc::new(local_mcp_server::tasks::SystemClock), config);
        Self {
            tool: BashTool::new(registry),
            _workspace: workspace,
            _logs: logs,
        }
    }

    /// 工作目录不存在的夹具：用于稳定的 spawn 失败。
    fn with_missing_workspace() -> Self {
        let root = tempfile::TempDir::new().expect("root");
        let logs = tempfile::TempDir::new().expect("logs");
        let persist: Arc<dyn OutputPersist> = Arc::new(DirOutputPersist::new(logs.path()));
        let mut config = TaskRegistryConfig::new(persist);
        config.poll_interval = None;
        let bash = Arc::new(BashTasks::new(BashTaskConfig::new(
            root.path().join("does-not-exist"),
            logs.path(),
        )));
        let registry =
            TaskRegistry::new(bash, Arc::new(local_mcp_server::tasks::SystemClock), config);
        Self {
            tool: BashTool::new(registry),
            _workspace: root,
            _logs: logs,
        }
    }

    fn context(&self) -> RequestContext {
        RequestContext::new("req", "alice", "conn-1")
    }

    async fn invoke(&self, arguments: serde_json::Value) -> local_mcp_server::wire::ToolResponse {
        self.tool.invoke(&arguments, &self.context()).await
    }

    /// 收尾：终止所有在跑任务，避免测试留下孤儿进程。
    async fn shutdown(&self) {
        self.tool.registry().close().await.expect("关闭");
    }
}

#[tokio::test]
async fn foreground_completion_returns_tool_output() {
    let harness = Harness::new();
    let response = harness
        .invoke(json!({ "command": "printf 'hello\\n'" }))
        .await;
    assert!(!response.is_error, "{}", response.text);
    assert_eq!(response.text, "hello\n");
    assert_eq!(response.structured["tool"], json!("Bash"));
    assert_eq!(response.structured["ok"], json!(true));
    assert_eq!(response.structured["exit_code"], json!(0));
    assert_eq!(response.structured["status"], json!("completed"));
    assert!(
        response.structured["elapsed_ms"].is_number(),
        "真实现必须报告真实耗时"
    );
    assert!(
        response.structured["task_id"].is_null(),
        "前台完成不返回 task_id"
    );
    // 两层限额都必须登记。
    assert_eq!(
        response.structured["host_projection"]["output_char_limit_chars"],
        json!(HOST_OUTPUT_CHAR_LIMIT)
    );
    assert_eq!(
        response.structured["tool_limits"]["max_output_chars"],
        json!(65_000)
    );
    assert_eq!(
        response.structured["tool_limits"]["max_output_lines"],
        json!(2_000)
    );
}

#[tokio::test]
async fn explicit_background_message_keeps_source_lines() {
    let harness = Harness::new();
    let response = harness
        .invoke(json!({ "command": "sleep 30", "run_in_background": true }))
        .await;
    assert!(!response.is_error, "显式后台是成功调用: {}", response.text);
    assert!(response
        .text
        .starts_with("Background shell task started.\n"));
    assert!(response.text.contains("\ntask_id: shell-"));
    let pid = response.structured["pid"].as_u64().expect("真 pid");
    assert!(
        response.text.contains(&format!("\npid: {pid}\n")),
        "{}",
        response.text
    );
    assert!(response
        .text
        .contains(&format!("- Kill it: run `kill {pid}` in another shell command (`kill -- -{pid}` kills the whole process group including child processes)")));
    assert!(response.text.contains("- Live output: Read the log file "));
    assert!(response
        .text
        .contains("— it appends while the command runs (use the Read tool to view)"));
    let task_id = response.structured["task_id"].as_str().expect("task_id");
    assert!(
        response.text.contains(&format!(
            "- Monitor: read the resource `{}`",
            task_resource_uri(task_id)
        )),
        "Monitor 一句指向真实资源路径: {}",
        response.text
    );
    assert_eq!(response.structured["status"], json!("running"));
    assert_eq!(response.structured["promoted"], json!(false));
    assert_eq!(response.structured["timed_out"], json!(false));

    harness.shutdown().await;
}

#[tokio::test]
async fn foreground_timeout_promotion_reports_progressing_task() {
    let harness = Harness::new();
    let response = harness
        .invoke(json!({ "command": "printf 'starting up\\n'; sleep 30", "timeout": 200 }))
        .await;
    assert!(response.is_error, "前台超时是 tool error");
    assert!(
        response.text.starts_with(
            "Command timed out after 0.2s. The process is still running and has been promoted to a background task (it was producing output, so it is likely progressing)."
        ),
        "{}",
        response.text
    );
    assert!(response.text.contains("\ntask_id: shell-"));
    let pid = response.structured["pid"].as_u64().expect("真 pid");
    assert!(response.text.contains(&format!("\npid: {pid}\n")));
    assert!(response.text.contains(
        "- It continues running in the background; you will be notified when it completes."
    ));
    assert!(response.text.contains("[Partial output saved to "));
    assert!(response
        .text
        .contains("Command that timed out: printf 'starting up\\n'; sleep 30"));
    assert_eq!(response.structured["promoted"], json!(true));
    assert_eq!(response.structured["status"], json!("running"));
    assert!(response.structured["task_id"].is_string());
    // F-007 回归：错误分支的 structuredContent 必须与 `isError` 一致
    //（`ok == !isError`），不得再出现 `ok: true` + `isError: true` 的矛盾；
    // 任务级字段保持真实（进程被提升且在跑，未被期限终止）。
    assert_eq!(
        response.structured["ok"],
        json!(false),
        "前台超时是 tool error，structuredContent 的 ok 必须为 false"
    );
    assert_eq!(
        response.structured["error"],
        json!(response.text),
        "错误文本必须与文本字段逐字一致"
    );
    assert_eq!(
        response.structured["timed_out"],
        json!(false),
        "提升不终止进程：任务级 timed_out 保持真实值"
    );
    // 提升后的任务在资源面可见（可查询/可停止）。
    let task_id = response.structured["task_id"].as_str().unwrap();
    assert!(
        harness
            .tool
            .registry()
            .snapshot_for("alice", "conn-1", task_id)
            .is_some(),
        "提升任务必须可经资源面查询"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn foreground_timeout_without_output_reports_stall_diagnosis() {
    let harness = Harness::new();
    let response = harness
        .invoke(json!({ "command": "sleep 30", "timeout": 1_000 }))
        .await;
    assert!(response.is_error);
    assert!(
        response.text.starts_with(
            "Command timed out after 1.0s with no output produced. The process is still running and has been promoted to a background task, but it may never complete on its own."
        ),
        "{}",
        response.text
    );
    assert!(response.text.contains("\nLikely causes:\n"));
    assert!(response
        .text
        .contains("- The command is waiting for input or for a resource (network, lock, another process) that will never arrive."));
    let pid = response.structured["pid"].as_u64().expect("真 pid");
    assert!(response.text.contains(&format!(
        "If it does not complete on its own, terminate it: run `kill {pid}`"
    )));
    assert_eq!(
        response.structured["ok"],
        json!(false),
        "无输出分支同样是 tool error（F-007 两版文案都必须自洽）"
    );
    assert_eq!(response.structured["status"], json!("running"));

    harness.shutdown().await;
}

#[tokio::test]
async fn promotion_failure_reports_terminated_process_group() {
    // shell_limit = 1：先占满，再让前台超时 → 提升失败。
    let harness = Harness::with_limit(1);
    let first = harness
        .invoke(json!({ "command": "sleep 30", "run_in_background": true }))
        .await;
    assert!(!first.is_error, "第一个后台任务应成功: {}", first.text);

    let response = harness
        .invoke(json!({ "command": "sleep 30", "timeout": 2_000 }))
        .await;
    assert!(response.is_error);
    assert!(
        response.text.starts_with(
            "Command timed out after 2.0s and could not be promoted to a background task: Maximum 1 concurrent background tasks reached. The process group has been terminated."
        ),
        "{}",
        response.text
    );
    assert!(response.text.contains("Command that timed out: sleep 30"));
    assert_eq!(response.structured["ok"], json!(false));
    assert_eq!(response.structured["error"], json!(response.text));
    assert_eq!(
        harness.tool.registry().list(&harness.context()).len(),
        1,
        "提升失败的任务不进入注册表"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn explicit_background_capacity_error_uses_registry_text() {
    let harness = Harness::with_limit(1);
    harness
        .invoke(json!({ "command": "sleep 30", "run_in_background": true }))
        .await;
    let response = harness
        .invoke(json!({ "command": "sleep 30", "run_in_background": true }))
        .await;
    assert!(response.is_error);
    assert_eq!(
        response.text,
        "Maximum 1 concurrent background tasks reached"
    );
    assert_eq!(response.structured["ok"], json!(false));

    harness.shutdown().await;
}

#[tokio::test]
async fn missing_command_and_spawn_failure_are_tool_errors() {
    let harness = Harness::new();
    let response = harness.invoke(json!({ "timeout": 100 })).await;
    assert!(response.is_error);
    assert_eq!(response.text, "Missing command parameter");
    assert_eq!(response.structured["tool"], json!("Bash"));
    assert_eq!(response.structured["ok"], json!(false));
    assert_eq!(
        response.structured["error"],
        json!("Missing command parameter")
    );

    // 工作目录不存在 → spawn 失败，文本保留底层原因。
    let broken = Harness::with_missing_workspace();
    let response = broken.invoke(json!({ "command": "nope" })).await;
    assert!(response.is_error);
    assert!(
        response.text.starts_with("Error executing command:"),
        "{}",
        response.text
    );
    assert!(
        response.text.contains("No such file or directory"),
        "spawn 失败必须保留底层原因: {}",
        response.text
    );
}

#[tokio::test]
async fn background_task_survives_request_completion() {
    let harness = Harness::new();
    let ctx = harness.context();
    let response = harness
        .invoke(json!({ "command": "sleep 30", "run_in_background": true }))
        .await;
    let task_id = response.structured["task_id"].as_str().unwrap().to_string();

    // 请求上下文被取消（modern 的请求结束语义）不应终止后台任务。
    ctx.cancellation.cancel();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let snapshot = harness
        .tool
        .registry()
        .snapshot_for("alice", "conn-1", &task_id)
        .expect("任务仍应存在");
    assert_eq!(snapshot.status, TaskStatus::Running);

    harness.shutdown().await;
}
