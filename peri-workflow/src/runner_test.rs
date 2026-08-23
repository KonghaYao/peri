// ─── E2E 集成测试（需要 @peri-code/workflow 已安装）──────────────

use super::{
    parse_agent_run_params, parse_run_scoped, validate_start_ack, workflow_local_dist_in,
    AgentExecutor, JournalTruncateParams, WorkflowDoneParams, WorkflowInput, WorkflowRunner,
};
use crate::journal::WorkflowJournalStore;
use crate::progress::{RunStatus, WorkflowProgressStore};
use crate::protocol::{AgentRunParams, AgentRunResult, Usage};
use std::sync::Arc;

/// Mock executor: 返回固定结果（delay 用于模拟慢 agent，保证 kill 测试的窗口）
struct MockAgentExecutor {
    delay: std::time::Duration,
}

#[async_trait::async_trait]
impl AgentExecutor for MockAgentExecutor {
    async fn execute(&self, params: AgentRunParams) -> AgentRunResult {
        tokio::time::sleep(self.delay).await;
        let preview = &params.prompt[..20.min(params.prompt.len())];
        AgentRunResult::Ok {
            output: format!("mock response to: {preview}").into(),
            usage: Usage { output_tokens: 10 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        }
    }
}

#[test]
fn test_agent_run_params_preserve_requested_model() {
    let params = parse_agent_run_params(
        Some(serde_json::json!({
            "runId": "run-1",
            "agentId": 7,
            "prompt": "inspect",
            "model": "sonnet"
        })),
        "run-1",
    )
    .unwrap();

    assert_eq!(params.model.as_deref(), Some("sonnet"));
}

#[test]
fn test_agent_run_params_reject_invalid_model_type() {
    let result = parse_agent_run_params(
        Some(serde_json::json!({
            "runId": "run-1",
            "agentId": 7,
            "prompt": "inspect",
            "model": 42
        })),
        "run-1",
    );

    assert!(result.is_err());
}

#[test]
fn test_agent_run_params_reject_missing_params() {
    assert!(parse_agent_run_params(None, "run-1").is_err());
}

#[test]
fn test_agent_run_params_reject_cross_run_identity() {
    let result = parse_agent_run_params(
        Some(serde_json::json!({
            "runId": "other-run",
            "agentId": 7,
            "prompt": "inspect"
        })),
        "run-1",
    );

    assert_eq!(
        result.unwrap_err(),
        "runId does not match the active workflow run"
    );
}

#[test]
fn test_run_scoped_done_rejects_cross_run_identity() {
    let result = parse_run_scoped::<WorkflowDoneParams>(
        Some(serde_json::json!({
            "runId": "other-run",
            "status": "completed"
        })),
        "run-1",
    );

    assert_eq!(
        result.unwrap_err(),
        "runId does not match the active workflow run"
    );
}

#[test]
fn test_run_scoped_journal_rejects_missing_run_identity() {
    let result = parse_run_scoped::<JournalTruncateParams>(Some(serde_json::json!({})), "run-1");

    assert_eq!(result.err(), Some("invalid run-scoped RPC parameters"));
}

#[test]
fn test_start_ack_accepts_matching_protocol_and_build() {
    validate_start_ack(serde_json::json!({
        "ok": true,
        "protocolVersion": 1,
        "buildId": "@peri-code/workflow@0.2.0"
    }))
    .unwrap();
}

#[test]
fn test_start_ack_rejects_protocol_mismatch() {
    let error = validate_start_ack(serde_json::json!({
        "ok": true,
        "protocolVersion": 2,
        "buildId": "@peri-code/workflow@0.2.0"
    }))
    .unwrap_err();

    assert!(error.to_string().contains("protocol mismatch"));
}

#[test]
fn test_workflow_local_dist_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    assert!(workflow_local_dist_in(tmp.path()).is_none());
}

#[test]
fn test_workflow_local_dist_found() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dist = tmp
        .path()
        .join("node_modules")
        .join("@peri-code")
        .join("workflow")
        .join("dist")
        .join("peri-workflow.js");
    std::fs::create_dir_all(dist.parent().unwrap()).unwrap();
    std::fs::write(&dist, "#!/usr/bin/env node\n").unwrap();
    std::fs::write(
        dist.parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("package.json"),
        serde_json::json!({
            "name": "@peri-code/workflow",
            "version": "0.2.0",
            "main": "dist/peri-workflow.js",
            "periProtocolVersion": 1,
            "periBuildId": "@peri-code/workflow@0.2.0"
        })
        .to_string(),
    )
    .unwrap();
    let got = workflow_local_dist_in(tmp.path()).unwrap();
    assert_eq!(std::path::PathBuf::from(got), dist.canonicalize().unwrap());
}

#[test]
fn test_workflow_local_dist_rejects_wrong_version() {
    let tmp = tempfile::TempDir::new().unwrap();
    let package = tmp
        .path()
        .join("node_modules")
        .join("@peri-code")
        .join("workflow");
    std::fs::create_dir_all(package.join("dist")).unwrap();
    std::fs::write(package.join("dist/peri-workflow.js"), "entry").unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{"name":"@peri-code/workflow","version":"0.1.0","main":"dist/peri-workflow.js"}"#,
    )
    .unwrap();

    assert!(workflow_local_dist_in(tmp.path()).is_none());
}

#[test]
fn test_workflow_local_dist_rejects_escaping_entry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let package = tmp
        .path()
        .join("node_modules")
        .join("@peri-code")
        .join("workflow");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(tmp.path().join("outside.js"), "entry").unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{"name":"@peri-code/workflow","version":"0.2.0","main":"../../../../outside.js"}"#,
    )
    .unwrap();

    assert!(workflow_local_dist_in(tmp.path()).is_none());
}

#[tokio::test]
#[ignore = "requires @peri-code/workflow installed"]
async fn test_e2e_simple_workflow() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();

    let executor = Arc::new(MockAgentExecutor {
        delay: std::time::Duration::ZERO,
    }) as Arc<dyn AgentExecutor>;
    let runner = WorkflowRunner::new(executor, cwd, None);
    let journal = Arc::new(WorkflowJournalStore::new(cwd));
    let progress = Arc::new(WorkflowProgressStore::new());
    let (done_tx, mut done_rx) = tokio::sync::watch::channel(None);
    let (_kill_tx, kill_rx) = tokio::sync::oneshot::channel();

    let script = r#"
export const meta = { name: 'test-workflow', description: 'simple test' }
const result = await agent('say hello')
return { output: result }
"#;

    let input = WorkflowInput {
        script: script.to_string(),
        args: None,
        max_concurrency: 3,
        budget_total: None,
        workflow_name: "test-workflow".to_string(),
        resume_from: None,
    };

    let run_id = uuid::Uuid::now_v7().to_string();
    runner
        .run(run_id, input, progress, journal, done_tx, kill_rx)
        .await
        .unwrap();

    let _ = done_rx.changed().await; // 等待完成信号
    let result = done_rx.borrow().clone().unwrap();
    // 打印调试信息
    eprintln!("=== WORKFLOW RESULT ===");
    eprintln!("status: {}", result.status);
    eprintln!("error: {:?}", result.error);
    eprintln!("stderr_tail: {:?}", result.stderr_tail);
    eprintln!("========================");
    assert_eq!(result.status, "completed");
    // bunx 启动时会输出 "Resolving dependencies" 等正常信息到 stderr，
    // npx 不会。因此 stderr 非空也可能是正常情况。
    if let Some(ref stderr) = result.stderr_tail {
        // 仅当 stderr 不全是 bun 解析信息时才算异常
        let is_bunx_noise = stderr.lines().all(|l| {
            l.is_empty()
                || l.contains("Resolving dependencies")
                || l.contains("Resolved, downloaded and extracted")
                || l.contains("Saved lockfile")
        });
        assert!(is_bunx_noise, "stderr 含非预期的错误输出:\n{}", stderr);
    }
}

#[tokio::test]
#[ignore = "requires @peri-code/workflow installed"]
async fn test_kill_marks_run_killed_in_progress_store() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();

    let executor = Arc::new(MockAgentExecutor {
        delay: std::time::Duration::from_secs(5),
    }) as Arc<dyn AgentExecutor>;
    let runner = WorkflowRunner::new(executor, cwd, None);
    let journal = Arc::new(WorkflowJournalStore::new(cwd));
    let progress = Arc::new(WorkflowProgressStore::new());
    let (done_tx, mut done_rx) = tokio::sync::watch::channel(None);
    // 持有 kill_tx：v1 打通后 kill 通道是 (kill_tx → kill_rx)，测试需保留 sender 以便触发
    let (kill_tx, kill_rx) = tokio::sync::oneshot::channel();

    let script = r#"
export const meta = { name: 'kill-test', description: 'kill test' }
const result = await agent('say hello')
return { output: result }
"#;

    let input = WorkflowInput {
        script: script.to_string(),
        args: None,
        max_concurrency: 3,
        budget_total: None,
        workflow_name: "kill-test".to_string(),
        resume_from: None,
    };

    let run_id = uuid::Uuid::now_v7().to_string();
    let progress_for_runner = Arc::clone(&progress);
    let run_id_for_runner = run_id.clone();
    let run_handle = tokio::spawn(async move {
        runner
            .run(
                run_id_for_runner,
                input,
                progress_for_runner,
                journal,
                done_tx,
                kill_rx,
            )
            .await
    });

    // 等待 run_started 写入 progress_store（run 进入 Running 状态）
    let progress_wait = Arc::clone(&progress);
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Some(run) = progress_wait.get_run(&run_id) {
                if matches!(run.status, RunStatus::Running) {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("run 未在超时内进入 Running 状态");

    // 触发 kill（等效 workflow/kill_run RPC → WorkflowTaskRegistry::kill → kill_tx）
    kill_tx.send(()).unwrap();

    // 等待完成信号：kill 分支是 done_tx 的唯一出口，必达
    tokio::time::timeout(std::time::Duration::from_secs(30), done_rx.changed())
        .await
        .expect("kill 后未收到完成信号")
        .unwrap();
    let result = done_rx.borrow().clone().unwrap();
    assert_eq!(result.status, "killed");

    // 核心断言：kill 后 progress_store 显示 Killed（workflow/list_runs 与 get_run 同源，
    // 回归点：修复前此处永久 Running —— 幽灵 running 根因）
    let run = progress
        .get_run(&run_id)
        .expect("run 应存在于 progress_store");
    assert!(
        matches!(run.status, RunStatus::Killed),
        "kill 后 progress_store 应显示 Killed，实际 {:?}",
        run.status
    );
    assert!(
        run.completed_at.is_some(),
        "Killed 条目必须设置 completed_at，否则 cleanup_completed 永不清理"
    );

    // run() 应在 kill 分支后正常返回 Ok
    run_handle.await.unwrap().unwrap();
}

/// [回归测试] Node 自然崩溃（非 kill）时 msg_loop failed 收尾必须收敛 progress_store
/// 为 Failed（issue 2026-08-05 遗留 2：修复前 run 永久 Running，幽灵 running 与
/// kill 分支同源）。
///
/// 防假阳性：脚本在 agent 执行**之后**顶层 throw → workflow/start 已成功、
/// RunStarted 已写入（先轮询 Running），随后 Node 进程崩溃退出——崩溃时进程已死，
/// 没有机会发 run_done progress 事件，只有 msg_loop 收尾（stdout 关闭 → recv None
/// → final_result 保持 "failed"）能标记终态。修复前该路径不写 progress_store，
/// 断言必然失败。
#[tokio::test]
#[ignore = "requires @peri-code/workflow installed"]
async fn test_natural_crash_marks_run_failed_in_progress_store() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();

    let executor = Arc::new(MockAgentExecutor {
        delay: std::time::Duration::ZERO,
    }) as Arc<dyn AgentExecutor>;
    let runner = WorkflowRunner::new(executor, cwd, None);
    let journal = Arc::new(WorkflowJournalStore::new(cwd));
    let progress = Arc::new(WorkflowProgressStore::new());
    let (done_tx, _done_rx) = tokio::sync::watch::channel(None);
    let (_kill_tx, kill_rx) = tokio::sync::oneshot::channel();

    let script = r#"
export const meta = { name: 'crash-test', description: 'crash test' }
const result = await agent('say hello')
throw new Error('intentional crash after agent')
"#;

    let input = WorkflowInput {
        script: script.to_string(),
        args: None,
        max_concurrency: 3,
        budget_total: None,
        workflow_name: "crash-test".to_string(),
        resume_from: None,
    };

    let run_id = uuid::Uuid::now_v7().to_string();
    let progress_for_runner = Arc::clone(&progress);
    let run_id_for_runner = run_id.clone();
    let run_handle = tokio::spawn(async move {
        runner
            .run(
                run_id_for_runner,
                input,
                progress_for_runner,
                journal,
                done_tx,
                kill_rx,
            )
            .await
    });

    // 等待 run_started 写入 progress_store（run 出现）——证明 workflow/start 已成功
    // 且 msg_loop 已 spawn（修复前此处之后永久 Running）。
    //
    // 注意：不能等待 Running 状态——mock executor 秒回 + 脚本顶层立即 throw，
    // Running 窗口仅毫秒级，50ms 轮询必然错过；run 可能直接以 Failed 终态出现
    // （崩溃收敛），因此只等待"run 存在"，终态由下方断言验证。
    let progress_wait = Arc::clone(&progress);
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if progress_wait.get_run(&run_id).is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("run 未在超时内出现");

    // 不触发 kill：Node 自然崩溃 → run() 应正常返回
    run_handle.await.unwrap().unwrap();

    // 核心断言：自然崩溃后 progress_store 显示 Failed（修复前此处永久 Running）
    let run = progress
        .get_run(&run_id)
        .expect("run 应存在于 progress_store");
    assert!(
        matches!(run.status, RunStatus::Failed),
        "自然崩溃后 progress_store 应显示 Failed，实际 {:?}",
        run.status
    );
    assert!(
        run.completed_at.is_some(),
        "Failed 条目必须设置 completed_at，否则 cleanup_completed 永不清理"
    );
}
