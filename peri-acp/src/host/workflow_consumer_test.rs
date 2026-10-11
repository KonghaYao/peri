//! `WorkflowTaskConsumer` 的接线语义测试（P3）：就绪屏障、订阅失败回落、
//! 基线扫描 → 唯一 claim → 执行 → 回填（带消费者令牌）。
//!
//! P6 生命周期等价覆盖（本文件后段）：P6 起 agent 执行权归本层，`peri-workflow`
//! 单测不再具备消费者。因此「取消调用者不丢完成所有权（含真实 agent 派发）」与
//! 「简单 workflow 端到端 completed」两类断言在本层用真实 `WorkflowRuntime` +
//! 真实消费者 + 真实 Node 引擎覆盖，唯一替换的边界是 MCP 池面
//! （[`RuntimeWiredPool`]：读基线走生产资源面函数，回填走生产授权判定与同一终态
//! 提交入口，订阅唤醒来自 runtime 的资源更新流）。
//!
//! 轻量验证口径：只覆盖外部可观察行为（就绪位 / 池调用序列 / 任务终态 / run 终态），
//! 不覆盖重连退避节奏与并发上限细节（由用户验收）。

#![cfg(not(target_os = "emscripten"))]

use std::any::Any;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::ports::{McpPoolPort, McpPoolShutdownReport, McpResourceSubscription};
use peri_acp_types::workflow::{AgentExecutor, AgentRunParams, AgentRunResult, Usage};
use peri_mcp_workflow::{WorkflowRuntime, TASKS_URI, WORKFLOW_CONSUMER_META_KEY};
use peri_workflow::registry::WorkflowRunStatus;
use peri_workflow::runtime::{WorkflowResourceUpdate, WorkflowTaskState};
use serde_json::{json, Value};
use serial_test::serial;
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

// ─── P6 生命周期等价覆盖（真实引擎 + 真实消费者）─────────────────────────
//
// P6 起 agent 执行权归宿主消费者，`peri-workflow` 单测不再具备消费者，无法覆盖
// 「调用者在启动窗口被取消 → run 仍须执行真实 agent 并只结算一次」。等价覆盖落在
// 本层：真实 `WorkflowRuntime` + 真实 `WorkflowTaskConsumer` + 真实 Node 引擎，
// 唯一被替换的边界是 MCP 池面（读基线走生产资源面函数，回填走生产授权判定与同一
// 终态提交入口，订阅唤醒来自 runtime 的资源更新流）。

/// 池面接线：把三项池调用路由到生产实现/生产同一入口。
struct RuntimeWiredPool {
    runtime: Arc<WorkflowRuntime>,
}

#[async_trait]
impl McpPoolPort for RuntimeWiredPool {
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
        assert_eq!(instance, "workflow");
        assert_eq!(
            tool, "WorkflowAgentResult",
            "消费者只允许回填 host-only 工具"
        );
        let token = request_meta
            .as_ref()
            .and_then(|meta| meta.get(WORKFLOW_CONSUMER_META_KEY))
            .and_then(Value::as_str)
            .ok_or("回填必须在 _meta 携带消费者令牌")?;
        if !self.runtime.verify_consumer_token(token) {
            return Err("unbound consumer token".to_string());
        }
        let run_id = arguments["runId"]
            .as_str()
            .ok_or("missing runId")?
            .to_string();
        let task_id = arguments["taskId"]
            .as_str()
            .ok_or("missing taskId")?
            .to_string();
        let result: AgentRunResult = serde_json::from_value(arguments["result"].clone())
            .map_err(|error| format!("invalid result: {error}"))?;
        // 与实例 handler 相同的终态映射（Ok → Completed；Skipped/Dead → 终态）。
        let (state, reason) = match &result {
            AgentRunResult::Ok { .. } => (WorkflowTaskState::Completed, None),
            AgentRunResult::Skipped => (WorkflowTaskState::Completed, None),
            AgentRunResult::Dead { reason, .. } => (WorkflowTaskState::Failed, reason.clone()),
        };
        self.runtime
            .settle_agent_task(&run_id, &task_id, state, Some(result), reason);
        match self.runtime.tasks().get(&run_id, &task_id) {
            Some(record) if record.state == state => Ok("settled".to_string()),
            other => Err(format!("settle 未生效: {other:?}")),
        }
    }

    async fn read_instance_resource(&self, instance: &str, uri: &str) -> Result<String, String> {
        assert_eq!(instance, "workflow");
        peri_mcp_workflow::read_payload(&self.runtime, uri)
            .map(|payload| payload.to_string())
            .map_err(|error| format!("{error:?}"))
    }

    async fn subscribe_instance_resource(
        &self,
        instance: &str,
        uri: &str,
    ) -> Result<McpResourceSubscription, String> {
        assert_eq!(instance, "workflow");
        assert_eq!(uri, TASKS_URI);
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        // 生产由实例按资源更新投递订阅通知；测试从同一条更新流转发（唤醒语义同源）。
        let mut updates = self.runtime.subscribe();
        tokio::spawn(async move {
            while let Ok(update) = updates.recv().await {
                if matches!(update, WorkflowResourceUpdate::Tasks)
                    && tx.try_send(TASKS_URI.to_string()).is_err()
                {
                    break;
                }
            }
        });
        Ok(McpResourceSubscription::new(rx, CancellationToken::new()))
    }
}

/// 真实引擎派发处的门控执行者：证明 agent 是在调用者被取消之后才被派发的。
struct GatedEngineExecutor {
    calls: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl AgentExecutor for GatedEngineExecutor {
    async fn execute(&self, _params: AgentRunParams) -> AgentRunResult {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        AgentRunResult::Ok {
            output: json!("finished"),
            usage: Usage { output_tokens: 1 },
            model: None,
            tool_count: Some(0),
            token_count: Some(1),
            phase: None,
            duration_ms: Some(1),
        }
    }
}

/// 真实引擎集成用例的显式前置：由开发者提供已校验引擎包目录
/// （`PERI_WORKFLOW_ARTIFACT_DIR`，例如发布包或本机缓存）。仓库不读工作树 dist，
/// 未提供时打印说明并返回（显式、可见，不用 `#[ignore]`）。
fn require_explicit_engine() -> bool {
    if std::env::var_os("PERI_WORKFLOW_ARTIFACT_DIR").is_some_and(|value| !value.is_empty()) {
        return true;
    }
    eprintln!(
        "note: 跳过真实引擎用例（仓库不读工作树 dist）；\
         需要时设置 PERI_WORKFLOW_ARTIFACT_DIR=<@peri-code/workflow 包目录> 后重跑"
    );
    false
}

/// [P6 回归] 调用者在启动快速窗口被取消：run 仍由宿主消费者执行真实 agent，
/// 且只结算一次（`Completed`）。断言集合与 P6 前 `peri-workflow` 的同名生命周期
/// 用例一致：派发一次、通知恰好一次、活跃数归零、agent 计数为 1。
#[tokio::test]
#[serial]
async fn cancelled_caller_keeps_completion_owner_with_real_engine() {
    if !require_explicit_engine() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let executor: Arc<dyn AgentExecutor> = Arc::new(GatedEngineExecutor {
        calls: Arc::clone(&calls),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let (notification_tx, _rx) =
        tokio::sync::broadcast::channel::<peri_acp_types::workflow::WorkflowTaskResult>(8);
    let runtime = Arc::new(WorkflowRuntime::new(
        Arc::clone(&executor),
        cwd,
        notification_tx,
        None,
    ));
    let pool = Arc::new(RuntimeWiredPool {
        runtime: Arc::clone(&runtime),
    });
    let consumer = WorkflowTaskConsumer::start(
        Arc::clone(&pool) as Arc<dyn McpPoolPort>,
        Arc::clone(&runtime),
        Arc::clone(&executor),
        "session-lifecycle".to_string(),
    );
    assert!(
        wait_ready(&runtime, true).await,
        "消费者在订阅 + 基线成功后必须置就绪"
    );

    let mut notifications = runtime.registry().notification_tx().subscribe();
    let script = "export const meta = { name: 'fixture', description: 'lifecycle' }; \
                  return await agent('finish');";
    let mut invoke = Box::pin(runtime.start_run(json!({ "script": script })));
    std::future::poll_fn(|cx| {
        let result = invoke.as_mut().poll(cx);
        assert!(
            result.is_pending(),
            "必须在 run 已注册、调用者尚未返回的启动窗口内取消"
        );
        if runtime.registry().active_count() == 1 {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    drop(invoke);

    tokio::time::timeout(Duration::from_secs(30), entered.notified())
        .await
        .unwrap_or_else(|_| {
            panic!("取消调用者后，宿主消费者仍须派发真实 agent（run 不得被调用者取消带走）")
        });
    assert_eq!(calls.load(Ordering::SeqCst), 1, "agent 只执行一次");
    release.notify_one();

    let result = tokio::time::timeout(Duration::from_secs(10), notifications.recv())
        .await
        .expect("取消调用者不能丢失完成通知")
        .expect("完成通知通道必须存活");
    assert_eq!(result.status, WorkflowRunStatus::Completed);
    assert_eq!(result.agent_count, 1);
    assert_eq!(runtime.registry().active_count(), 0);
    assert!(
        notifications.try_recv().is_err(),
        "同一 run 只结算一次（不得双投递）"
    );

    consumer.stop().await;
}

/// [等价覆盖] 简单 workflow 端到端：真实引擎发布 `agent/task`，宿主消费者派发一次并
/// 回填，run 收敛为 `Completed`。
///
/// 承接原 `peri-workflow::runner_test::test_e2e_simple_workflow` 的「completed + agent
/// 正常往返」断言：P6 起 agent 结果只能由宿主消费者提供，该断言只能在本层成立。
#[tokio::test]
#[serial]
async fn simple_workflow_completes_through_host_consumer() {
    if !require_explicit_engine() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let executor: Arc<dyn AgentExecutor> = Arc::new(StubExecutor {
        calls: Arc::clone(&calls),
    });
    let (notification_tx, _rx) =
        tokio::sync::broadcast::channel::<peri_acp_types::workflow::WorkflowTaskResult>(8);
    let runtime = Arc::new(WorkflowRuntime::new(
        Arc::clone(&executor),
        cwd,
        notification_tx,
        None,
    ));
    let pool = Arc::new(RuntimeWiredPool {
        runtime: Arc::clone(&runtime),
    });
    let consumer = WorkflowTaskConsumer::start(
        Arc::clone(&pool) as Arc<dyn McpPoolPort>,
        Arc::clone(&runtime),
        Arc::clone(&executor),
        "session-simple".to_string(),
    );
    assert!(wait_ready(&runtime, true).await, "消费者必须先就绪");

    let mut notifications = runtime.registry().notification_tx().subscribe();
    let script = "export const meta = { name: 'e2e-simple', description: 'simple e2e' }; \
                  const r = await agent('say hello'); \
                  return { answer: r };";
    runtime
        .start_run(json!({ "script": script }))
        .await
        .expect("简单 workflow 必须启动并在快速路径内可收敛");

    let result = tokio::time::timeout(Duration::from_secs(20), notifications.recv())
        .await
        .expect("简单 workflow 必须在超时内收敛")
        .expect("完成通知通道必须存活");
    assert_eq!(result.status, WorkflowRunStatus::Completed);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "agent 恰好执行一次");
    assert_eq!(result.agent_count, 1);

    consumer.stop().await;
}
