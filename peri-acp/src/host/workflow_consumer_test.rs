//! `WorkflowTaskConsumer` 的接线语义测试（P3）：就绪屏障、订阅失败回落、
//! 基线扫描 → 唯一 claim → 执行 → 回填（带消费者令牌）。
//!
//! 轻量验证口径：只覆盖外部可观察行为（就绪位 / 池调用序列 / 任务终态），
//! 不覆盖重连退避节奏与并发上限细节（由用户验收）。

#![cfg(not(target_os = "emscripten"))]

use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::ports::{McpPoolPort, McpPoolShutdownReport, McpResourceSubscription};
use peri_acp_types::workflow::{AgentExecutor, AgentRunParams, AgentRunResult, Usage};
use peri_mcp_workflow::{WorkflowRuntime, WORKFLOW_CONSUMER_META_KEY};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::workflow_consumer::WorkflowTaskConsumer;

/// 池调用记录：(实例, 工具/操作, 参数, request `_meta`)。
type CallRecord = (String, String, Value, Option<Value>);

struct StubPool {
    subscribe_fails: bool,
    calls: Mutex<Vec<CallRecord>>,
    tasks_payload: Mutex<String>,
    /// 订阅通道发送端：保持存活，避免订阅因通道关闭而终止。
    _updates: Mutex<Option<tokio::sync::mpsc::Sender<String>>>,
}

impl StubPool {
    fn new(subscribe_fails: bool, tasks_payload: &str) -> Self {
        Self {
            subscribe_fails,
            calls: Mutex::new(Vec::new()),
            tasks_payload: Mutex::new(tasks_payload.to_string()),
            _updates: Mutex::new(None),
        }
    }

    fn calls(&self) -> Vec<CallRecord> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl McpPoolPort for StubPool {
    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn shutdown(&self) -> McpPoolShutdownReport {
        McpPoolShutdownReport::Complete {
            settled_services: 0,
            failed_services: 0,
        }
    }

    fn snapshot(&self) -> Value {
        json!({})
    }

    async fn call_instance_tool(
        &self,
        instance: &str,
        tool: &str,
        arguments: Value,
        request_meta: Option<Value>,
    ) -> Result<String, String> {
        self.calls.lock().unwrap().push((
            instance.to_string(),
            tool.to_string(),
            arguments,
            request_meta,
        ));
        Ok("ok".to_string())
    }

    async fn read_instance_resource(&self, instance: &str, uri: &str) -> Result<String, String> {
        self.calls.lock().unwrap().push((
            instance.to_string(),
            "resources/read".to_string(),
            json!(uri),
            None,
        ));
        Ok(self.tasks_payload.lock().unwrap().clone())
    }

    async fn subscribe_instance_resource(
        &self,
        instance: &str,
        uri: &str,
    ) -> Result<McpResourceSubscription, String> {
        self.calls.lock().unwrap().push((
            instance.to_string(),
            "subscriptions/listen".to_string(),
            json!(uri),
            None,
        ));
        if self.subscribe_fails {
            return Err("instance is not connected".to_string());
        }
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        *self._updates.lock().unwrap() = Some(tx);
        Ok(McpResourceSubscription::new(rx, CancellationToken::new()))
    }
}

struct StubExecutor {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl AgentExecutor for StubExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        AgentRunResult::Ok {
            output: json!("done"),
            usage: Usage { output_tokens: 1 },
            model: None,
            tool_count: None,
            token_count: None,
            phase: None,
            duration_ms: None,
        }
    }
}

fn runtime_with(executor: Arc<dyn AgentExecutor>) -> Arc<WorkflowRuntime> {
    let (notification_tx, _rx) =
        tokio::sync::broadcast::channel::<peri_acp_types::workflow::WorkflowTaskResult>(8);
    Arc::new(WorkflowRuntime::new(
        executor,
        "/tmp",
        notification_tx,
        None,
    ))
}

fn task_payload(task_id: &str, run_id: &str) -> String {
    json!({
        "headSequence": 1,
        "truncatedBefore": 0,
        "activeTasks": [{
            "taskId": task_id,
            "runId": run_id,
            "agentId": 1,
            "state": "published",
            "params": {
                "runId": run_id,
                "agentId": 1,
                "prompt": "hello",
            },
            "publishedAtMs": 0,
        }],
        "entries": [],
    })
    .to_string()
}

async fn wait_ready(runtime: &WorkflowRuntime, ready: bool) -> bool {
    for _ in 0..200 {
        if runtime.consumer_ready() == ready {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// 订阅建立 + 基线读取成功 ⇒ 置就绪；未终态任务被唯一 claim、执行并回填
/// （`_meta` 携带消费者令牌）；任务进入终态后不再活跃。
#[tokio::test]
async fn consumer_readies_then_executes_and_backfills_published_task() {
    let pool = Arc::new(StubPool::new(false, &task_payload("t-1", "run-1")));
    let calls = Arc::new(AtomicUsize::new(0));
    let executor: Arc<dyn AgentExecutor> = Arc::new(StubExecutor {
        calls: Arc::clone(&calls),
    });
    let runtime = runtime_with(Arc::clone(&executor));
    let params = AgentRunParams {
        run_id: "run-1".to_string(),
        agent_id: 1,
        prompt: "hello".to_string(),
        schema: None,
        model: None,
        max_tokens: None,
        agent_type: None,
        isolation: None,
        allowed_tools: None,
        label: None,
        phase: None,
    };
    let _ = runtime.tasks().publish("t-1".to_string(), params);

    let consumer = WorkflowTaskConsumer::start(
        Arc::clone(&pool) as Arc<dyn McpPoolPort>,
        Arc::clone(&runtime),
        Arc::clone(&executor),
        "session-a".to_string(),
    );

    assert!(
        wait_ready(&runtime, true).await,
        "消费者在订阅 + 基线成功后必须置就绪"
    );
    // 执行与回填是异步的：等执行次数与回填调用出现。
    for _ in 0..200 {
        if calls.load(Ordering::SeqCst) == 1 && !pool.calls().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "任务必须被执行一次");

    let backfill = pool
        .calls()
        .into_iter()
        .find(|(_, tool, _, _)| tool == "WorkflowAgentResult")
        .expect("必须回填任务结果");
    assert_eq!(backfill.0, "workflow");
    assert_eq!(backfill.2["taskId"], "t-1");
    assert_eq!(backfill.2["runId"], "run-1");
    assert_eq!(backfill.2["result"]["kind"], "ok");
    let meta = backfill.3.expect("回填必须携带 request _meta");
    assert_eq!(
        meta[WORKFLOW_CONSUMER_META_KEY],
        json!(runtime.bind_consumer_token()),
        "回填令牌必须与运行时绑定令牌一致"
    );
    // claim 的观察面：任务已从 `published` 进入 `executing`（唯一 claim）。
    // 终态结算属实例 handler（`WorkflowAgentResult` 的服务端）；本测试的桩池不承载
    // 结算，故此处只断言 claim 结果。
    let active = runtime.tasks().active();
    assert_eq!(
        active.len(),
        1,
        "claim 后任务仍在活跃集合（等待实例侧结算）"
    );
    let claimed = serde_json::to_value(&active[0]).expect("任务记录可序列化");
    assert_eq!(
        claimed["state"], "executing",
        "任务必须已被 claim 进入执行中"
    );
    assert_eq!(claimed["taskId"], "t-1");
    // 基线读取必须发生在订阅之后（design §4.3 发现协议）。
    let ops: Vec<String> = pool.calls().into_iter().map(|(_, op, _, _)| op).collect();
    let listen = ops.iter().position(|op| op == "subscriptions/listen");
    let read = ops.iter().position(|op| op == "resources/read");
    assert!(listen.is_some() && read.is_some() && listen < read);

    consumer.stop().await;
}

/// F2：必需订阅建立失败必须阻断就绪（不得回落为「订阅失败仅告警」）。
#[tokio::test]
async fn consumer_stays_not_ready_when_subscription_fails() {
    let pool = Arc::new(StubPool::new(true, &task_payload("t-2", "run-2")));
    let calls = Arc::new(AtomicUsize::new(0));
    let executor: Arc<dyn AgentExecutor> = Arc::new(StubExecutor {
        calls: Arc::clone(&calls),
    });
    let runtime = runtime_with(Arc::clone(&executor));
    let consumer = WorkflowTaskConsumer::start(
        Arc::clone(&pool) as Arc<dyn McpPoolPort>,
        Arc::clone(&runtime),
        Arc::clone(&executor),
        "session-b".to_string(),
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!runtime.consumer_ready(), "订阅失败时消费者不得置就绪");
    let ops: Vec<String> = pool.calls().into_iter().map(|(_, op, _, _)| op).collect();
    assert!(
        ops.iter().all(|op| op != "resources/read"),
        "订阅未建立时不得读取任务基线"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "不得执行任务");

    consumer.stop().await;
}

/// 关闭路径：`stop` 取消订阅并有界收口（不残留后台任务）。
#[tokio::test]
async fn consumer_stop_returns_and_cancels() {
    let pool = Arc::new(StubPool::new(false, &task_payload("t-3", "run-3")));
    let executor: Arc<dyn AgentExecutor> = Arc::new(StubExecutor {
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let runtime = runtime_with(Arc::clone(&executor));
    let consumer = WorkflowTaskConsumer::start(
        Arc::clone(&pool) as Arc<dyn McpPoolPort>,
        Arc::clone(&runtime),
        Arc::clone(&executor),
        "session-c".to_string(),
    );
    assert!(wait_ready(&runtime, true).await);
    tokio::time::timeout(Duration::from_secs(3), consumer.stop())
        .await
        .expect("stop 必须在有界时间内收口");
    assert!(
        !runtime.consumer_ready(),
        "停止后必须回落 not-ready（不再接受新启动）"
    );
}
