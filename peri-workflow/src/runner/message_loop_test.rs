use super::*;
use crate::journal::RunState;
use crate::progress::{RunProgress, RunStatus};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// 与 RPC 单测相同的本地管道 fixture；不查询 HOME、artifact cache 或网络。
///
/// 返回 (终态, state.json, progress)。
async fn run_messages(
    sink: Arc<dyn AgentTaskSink>,
    method: &str,
    params: serde_json::Value,
    limits: WorkflowLimits,
) -> (WorkflowResult, RunState, RunProgress) {
    let tmp = tempfile::tempdir().unwrap();
    let journal = Arc::new(WorkflowJournalStore::new(tmp.path().to_str().unwrap()));
    journal.init_run("run-1", "return null").unwrap();
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
        .unwrap();
    msg_tx
        .send(IncomingMessage::Request {
            id: None,
            method: "workflow/done".into(),
            params: Some(json!({"runId": "run-1", "status": "completed"})),
        })
        .await
        .unwrap();
    drop(msg_tx);
    let (done_tx, mut done_rx) = watch::channel(None);
    let message_loop = MessageLoop {
        run_scope: Arc::new(super::super::scope::RunScope::new()),
        agent_task_sink: Some(sink),
        limit_state: Arc::new(RunLimitState::new(limits.max_tool_calls)),
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
            git_baseline: None,
        },
        started_at_iso: "2026-09-10T00:00:00Z".into(),
        run_started: std::time::Instant::now(),
        msg_rx,
        done_tx,
    };
    message_loop.run().await;
    host.kill().await.expect("应回收本地管道进程");
    let result = crate::runner::receive_workflow_result(&mut done_rx)
        .await
        .expect("消息循环必须发布终态");
    (
        result,
        journal.read_state("run-1").unwrap(),
        progress.get_run("run-1").unwrap(),
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
