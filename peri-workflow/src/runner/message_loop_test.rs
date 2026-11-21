use crate::journal::RunState;
use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::progress::{RunProgress, RunStatus};
use crate::protocol::WorkflowLimits;
use crate::rpc::IncomingMessage;
use crate::rpc::RpcChannel;
use crate::runner::limits::RunLimitState;
use crate::runner::message_loop::MessageLoop;
use crate::runner::AgentTaskPublish;
use crate::runner::AgentTaskSink;
use crate::runner::WorkflowInput;
use crate::runner::WorkflowResult;
use peri_acp_types::workflow::AgentRunParams;
use peri_acp_types::workflow::ProgressEvent;
use peri_js_runtime::JsExecutionHost;
use serde_json::json;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::watch;

/// 无执行事实的测试执行器：attempt 富化应显式留 None，不得写死默认值。
struct NoopExecutor;

#[async_trait::async_trait]
impl crate::runner::AgentExecutor for NoopExecutor {
    async fn execute(
        &self,
        _params: peri_acp_types::workflow::AgentRunParams,
    ) -> peri_acp_types::workflow::AgentRunResult {
        peri_acp_types::workflow::AgentRunResult::Skipped
    }
}

/// 记录发布调用的测试 sink：返回固定结果（不触达任务表）。
///
/// 真实准入（maxAgents / 关闭）在 `WorkflowRuntime` 的 sink 实现内；本测试验证
/// 消息循环对发布结果的处置（拒绝参数 / 准入拒绝 / 放行）。
struct RecordingSink {
    calls: Arc<AtomicU64>,
    outcome: AgentTaskPublish,
}

impl AgentTaskSink for RecordingSink {
    fn publish_agent_task(&self, _task_id: String, _params: AgentRunParams) -> AgentTaskPublish {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.outcome.clone()
    }
}

/// 初始化带一个提交的最小仓库，供终态 Git 对账使用。
fn init_git_repository(root: &std::path::Path) {
    let run = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("the status fixture result is required for this assertion")
            .success());
    };
    run(&["init", "-q"]);
    std::fs::write(root.join("tracked.txt"), "base\n")
        .expect("the test fixture file must be writable");
    // 故意不忽略 `.claude/`：journal 由宿主在本 run 内创建，终态对账必须排除
    // 自身产物，而不能依赖仓库的 ignore 规则。
    run(&["add", "tracked.txt"]);
    run(&[
        "-c",
        "user.name=Peri Test",
        "-c",
        "user.email=peri-test@example.invalid",
        "commit",
        "-qm",
        "baseline",
    ]);
}

#[allow(clippy::too_many_arguments)]
async fn run_messages(
    sink: Arc<dyn AgentTaskSink>,
    method: &str,
    params: serde_json::Value,
    limits: WorkflowLimits,
    breach: Option<&str>,
    git_repository: bool,
    budget_total: Option<u64>,
    seed: Option<fn(&RunLimitState)>,
) -> (WorkflowResult, RunState, RunProgress) {
    // 与 RPC 单测相同的本地管道 fixture；不查询 HOME、artifact cache 或网络。
    // 返回 (终态, state.json, progress)。
    let tmp = tempfile::tempdir().expect("the temporary test directory must be available");
    let observation = if git_repository {
        init_git_repository(tmp.path());
        crate::journal::GitBaseline::capture(tmp.path())
            .map(crate::journal::GitObservation::Captured)
            .expect("the fixture repository must be observable")
    } else {
        crate::journal::GitObservation::default()
    };
    let journal = Arc::new(WorkflowJournalStore::new(
        tmp.path()
            .to_str()
            .expect("the fixture text must be valid UTF-8"),
    ));
    journal
        .init_run("run-1", "return null")
        .expect("the init run fixture result is required for this assertion");
    let progress = Arc::new(WorkflowProgressStore::new());
    progress.apply_event(&ProgressEvent::RunStarted {
        run_id: "run-1".into(),
        workflow_name: "dispatch-test".into(),
        meta: None,
    });
    let host = Arc::new(
        JsExecutionHost::spawn(peri_js_runtime::JsProcessSpec::new(
            "perl",
            vec!["-e".into(), "sleep 60".into()],
        ))
        .expect("应创建本地 RPC 管道"),
    );
    let (msg_tx, msg_rx) = tokio::sync::mpsc::channel(2);
    msg_tx
        .send(IncomingMessage::Request {
            id: None,
            method: method.into(),
            params: Some(params),
        })
        .await
        .expect("msg tx send test receiver must remain available for this delivery");
    msg_tx
        .send(IncomingMessage::Request {
            id: None,
            method: "workflow/done".into(),
            params: Some(json!({"runId": "run-1", "status": "completed"})),
        })
        .await
        .expect("msg tx send test receiver must remain available for this delivery");
    drop(msg_tx);
    let (done_tx, mut done_rx) = watch::channel(None);
    let limit_state = Arc::new(RunLimitState::new(limits.max_tool_calls, budget_total));
    if let Some(breach) = breach {
        limit_state.set_breach(breach.to_string());
    }
    if let Some(seed) = seed {
        seed(&limit_state);
    }
    let message_loop = MessageLoop {
        run_scope: Arc::new(crate::runner::scope::RunScope::new()),
        agent_task_sink: Some(sink),
        agent_executor: Arc::new(NoopExecutor),
        limit_state,
        channel: Arc::new(RpcChannel::new(host.channel())),
        journal_store: Arc::clone(&journal),
        progress_store: Arc::clone(&progress),
        run_id: "run-1".into(),
        host: Arc::clone(&host),
        input: WorkflowInput {
            script: "return null".into(),
            args: None,
            max_concurrency: 1,
            budget_total,
            limits,
            workflow_name: "dispatch-test".into(),
            resume_from: None,
            write_intent: None,
            git_observation: observation,
        },
        started_at_iso: "2026-09-10T00:00:00Z".into(),
        run_started: std::time::Instant::now(),
        msg_rx,
        done_tx,
        attempts: crate::journal::AttemptTracker::default(),
    };
    message_loop.run().await;
    host.kill().await.expect("应回收本地管道进程");
    let result = crate::runner::receive_workflow_result(&mut done_rx)
        .await
        .expect("消息循环必须发布终态");
    (
        result,
        journal
            .read_state("run-1")
            .expect("the read state fixture result is required for this assertion"),
        progress
            .run_snapshot("run-1")
            .expect("the run snapshot fixture result is required for this assertion"),
    )
}

#[tokio::test]
async fn test_invalid_agent_task_continues_to_workflow_done() {
    let calls = Arc::new(AtomicU64::new(0));
    let sink = RecordingSink {
        calls: Arc::clone(&calls),
        outcome: AgentTaskPublish::Accepted,
    };
    let (result, state, progress) = run_messages(
        Arc::new(sink),
        "agent/task",
        // 跨 run 的发布：runId 与当前 run 不匹配，必须在准入前被拒绝。
        json!({"runId": "other-run", "taskId": "t-1", "agentId": 7, "prompt": "inspect"}),
        WorkflowLimits::default(),
        None,
        false,
        None,
        None,
    )
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 0, "跨 run 发布不得进入准入");
    assert_eq!(result.status, "completed", "拒绝参数不应提前结束消息循环");
    assert_eq!(state.status, "completed");
    assert!(matches!(progress.status, RunStatus::Completed));
}

#[tokio::test]
async fn test_agent_limit_stops_before_queued_workflow_done() {
    let calls = Arc::new(AtomicU64::new(0));
    let sink = RecordingSink {
        calls: Arc::clone(&calls),
        outcome: AgentTaskPublish::LimitBreach(crate::runner::RunLimitBreach::new(
            peri_acp_types::workflow::LimitScope::Agents,
            Some(0),
            Some(0),
            "workflow exceeded maxAgents (0)".into(),
        )),
    };
    let limits = WorkflowLimits {
        max_agents: Some(0),
        ..WorkflowLimits::default()
    };
    let (result, state, progress) = run_messages(
        Arc::new(sink),
        "agent/task",
        json!({"runId": "run-1", "taskId": "t-1", "agentId": 7, "prompt": "inspect"}),
        limits,
        None,
        false,
        None,
        None,
    )
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "准入被调用一次并拒绝");
    assert_eq!(result.status, "failed", "限额失败不得被后续 done 覆写");
    assert_eq!(
        result.error.as_deref(),
        Some("workflow exceeded maxAgents (0)")
    );
    assert_eq!(state.status, "failed");
    assert_eq!(state.error, result.error);
    assert!(matches!(progress.status, RunStatus::Failed));
}

/// maxElapsedMs 超时是后到的失败：终态必须保留先前记录的首个 breach 诊断。
#[tokio::test]
async fn test_elapsed_timeout_keeps_the_first_breach() {
    let calls = Arc::new(AtomicU64::new(0));
    let sink = RecordingSink {
        calls: Arc::clone(&calls),
        outcome: AgentTaskPublish::Accepted,
    };
    let limits = WorkflowLimits {
        max_elapsed_ms: Some(0),
        ..WorkflowLimits::default()
    };
    let (result, state, progress) = run_messages(
        Arc::new(sink),
        "agent/task",
        json!({"runId": "run-1", "taskId": "t-1", "agentId": 7, "prompt": "inspect"}),
        limits,
        Some("workflow exceeded maxToolCalls (5)"),
        false,
        None,
        None,
    )
    .await;
    assert_eq!(
        result.error.as_deref(),
        Some("workflow exceeded maxToolCalls (5)"),
        "超时不得覆写首个 breach"
    );
    assert_eq!(state.error, result.error);
    assert_eq!(result.status, "failed");
    assert!(matches!(progress.status, RunStatus::Failed));
}

/// issue #522：completed + 未声明 writeIntent、Git 观测完整且无差异。
///
/// 旧行为把这种情况投影成 `delivery=blocked`；新语义是“交付未请求”，
/// 旧枚举保持 `unknown`，原因与观测完整性写入 `state.json`。
#[tokio::test]
async fn completed_without_write_intent_declares_unrequested_delivery() {
    let calls = Arc::new(AtomicU64::new(0));
    let sink = RecordingSink {
        calls: Arc::clone(&calls),
        outcome: AgentTaskPublish::Accepted,
    };
    let (result, state, progress) = run_messages(
        Arc::new(sink),
        "agent/task",
        // 跨 run 发布在准入前被拒绝：本用例只关心终态投影。
        json!({"runId": "other-run", "taskId": "t-1", "agentId": 7, "prompt": "inspect"}),
        WorkflowLimits::default(),
        None,
        true,
        None,
        None,
    )
    .await;

    assert_eq!(result.status, "completed");
    assert_eq!(result.error, None, "干净省略意图的 run 不应附带 Git 错误");
    assert_eq!(
        result.post_processing_status,
        peri_acp_types::workflow::PostProcessingStatus::Unknown
    );
    assert_eq!(
        result.delivery_status,
        peri_acp_types::workflow::DeliveryStatus::Unknown,
        "未请求交付不得投影为 blocked"
    );
    assert_eq!(
        state.delivery_reason,
        Some(peri_acp_types::workflow::DeliveryReason::NotRequested)
    );
    assert_eq!(
        state.postcondition_observation,
        Some(peri_acp_types::workflow::PostconditionObservation::Complete)
    );
    assert_eq!(
        progress.delivery_status,
        peri_acp_types::workflow::DeliveryStatus::Unknown
    );
}

/// issue #522：maxToolCalls 与 token 预算同时触发。
///
/// 顶层 `error` 仍是首条 breach（旧消费者兼容），`limit_breaches` 列出全部触发项，
/// 每项带 scope / 上限 / 观测值 / attempt 身份 / 检测时点。
#[tokio::test]
async fn tool_call_and_token_budget_breaches_are_both_visible() {
    fn seed(state: &RunLimitState) {
        state.observe_tool_calls("run-1", "t-1", 7, 6);
        state.observe_tokens("run-1", "t-2", 8, 75_323);
    }
    let sink = RecordingSink {
        calls: Arc::new(AtomicU64::new(0)),
        outcome: AgentTaskPublish::Accepted,
    };
    let limits = WorkflowLimits {
        max_tool_calls: Some(5),
        ..WorkflowLimits::default()
    };
    let (result, state, _progress) = run_messages(
        Arc::new(sink),
        "agent/task",
        json!({"runId": "other-run", "taskId": "t-1", "agentId": 7, "prompt": "inspect"}),
        limits,
        None,
        false,
        Some(60_000),
        Some(seed),
    )
    .await;

    assert_eq!(result.status, "failed");
    assert_eq!(
        result.error.as_deref(),
        Some("workflow exceeded maxToolCalls (5)"),
        "顶层 error 仍是首条 breach，保持旧消费者兼容"
    );
    assert_eq!(state.error, result.error);
    assert_eq!(
        state.limits.max_tool_calls,
        Some(5),
        "state.limits 保留配置事实"
    );
    let scopes: Vec<_> = state
        .limit_breaches
        .iter()
        .map(|breach| breach.scope)
        .collect();
    assert_eq!(
        scopes,
        vec![
            peri_acp_types::workflow::LimitScope::ToolCalls,
            peri_acp_types::workflow::LimitScope::Tokens,
        ],
        "两项触发都必须可见，不能只留首条"
    );
    let tool_breach = &state.limit_breaches[0];
    assert_eq!(tool_breach.limit, Some(5));
    assert_eq!(tool_breach.observed, Some(6));
    assert_eq!(
        tool_breach
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.task_id.as_deref()),
        Some("t-1")
    );
    assert!(!tool_breach.detected_at.is_empty());
    let token_breach = &state.limit_breaches[1];
    assert_eq!(token_breach.limit, Some(60_000));
    assert_eq!(token_breach.observed, Some(75_323));
    assert_eq!(state.token_usage, Some(75_323));
}

/// 重投与缓存命中不重复计费：同一 taskId 只计首次，真实重试（新 taskId）独立计量。
#[test]
fn redelivery_is_not_charged_twice_but_a_real_retry_is() {
    let state = RunLimitState::new(None, Some(10));
    assert!(state.observe_tokens("run-1", "task-1", 1, 6).is_none());
    assert!(
        state.observe_tokens("run-1", "task-1", 1, 6).is_none(),
        "投递重试不得重复累计"
    );
    assert_eq!(state.observed_tokens(), Some(6));
    assert_eq!(
        state.observe_tokens("run-1", "task-2", 2, 6).as_deref(),
        Some("workflow exceeded budgetTotal (10)"),
        "真实重试是新 taskId，独立计量后才能超限"
    );
    assert_eq!(state.observed_tokens(), Some(12));
}

/// 未配置预算时也累计用量；没有任何上报时保持 None（不显示 0）。
#[test]
fn token_usage_stays_unset_when_nothing_was_reported() {
    let state = RunLimitState::new(None, None);
    assert_eq!(state.observed_tokens(), None);
    assert!(state.observe_tokens("run-1", "task-1", 1, 7).is_none());
    assert_eq!(state.observed_tokens(), Some(7));
    assert!(state.breaches().is_empty(), "未配置预算不得产生超限");
}

/// produced 的 dead 尝试：disposition 保持来源语义，执行结果单独可见。
#[test]
fn produced_dead_attempt_keeps_disposition_and_reports_dead_outcome() {
    use peri_acp_types::workflow::AgentRunResult;
    use peri_acp_types::workflow::AttemptDisposition;
    use peri_acp_types::workflow::AttemptOutcome;
    use peri_acp_types::workflow::WorkflowAttempt;

    let mut entry = crate::protocol::JournalEntry {
        key: "key-0".into(),
        seq: 0,
        result: AgentRunResult::Dead {
            scope_close_incomplete: false,
            reason: Some("runagent-threw".into()),
            detail: Some("boom".into()),
            duration_ms: Some(1),
            token_count: Some(9),
        },
        attempt: Some(WorkflowAttempt {
            run_id: "run-1".into(),
            agent_id: Some(0),
            journal_seq: 0,
            attempt_index: Some(1),
            recovered_from: None,
            consumed: true,
            disposition: AttemptDisposition::Produced,
            execution: None,
        }),
    };
    super::enrich_attempt_execution(
        &mut entry,
        Some(peri_acp_types::workflow::AgentExecutionLimits {
            iteration_limit: 400,
            iteration_limit_source: peri_acp_types::workflow::IterationLimitSource::AgentDefinition,
            iterations_used: Some(3),
        }),
        Some(60_000),
    );
    let attempt = entry.attempt.expect("attempt must remain present");
    assert_eq!(
        attempt.disposition,
        AttemptDisposition::Produced,
        "produced 只表示结果来源"
    );
    let execution = attempt.execution.expect("execution must be recorded");
    assert_eq!(execution.outcome, AttemptOutcome::Dead);
    assert_eq!(execution.reason.as_deref(), Some("runagent-threw"));
    assert_eq!(execution.iteration_limit, Some(400));
    assert_eq!(execution.iterations_used, Some(3));
    assert_eq!(execution.token_budget, Some(60_000));
    assert_eq!(execution.token_count, Some(9));
}

/// 迭代耗尽的 dead 尝试投影为 limited，并保留结构化 reason。
#[test]
fn exhausted_iteration_attempt_projects_limited_outcome() {
    use peri_acp_types::workflow::AgentRunResult;
    use peri_acp_types::workflow::AttemptDisposition;
    use peri_acp_types::workflow::AttemptOutcome;
    use peri_acp_types::workflow::WorkflowAttempt;

    let mut entry = crate::protocol::JournalEntry {
        key: "key-0".into(),
        seq: 0,
        result: AgentRunResult::Dead {
            scope_close_incomplete: false,
            reason: Some(peri_acp_types::workflow::AGENT_DEAD_REASON_MAX_ITERATIONS.into()),
            detail: Some("Max iterations exceeded (400)".into()),
            duration_ms: None,
            token_count: None,
        },
        attempt: Some(WorkflowAttempt {
            run_id: "run-1".into(),
            agent_id: Some(0),
            journal_seq: 0,
            attempt_index: None,
            recovered_from: None,
            consumed: true,
            disposition: AttemptDisposition::Produced,
            execution: None,
        }),
    };
    super::enrich_attempt_execution(&mut entry, None, None);
    let execution = entry
        .attempt
        .and_then(|attempt| attempt.execution)
        .expect("execution must be recorded");
    assert_eq!(execution.outcome, AttemptOutcome::Limited);
    assert_eq!(
        execution.reason.as_deref(),
        Some(peri_acp_types::workflow::AGENT_DEAD_REASON_MAX_ITERATIONS)
    );
    assert_eq!(
        execution.iteration_limit, None,
        "没有执行事实时显式 None，不写默认值"
    );
    assert_eq!(execution.iterations_used, None);
}

/// 缓存命中（recovered）不产生本次执行事实，也不参与计费。
#[test]
fn recovered_attempt_does_not_carry_execution_facts() {
    use peri_acp_types::workflow::AgentRunResult;
    use peri_acp_types::workflow::AttemptDisposition;
    use peri_acp_types::workflow::WorkflowAttempt;

    let mut entry = crate::protocol::JournalEntry {
        key: "key-0".into(),
        seq: 0,
        result: AgentRunResult::Skipped,
        attempt: Some(WorkflowAttempt {
            run_id: "run-1".into(),
            agent_id: Some(0),
            journal_seq: 0,
            attempt_index: Some(1),
            recovered_from: None,
            consumed: true,
            disposition: AttemptDisposition::Recovered,
            execution: None,
        }),
    };
    super::enrich_attempt_execution(
        &mut entry,
        Some(peri_acp_types::workflow::AgentExecutionLimits {
            iteration_limit: 200,
            iteration_limit_source: peri_acp_types::workflow::IterationLimitSource::Default,
            iterations_used: Some(2),
        }),
        Some(60_000),
    );
    assert!(
        entry
            .attempt
            .expect("attempt must remain")
            .execution
            .is_none(),
        "缓存命中没有本次执行，不得补执行事实"
    );
}
