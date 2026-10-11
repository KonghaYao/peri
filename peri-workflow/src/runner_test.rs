// ─── E2E 集成测试（真实 Node 引擎；产物经 PERI_WORKFLOW_ARTIFACT_DIR 注入）───
//
// P6 起 agent 执行权归宿主消费者：需要 agent 结果才能收敛的用例在宿主层
// （`peri-acp::host::workflow_consumer_tests`）覆盖；本文件只保留不依赖消费者的
// 引擎生命周期用例（kill / 自然崩溃）。

use serial_test::serial;

use super::limits::RunLimitState;
use super::run_protocol::{
    parse_agent_task_params, parse_run_scoped, reusable_journal_prefix, validate_start_ack,
    workflow_start_params, JournalTruncateParams,
};
use super::terminal::project_postcondition;
use super::{
    receive_workflow_result, AgentExecutor, WorkflowInput, WorkflowResult, WorkflowRunner,
};
use crate::journal::WorkflowJournalStore;
use crate::progress::{RunStatus, WorkflowProgressStore};
use crate::protocol::WorkflowDoneParams;
use crate::protocol::{AgentRunParams, AgentRunResult, Usage, WorkflowLimits};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[test]
fn resume_reuses_only_the_contiguous_success_prefix() {
    let entries: Vec<crate::protocol::JournalEntry> = serde_json::from_value(serde_json::json!([
        {
            "key": "first",
            "seq": 0,
            "result": {"kind": "ok", "output": "one", "usage": {"outputTokens": 1}}
        },
        {
            "key": "failed",
            "seq": 1,
            "result": {"kind": "dead", "reason": "runagent-threw"}
        },
        {
            "key": "later",
            "seq": 2,
            "result": {"kind": "ok", "output": "must-replay", "usage": {"outputTokens": 1}}
        }
    ]))
    .unwrap();

    let prefix = reusable_journal_prefix(entries);
    assert_eq!(prefix.len(), 1);
    assert_eq!(prefix[0].key, "first");
}

#[test]
fn resume_skipped_entry_is_a_cache_boundary() {
    let entries: Vec<crate::protocol::JournalEntry> = serde_json::from_value(serde_json::json!([
        {
            "key": "first",
            "seq": 0,
            "result": {"kind": "skipped"}
        },
        {
            "key": "later",
            "seq": 1,
            "result": {"kind": "ok", "output": "must-replay", "usage": {"outputTokens": 1}}
        }
    ]))
    .unwrap();

    assert!(reusable_journal_prefix(entries).is_empty());
}

#[test]
fn resume_gap_is_not_treated_as_a_success_prefix() {
    let entries: Vec<crate::protocol::JournalEntry> = serde_json::from_value(serde_json::json!([
        {
            "key": "later",
            "seq": 2,
            "result": {"kind": "ok", "output": "must-replay", "usage": {"outputTokens": 1}}
        }
    ]))
    .unwrap();

    assert!(reusable_journal_prefix(entries).is_empty());
}

struct CountingAgentExecutor {
    calls: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl AgentExecutor for CountingAgentExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        AgentRunResult::Dead {
            reason: Some("test executor must not run".into()),
            detail: None,
        }
    }
}

#[tokio::test]
async fn resume_sequence_gap_fails_before_any_agent_execution() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap();
    let journal = Arc::new(WorkflowJournalStore::new(cwd));
    journal.init_run("source", "script").unwrap();
    std::fs::write(
        journal.run_dir("source").join("journal.jsonl"),
        "{\"key\":\"later\",\"seq\":1,\"result\":{\"kind\":\"ok\",\"output\":\"later\",\"usage\":{\"outputTokens\":1}}}\n",
    )
    .unwrap();

    let calls = Arc::new(AtomicU64::new(0));
    let executor = Arc::new(CountingAgentExecutor {
        calls: Arc::clone(&calls),
    });
    let runner = WorkflowRunner::new(executor, cwd, None);
    let progress = Arc::new(WorkflowProgressStore::new());
    let (done_tx, _done_rx) = tokio::sync::watch::channel(None);
    let (_kill_tx, kill_rx) = tokio::sync::oneshot::channel();
    let input = WorkflowInput {
        script: "export const meta = { name: 'gap', description: 'gap' }".into(),
        args: None,
        max_concurrency: 3,
        budget_total: None,
        limits: WorkflowLimits::default(),
        workflow_name: "gap".into(),
        resume_from: Some("source".into()),
        write_intent: None,
        git_baseline: None,
    };

    let error = runner
        .run("target".into(), input, progress, journal, done_tx, kill_rx)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("expected seq 0"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

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
fn run_limit_state_tracks_tool_calls_and_keeps_first_breach() {
    let state = RunLimitState::new(Some(5));
    assert!(state.observe_tool_calls(3).is_none());
    assert_eq!(state.breach(), None, "未超限不得置 breach");
    assert_eq!(
        state.observe_tool_calls(3).as_deref(),
        Some("workflow exceeded maxToolCalls (5)"),
        "累计超限时返回首次 breach 详情",
    );
    assert_eq!(
        state.breach().as_deref(),
        Some("workflow exceeded maxToolCalls (5)"),
    );
    // breach 只写首次：后到的超限文本不得覆写（沿用旧 limit_breach 语义）。
    state.set_breach("workflow exceeded maxAgents (0)".into());
    assert_eq!(
        state.breach().as_deref(),
        Some("workflow exceeded maxToolCalls (5)"),
    );
}

/// 真实引擎集成用例的显式前置：由开发者提供已校验引擎包目录
/// （`PERI_WORKFLOW_ARTIFACT_DIR`）。仓库不读工作树 dist、也不假设本机装有引擎：
/// 未提供时打印说明并返回（显式、可见，不用 `#[ignore]`）。
fn require_explicit_engine() -> bool {
    if std::env::var_os("PERI_WORKFLOW_ARTIFACT_DIR").is_some_and(|value| !value.is_empty()) {
        return true;
    }
    eprintln!(
        "note: 跳过真实引擎用例（仓库不读工作树 dist）；         需要时设置 PERI_WORKFLOW_ARTIFACT_DIR=<@peri-code/workflow 包目录> 后重跑"
    );
    false
}

fn completed_workflow_result() -> WorkflowResult {
    WorkflowResult {
        run_id: "run-fast".into(),
        status: "completed".into(),
        return_value: None,
        error: None,
        post_processing_status: peri_acp_types::workflow::PostProcessingStatus::NotRequired,
        delivery_status: peri_acp_types::workflow::DeliveryStatus::Blocked,
        stderr_tail: None,
    }
}

#[tokio::test]
async fn workflow_result_receiver_reads_already_published_value() {
    let (done_tx, done_rx) = tokio::sync::watch::channel(None);
    done_tx.send(Some(completed_workflow_result())).unwrap();
    let mut late_rx = done_rx.clone();
    drop(done_tx);

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        receive_workflow_result(&mut late_rx),
    )
    .await
    .expect("已发布的 watch 终态必须立即可读")
    .expect("已发布的终态不得丢失");

    assert_eq!(result.status, "completed");
}

#[tokio::test]
async fn workflow_result_receiver_returns_none_when_closed_without_result() {
    let (done_tx, mut done_rx) = tokio::sync::watch::channel::<Option<WorkflowResult>>(None);
    drop(done_tx);

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        receive_workflow_result(&mut done_rx),
    )
    .await
    .expect("关闭且无终态的 watch 不得挂起");

    assert!(result.is_none());
}

#[test]
fn postcondition_projection_requires_acceptance_evidence_for_delivery() {
    use peri_acp_types::workflow::{
        AcceptanceStatus, DeliveryStatus, PostProcessingStatus, WorkflowWriteIntent,
    };

    let read_only = WorkflowWriteIntent::ReadOnly;
    assert_eq!(
        project_postcondition(
            "completed",
            AcceptanceStatus::Unknown,
            Some(&read_only),
            Some(&Ok(())),
        ),
        (PostProcessingStatus::NotRequired, DeliveryStatus::Unknown)
    );
    assert_eq!(
        project_postcondition(
            "completed",
            AcceptanceStatus::Passed,
            Some(&read_only),
            Some(&Ok(())),
        ),
        (
            PostProcessingStatus::NotRequired,
            DeliveryStatus::Deliverable,
        )
    );
    assert_eq!(
        project_postcondition(
            "completed",
            AcceptanceStatus::Failed,
            Some(&read_only),
            Some(&Ok(())),
        ),
        (PostProcessingStatus::NotRequired, DeliveryStatus::Blocked)
    );
}

#[test]
fn postcondition_projection_fails_safe_without_evidence() {
    use peri_acp_types::workflow::{
        AcceptanceStatus, DeliveryStatus, PostProcessingStatus, WorkflowWriteIntent,
    };

    let write = WorkflowWriteIntent::Write {
        repo_root: "/repo".into(),
        cwd: "/repo".into(),
        path_allowlist: vec!["src".into()],
        head_may_change: false,
        commit_required: None,
    };
    assert_eq!(
        project_postcondition("completed", AcceptanceStatus::Unknown, Some(&write), None),
        (PostProcessingStatus::Blocked, DeliveryStatus::Blocked)
    );
    assert_eq!(
        project_postcondition(
            "completed",
            AcceptanceStatus::Unknown,
            Some(&write),
            Some(&Err("credential token=secret".into())),
        ),
        (PostProcessingStatus::Failed, DeliveryStatus::Blocked)
    );
    assert_eq!(
        project_postcondition("completed", AcceptanceStatus::Unknown, None, Some(&Ok(())),),
        (PostProcessingStatus::Blocked, DeliveryStatus::Blocked)
    );
}

#[test]
fn test_workflow_start_params_preserve_budget_total() {
    let input = WorkflowInput {
        script: "export const meta = { name: 'budget', description: 'test' }".to_string(),
        args: Some(serde_json::json!({"key": "value"})),
        max_concurrency: 2,
        budget_total: Some(9_007_199_254_740_991),
        limits: WorkflowLimits::default(),
        workflow_name: "budget".to_string(),
        resume_from: None,
        write_intent: None,
        git_baseline: None,
    };

    let params = workflow_start_params("run-1", &input, None, "/tmp");

    assert_eq!(params.budget_total, Some(9_007_199_254_740_991));
    assert_eq!(params.max_concurrency, 2);
    assert_eq!(params.args, input.args);
}

#[test]
fn test_agent_task_params_preserve_requested_model() {
    let (task_id, params) = parse_agent_task_params(
        Some(serde_json::json!({
            "runId": "run-1",
            "taskId": "t-1",
            "agentId": 7,
            "prompt": "inspect",
            "model": "sonnet"
        })),
        "run-1",
    )
    .unwrap();

    assert_eq!(task_id, "t-1");
    assert_eq!(params.model.as_deref(), Some("sonnet"));
}

#[test]
fn test_agent_task_params_reject_invalid_model_type() {
    let result = parse_agent_task_params(
        Some(serde_json::json!({
            "runId": "run-1",
            "taskId": "t-1",
            "agentId": 7,
            "prompt": "inspect",
            "model": 42
        })),
        "run-1",
    );

    assert!(result.is_err());
}

#[test]
fn test_agent_task_params_reject_missing_params() {
    assert!(parse_agent_task_params(None, "run-1").is_err());
}

#[test]
fn test_agent_task_params_reject_empty_task_id() {
    let result = parse_agent_task_params(
        Some(serde_json::json!({
            "runId": "run-1",
            "taskId": "  ",
            "agentId": 7,
            "prompt": "inspect"
        })),
        "run-1",
    );

    assert_eq!(result.unwrap_err(), "taskId must be a non-empty string");
}

#[test]
fn test_agent_task_params_reject_cross_run_identity() {
    let result = parse_agent_task_params(
        Some(serde_json::json!({
            "runId": "other-run",
            "taskId": "t-1",
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
        "protocolVersion": super::artifact::WORKFLOW_PROTOCOL_VERSION,
        "buildId": super::artifact::WORKFLOW_BUILD_ID
    }))
    .unwrap();
}

#[test]
fn test_start_ack_rejects_protocol_mismatch() {
    let error = validate_start_ack(serde_json::json!({
        "ok": true,
        "protocolVersion": super::artifact::WORKFLOW_PROTOCOL_VERSION + 1,
        "buildId": super::artifact::WORKFLOW_BUILD_ID
    }))
    .unwrap_err();

    assert!(error.to_string().contains("protocol mismatch"));
}

/// [回归测试] kill 后 run 必须收敛为 Killed 且写 completed_at（不能停在幽灵 Running）。
///
/// 只依赖真实引擎启动（kill 分支不经过 agent 执行），因此可在本 crate 覆盖。
#[tokio::test]
#[serial]
async fn test_kill_marks_run_killed_in_progress_store() {
    if !require_explicit_engine() {
        return;
    }
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
        limits: WorkflowLimits::default(),
        workflow_name: "kill-test".to_string(),
        resume_from: None,
        write_intent: None,
        git_baseline: None,
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
/// 防假阳性：脚本先发布 `agent/task`（**不等待结果**，故本用例不需要宿主消费者：
/// P6 起 agent 执行权在宿主，等待结果会永久挂起），随后顶层 throw → Node 进程在
/// workflow/start 成功后自然退出；崩溃时进程已死，没有机会发 run_done progress
/// 事件，只有 msg_loop 收尾（stdout 关闭 → recv None → final_result 保持 "failed"）
/// 能标记终态。修复前该路径不写 progress_store，断言必然失败。
#[tokio::test]
#[serial]
async fn test_natural_crash_marks_run_failed_in_progress_store() {
    if !require_explicit_engine() {
        return;
    }
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
agent('say hello') // 发布 agent/task（不等待结果：本用例不依赖宿主消费者）
throw new Error('intentional crash after agent')
"#;

    let input = WorkflowInput {
        script: script.to_string(),
        args: None,
        max_concurrency: 3,
        budget_total: None,
        limits: WorkflowLimits::default(),
        workflow_name: "crash-test".to_string(),
        resume_from: None,
        write_intent: None,
        git_baseline: None,
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
