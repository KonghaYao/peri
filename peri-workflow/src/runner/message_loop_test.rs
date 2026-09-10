use super::*;
use crate::journal::RunState;
use crate::progress::{RunProgress, RunStatus};
use serde_json::json;
use std::sync::atomic::Ordering;

struct CountingExecutor(Arc<AtomicU64>);

#[async_trait::async_trait]
impl AgentExecutor for CountingExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        self.0.fetch_add(1, Ordering::SeqCst);
        AgentRunResult::Dead {
            reason: Some("unexpected-dispatch".into()),
            detail: None,
        }
    }
}

// 与 RPC 单测相同的本地管道 fixture；不查询 HOME、artifact cache 或网络。
async fn run_messages(
    agent_run_id: &str,
    limits: WorkflowLimits,
) -> (WorkflowResult, RunState, RunProgress, u64) {
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
    let calls = Arc::new(AtomicU64::new(0));
    let (msg_tx, msg_rx) = tokio::sync::mpsc::channel(2);
    msg_tx
        .send(IncomingMessage::Request {
            id: None,
            method: "agent/run".into(),
            params: Some(json!({"runId": agent_run_id, "agentId": 7, "prompt": "inspect"})),
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
        agent_executor: Arc::new(CountingExecutor(Arc::clone(&calls))),
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
        calls.load(Ordering::SeqCst),
    )
}

#[tokio::test]
async fn test_invalid_agent_run_continues_to_workflow_done() {
    let (result, state, progress, calls) =
        run_messages("other-run", WorkflowLimits::default()).await;
    assert_eq!(calls, 0, "跨 run 请求不得执行 agent");
    assert_eq!(result.status, "completed", "拒绝参数不应提前结束消息循环");
    assert_eq!(state.status, "completed");
    assert!(matches!(progress.status, RunStatus::Completed));
}

#[tokio::test]
async fn test_agent_limit_stops_before_queued_workflow_done() {
    let limits = WorkflowLimits {
        max_agents: Some(0),
        ..WorkflowLimits::default()
    };
    let (result, state, progress, calls) = run_messages("run-1", limits).await;
    assert_eq!(calls, 0, "超额请求不得创建 agent task");
    assert_eq!(result.status, "failed", "限额失败不得被后续 done 覆写");
    assert_eq!(
        result.error.as_deref(),
        Some("workflow exceeded maxAgents (0)")
    );
    assert_eq!(state.status, "failed");
    assert_eq!(state.error, result.error);
    assert!(matches!(progress.status, RunStatus::Failed));
}
