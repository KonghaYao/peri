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
    // workflow journal 位于 cwd/.claude；fixture 必须忽略它，否则终态对账会把
    // 本 run 自己的运行目录当成外部变化。
    std::fs::write(root.join(".gitignore"), ".claude/\n")
        .expect("the test fixture file must be writable");
    run(&["add", "tracked.txt", ".gitignore"]);
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

async fn run_messages(
    sink: Arc<dyn AgentTaskSink>,
    method: &str,
    params: serde_json::Value,
    limits: WorkflowLimits,
    breach: Option<&str>,
    git_repository: bool,
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
    let limit_state = Arc::new(RunLimitState::new(limits.max_tool_calls));
    if let Some(breach) = breach {
        limit_state.set_breach(breach.to_string());
    }
    let message_loop = MessageLoop {
        run_scope: Arc::new(crate::runner::scope::RunScope::new()),
        agent_task_sink: Some(sink),
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
            budget_total: None,
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
        outcome: AgentTaskPublish::LimitBreach("workflow exceeded maxAgents (0)".into()),
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
