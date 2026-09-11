use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use peri_acp_types::tools::{BaseTool, ToolContext};

use super::*;
use crate::protocol::{AgentRunParams, AgentRunResult, Usage};
use crate::runner::AgentExecutor;

struct CountingExecutor {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl AgentExecutor for CountingExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        AgentRunResult::Ok {
            output: "unexpected".into(),
            usage: Usage { output_tokens: 1 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        }
    }
}

fn make_tool(cwd: &str, calls: Arc<AtomicUsize>) -> (WorkflowTool, Arc<WorkflowTaskRegistry>) {
    let executor = Arc::new(CountingExecutor { calls }) as Arc<dyn AgentExecutor>;
    let runner = Arc::new(WorkflowRunner::new(executor, cwd, None));
    let (notification_tx, _) = tokio::sync::broadcast::channel(4);
    let registry = Arc::new(WorkflowTaskRegistry::new(notification_tx));
    let tool = WorkflowTool::new(
        runner,
        Arc::clone(&registry),
        Arc::new(WorkflowProgressStore::new()),
        Arc::new(WorkflowJournalStore::new(cwd)),
    );
    (tool, registry)
}

#[test]
fn workflow_schema_exposes_budget_total_integer_bounds() {
    let tmp = tempfile::TempDir::new().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (tool, _) = make_tool(tmp.path().to_str().unwrap(), calls);

    let schema = tool.parameters();
    let budget = &schema["properties"]["budgetTotal"];

    assert_eq!(budget["type"], "integer");
    assert_eq!(budget["minimum"], 1);
    assert_eq!(budget["maximum"], MAX_SAFE_BUDGET_TOTAL);
    assert!(budget["description"]
        .as_str()
        .unwrap()
        .contains("total token budget"));
}

#[test]
fn parse_budget_total_accepts_omitted_and_safe_integer_bounds() {
    assert_eq!(parse_budget_total(&serde_json::json!({})).unwrap(), None);
    assert_eq!(
        parse_budget_total(&serde_json::json!({"budgetTotal": 1})).unwrap(),
        Some(1)
    );
    assert_eq!(
        parse_budget_total(&serde_json::json!({
            "budgetTotal": MAX_SAFE_BUDGET_TOTAL
        }))
        .unwrap(),
        Some(MAX_SAFE_BUDGET_TOTAL)
    );
}

#[test]
fn parse_budget_total_rejects_invalid_values_with_stable_range_error() {
    let invalid = [
        serde_json::json!(null),
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!("1000"),
        serde_json::json!(true),
        serde_json::json!(MAX_SAFE_BUDGET_TOTAL + 1),
    ];

    for value in invalid {
        let error = parse_budget_total(&serde_json::json!({"budgetTotal": value})).unwrap_err();
        assert_eq!(
            error,
            format!("'budgetTotal' must be an integer between 1 and {MAX_SAFE_BUDGET_TOTAL}")
        );
    }
}

#[test]
fn parse_host_limits_rejects_invalid_values() {
    for field in ["maxAgents", "maxToolCalls", "maxElapsedMs"] {
        let input = serde_json::json!({(field): 0});
        let error = parse_bounded_integer(&input, field, None, MAX_SAFE_INTEGER).unwrap_err();
        assert!(error.contains(field));
    }
    assert!(parse_bounded_integer(
        &serde_json::json!({"maxConcurrency": 17}),
        "maxConcurrency",
        Some(3),
        MAX_CONCURRENCY_CAP,
    )
    .is_err());
}

#[tokio::test]
async fn invalid_script_fails_before_workflow_side_effects() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (tool, registry) = make_tool(cwd, Arc::clone(&calls));

    let error = tool
        .invoke(
            serde_json::json!({
                "script": "export const meta = { name: 'broken', description: 'test' }; const = nope"
            }),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("Workflow preflight failed"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(registry.active_count(), 0);
    assert!(!tmp.path().join(".claude/workflow-runs").exists());
}

#[tokio::test]
async fn strict_preflight_fails_before_workflow_side_effects() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (tool, registry) = make_tool(cwd, Arc::clone(&calls));

    let error = tool
        .invoke(
            serde_json::json!({
                "script": "export const meta = { name: 'strict' }",
                "strictPreflight": true
            }),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("cannot be statically validated"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(registry.active_count(), 0);
    assert!(!tmp.path().join(".claude/workflow-runs").exists());
}

#[tokio::test]
async fn invalid_write_intent_fails_before_workflow_side_effects() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (tool, registry) = make_tool(cwd, Arc::clone(&calls));

    let error = tool
        .invoke(
            serde_json::json!({
                "script": "export const meta = { name: 'invalid-intent' }",
                "writeIntent": {"kind": "write", "cwd": cwd}
            }),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("Invalid writeIntent"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(registry.active_count(), 0);
    assert!(!tmp.path().join(".claude/workflow-runs").exists());
}

#[tokio::test]
async fn invalid_budget_fails_before_workflow_side_effects() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (tool, registry) = make_tool(cwd, Arc::clone(&calls));
    let resume_id = uuid::Uuid::now_v7().to_string();

    let error = tool
        .invoke(
            serde_json::json!({
                "script": "export const meta = { name: 'invalid-budget', description: 'test' }; return 'ok'",
                "budgetTotal": 0,
                "resumeFromRunId": resume_id
            }),
            ToolContext::new(&[], cwd),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("'budgetTotal'"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(registry.active_count(), 0);
    assert!(!tmp.path().join(".claude/workflow-runs").exists());
}

// 每项真实 Node 生命周期测试使用独立进程和 HOME，避免污染用户 artifact cache。
fn isolated_completion_child(test: &str) -> bool {
    if std::env::var("PERI_WORKFLOW_COMPLETION_TEST").as_deref() == Ok(test) {
        return false;
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("PERI_WORKFLOW_COMPLETION_TEST", test)
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "child exact filter must run one test: {test}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

struct GatedCompletionExecutor {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl AgentExecutor for GatedCompletionExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        self.entered.notify_one();
        self.release.notified().await;
        AgentRunResult::Ok {
            output: "finished".into(),
            usage: Usage { output_tokens: 1 },
            model: None,
            tool_count: Some(0),
            token_count: Some(1),
            phase: None,
            duration_ms: Some(1),
        }
    }
}

fn make_completion_tool(
    cwd: &str,
) -> (
    WorkflowTool,
    Arc<WorkflowTaskRegistry>,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
) {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let runner = Arc::new(WorkflowRunner::new(
        Arc::new(GatedCompletionExecutor {
            entered: entered.clone(),
            release: release.clone(),
        }),
        cwd,
        None,
    ));
    let (tx, _) = tokio::sync::broadcast::channel(4);
    let registry = Arc::new(WorkflowTaskRegistry::new(tx));
    let tool = WorkflowTool::new(
        runner,
        registry.clone(),
        Arc::new(WorkflowProgressStore::new()),
        Arc::new(WorkflowJournalStore::new(cwd)),
    );
    (tool, registry, entered, release)
}

/// [回归测试] 调用future在快速检测窗口被取消，后台run仍须完成登记并广播一次。
#[test]
fn test_invoke_cancel_during_fast_window_keeps_completion_owner() {
    if isolated_completion_child(
        "tool::tests::test_invoke_cancel_during_fast_window_keeps_completion_owner",
    ) {
        return;
    }
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap().block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let (tool, registry, entered, release) = make_completion_tool(dir.path().to_str().unwrap());
        let mut notifications = registry.notification_tx().subscribe();
        let mut invoke = Box::pin(tool.invoke(serde_json::json!({"script": "export const meta = { name: 'fixture', description: 'lifecycle' }; return await agent('finish');"}), ToolContext::new(&[], dir.path().to_str().unwrap())));
        std::future::poll_fn(|cx| {
            let result = invoke.as_mut().poll(cx);
            assert!(result.is_pending(), "必须在invoke尚未返回的启动窗口取消");
            if registry.active_count() == 1 { std::task::Poll::Ready(()) } else { std::task::Poll::Pending }
        }).await;
        drop(invoke);
        tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified()).await.expect("真实Node须派发agent");
        release.notify_one();
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), notifications.recv()).await.expect("取消调用者不能丢失完成通知").unwrap();
        assert_eq!(result.status, WorkflowRunStatus::Completed);
        assert_eq!(registry.active_count(), 0);
        assert_eq!(result.agent_count, 1);
        assert!(notifications.try_recv().is_err(), "同一run只结算一次");
    });
}

/// [回归测试] 快速窗口中的kill也必须保留Killed，不能被另一路完成映射改成Failed。
#[test]
fn test_invoke_fast_kill_preserves_terminal_projection() {
    if isolated_completion_child("tool::tests::test_invoke_fast_kill_preserves_terminal_projection")
    {
        return;
    }
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap().block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let (tool, registry, _entered, _release) = make_completion_tool(dir.path().to_str().unwrap());
        let mut notifications = registry.notification_tx().subscribe();
        let mut invoke = Box::pin(tool.invoke(serde_json::json!({"script": "export const meta = { name: 'fixture', description: 'lifecycle' }; return await agent('finish');"}), ToolContext::new(&[], dir.path().to_str().unwrap())));
        std::future::poll_fn(|cx| {
            assert!(invoke.as_mut().poll(cx).is_pending());
            if registry.active_count() == 1 { std::task::Poll::Ready(()) } else { std::task::Poll::Pending }
        }).await;
        let run_id = registry.list_runs()[0].0.clone();
        registry.kill(&run_id).unwrap();
        assert!(tokio::time::timeout(std::time::Duration::from_secs(10), invoke).await.unwrap().is_err());
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), notifications.recv()).await.unwrap().unwrap();
        assert_eq!(result.status, WorkflowRunStatus::Killed);
        assert_eq!(result.execution_status, peri_acp_types::workflow::ExecutionStatus::Killed);
        assert!(notifications.try_recv().is_err());
    });
}

/// [回归测试] preflight 被取消时不能留下验证进程或脚本目录。
#[cfg(unix)]
#[test]
fn test_preflight_cancellation_releases_process_and_script() {
    if isolated_completion_child(
        "tool::tests::test_preflight_cancellation_releases_process_and_script",
    ) {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir().unwrap();
    let probe = fixture.path().join("probe");
    let node = fixture.path().join("node");
    std::fs::write(&node, "#!/bin/sh\nprintf '%s\\n' \"$$\" \"$1\" > \"$PERI_WORKFLOW_PREFLIGHT_PROBE\"\nexec /bin/sleep 30\n").unwrap();
    std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).unwrap();
    // 当前测试只在独立子进程中设置环境，且 runtime 尚未启动。
    std::env::set_var("PATH", fixture.path());
    std::env::set_var("PERI_WORKFLOW_PREFLIGHT_PROBE", &probe);
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut validation = Box::pin(preflight_validate_script("return 'ok'"));
        let observed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    result = &mut validation => panic!("验证进程必须先停在 fixture 门控处: {result:?}"),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
                }
                if let Ok(contents) = std::fs::read_to_string(&probe) {
                    let lines: Vec<_> = contents.lines().collect();
                    if lines.len() == 2 {
                        break (lines[0].to_owned(), std::path::PathBuf::from(lines[1]));
                    }
                }
            }
        }).await.expect("真实进程必须启动并报告 PID 与脚本路径");
        drop(validation);
        let directory = observed.1.parent().unwrap();
        let directory_removed = !directory.exists();
        let reaped = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let alive = tokio::process::Command::new("/bin/kill").args(["-0", &observed.0])
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                    .status().await.unwrap().success();
                if !alive { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.is_ok();
        // 旧实现红灯也清理 fixture，不能让测试本身留下后台进程或目录。
        if !reaped {
            let _ = tokio::process::Command::new("/bin/kill").args(["-KILL", &observed.0]).status().await;
        }
        if !directory_removed { let _ = std::fs::remove_dir_all(directory); }
        assert!(directory_removed && reaped, "取消后目录释放={directory_removed}，进程回收={reaped}");
    });
}
