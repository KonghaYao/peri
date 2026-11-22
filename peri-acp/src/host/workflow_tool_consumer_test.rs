//! 脚本工具调用（issue #407）的真实引擎 E2E：`globalThis.tool` → `tool/task`
//! → 宿主消费者 → `WorkflowToolResult` → `tool/result` → 脚本，以及 resume 回放。
//!
//! 与父模块共用测试装配（`RuntimeWiredPool` / `StubExecutor` / `wait_ready`）；
//! 用例在 `PERI_WORKFLOW_ARTIFACT_DIR` 缺失时跳过（仓库不读工作树 dist）。

use async_trait::async_trait;
use serde_json::{json, Value};
use serial_test::serial;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use super::*;

// ─── 脚本工具调用（issue #407）：真实引擎 + 宿主消费者闭环 ─────────────────

/// 记录每次 `tool()` 调用，并返回配置的终态。
struct RecordingToolInvoker {
    calls: Mutex<Vec<(String, Value)>>,
    outcome: Mutex<WorkflowToolOutcome>,
    entered: Mutex<Option<Arc<tokio::sync::Notify>>>,
    /// 释放门：存在时调用等待该信号（kill 用例保持未释放）。
    gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
}

impl RecordingToolInvoker {
    fn new(outcome: WorkflowToolOutcome) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            outcome: Mutex::new(outcome),
            entered: Mutex::new(None),
            gate: Mutex::new(None),
        }
    }

    fn gated(
        outcome: WorkflowToolOutcome,
        entered: Arc<tokio::sync::Notify>,
        gate: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            outcome: Mutex::new(outcome),
            entered: Mutex::new(Some(entered)),
            gate: Mutex::new(Some(gate)),
        }
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl WorkflowToolInvoker for RecordingToolInvoker {
    async fn invoke_tool(&self, call: WorkflowToolCall) -> WorkflowToolOutcome {
        self.calls
            .lock()
            .unwrap()
            .push((call.tool_name.clone(), Value::Object(call.arguments)));
        let entered = self.entered.lock().unwrap().clone();
        if let Some(entered) = entered {
            entered.notify_one();
        }
        let gate = self.gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.notified().await;
        }
        self.outcome.lock().unwrap().clone()
    }
}

type RealEngineToolSetup = (
    Arc<WorkflowRuntime>,
    WorkflowTaskConsumer,
    tokio::sync::broadcast::Receiver<peri_acp_types::workflow::WorkflowTaskResult>,
);

async fn start_real_engine_with_tools(
    cwd: &str,
    tool_invoker: Arc<dyn WorkflowToolInvoker>,
    session: &str,
) -> RealEngineToolSetup {
    let executor: Arc<dyn AgentExecutor> = Arc::new(StubExecutor {
        calls: Arc::new(AtomicUsize::new(0)),
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
        runtime.bind_consumer_token(),
        executor,
        tool_invoker,
        session.to_string(),
    );
    assert!(wait_ready(&runtime, true).await, "消费者必须先就绪");
    let notifications = runtime.registry().notification_tx().subscribe();
    (runtime, consumer, notifications)
}

fn run_dir(root: &std::path::Path, run_id: &str) -> std::path::PathBuf {
    root.join(".claude/workflow-runs").join(run_id)
}

fn read_run_json(root: &std::path::Path, run_id: &str, name: &str) -> Value {
    let path = run_dir(root, run_id).join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must exist: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} must be valid JSON: {error}", path.display()))
}

/// 「脚本只调用 `tool()`（无 agent）」端到端：真实引擎发布 `tool/task`，宿主消费者
/// 经 invoker 执行并回填，脚本拿到 `structuredContent`，run 收敛为 `Completed`，
/// 同时 `tools.jsonl` 落一条 `toolSeq=0 / agentSeq=0` 的记录。
#[tokio::test]
#[serial]
async fn tool_only_workflow_completes_through_host_consumer() {
    if !require_explicit_engine() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let recording = Arc::new(RecordingToolInvoker::new(WorkflowToolOutcome::Ok {
        structured_content: Some(json!({"value": 42})),
        content: "tool-output".to_string(),
    }));
    let invoker: Arc<dyn WorkflowToolInvoker> = recording.clone();
    let (runtime, consumer, mut notifications) =
        start_real_engine_with_tools(cwd, invoker, "session-tool-e2e").await;

    let script = "export const meta = { name: 'e2e-tool', description: 'tool e2e' }; \
                  const r = await tool('Read', { file_path: '/tmp/a' }); \
                  return { answer: r };";
    let started = runtime
        .start_run(json!({ "script": script }))
        .await
        .expect("tool workflow must start");

    let result = tokio::time::timeout(Duration::from_secs(20), notifications.recv())
        .await
        .expect("tool workflow must converge in time")
        .expect("completion channel must stay alive");
    assert_eq!(result.status, WorkflowRunStatus::Completed, "{result:?}");
    assert_eq!(result.agent_count, 0);
    assert!(!result.unknown_side_effects, "{result:?}");

    let recorded = recording.calls();
    assert_eq!(recorded.len(), 1, "脚本只调用一次 tool()");
    assert_eq!(recorded[0].0, "Read");
    assert_eq!(recorded[0].1, json!({ "file_path": "/tmp/a" }));

    let state = read_run_json(dir.path(), &started.run_id, "state.json");
    assert_eq!(
        state["return_value"]["answer"],
        json!({"value": 42}),
        "structuredContent 必须优先于文本 content"
    );
    let tools = std::fs::read_to_string(run_dir(dir.path(), &started.run_id).join("tools.jsonl"))
        .expect("tools.jsonl must be written");
    let entry: Value = serde_json::from_str(tools.lines().next().unwrap()).unwrap();
    assert_eq!(entry["toolSeq"], 0);
    assert_eq!(entry["agentSeq"], 0);
    assert_eq!(entry["toolName"], "Read");
    assert_eq!(entry["outcome"]["kind"], "ok");

    consumer.stop().await;
}

/// 宿主工具失败 → 脚本 `try/catch` 拿到类型化错误码，run 仍可正常完成。
#[tokio::test]
#[serial]
async fn tool_error_is_catchable_in_script() {
    if !require_explicit_engine() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let invoker: Arc<dyn WorkflowToolInvoker> = Arc::new(RecordingToolInvoker::new(
        WorkflowToolOutcome::error(WorkflowToolErrorCode::ToolError, "Bash", "boom"),
    ));
    let (runtime, consumer, mut notifications) =
        start_real_engine_with_tools(cwd, invoker, "session-tool-error").await;

    let script = "export const meta = { name: 'e2e-tool-error', description: 'tool error' }; \
                  let code = 'none'; \
                  try { await tool('Bash', { command: 'exit 1' }) } catch (e) { code = e.code }; \
                  return { code };";
    let started = runtime
        .start_run(json!({ "script": script }))
        .await
        .expect("tool workflow must start");

    let result = tokio::time::timeout(Duration::from_secs(20), notifications.recv())
        .await
        .expect("tool workflow must converge in time")
        .expect("completion channel must stay alive");
    assert_eq!(result.status, WorkflowRunStatus::Completed, "{result:?}");

    let state = read_run_json(dir.path(), &started.run_id, "state.json");
    assert_eq!(state["return_value"]["code"], "tool_error");
    let tools = std::fs::read_to_string(run_dir(dir.path(), &started.run_id).join("tools.jsonl"))
        .expect("tools.jsonl must be written");
    let entry: Value = serde_json::from_str(tools.lines().next().unwrap()).unwrap();
    assert_eq!(entry["outcome"]["kind"], "error");
    assert_eq!(entry["outcome"]["code"], "tool_error");

    consumer.stop().await;
}

/// >1 MiB 的工具结果不得截断（wire + 脚本 + tools.jsonl 三处完整）。
#[tokio::test]
#[serial]
async fn large_tool_result_is_not_truncated() {
    if !require_explicit_engine() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let big = "x".repeat(1_500_000);
    let invoker: Arc<dyn WorkflowToolInvoker> =
        Arc::new(RecordingToolInvoker::new(WorkflowToolOutcome::Ok {
            structured_content: None,
            content: big.clone(),
        }));
    let (runtime, consumer, mut notifications) =
        start_real_engine_with_tools(cwd, invoker, "session-tool-large").await;

    let script = "export const meta = { name: 'e2e-tool-large', description: 'tool large' }; \
                  const r = await tool('Read', { file_path: '/tmp/big' }); \
                  return { len: r.length };";
    let started = runtime
        .start_run(json!({ "script": script }))
        .await
        .expect("tool workflow must start");

    let result = tokio::time::timeout(Duration::from_secs(30), notifications.recv())
        .await
        .expect("large tool workflow must converge in time")
        .expect("completion channel must stay alive");
    assert_eq!(result.status, WorkflowRunStatus::Completed, "{result:?}");

    let state = read_run_json(dir.path(), &started.run_id, "state.json");
    assert_eq!(
        state["return_value"]["len"],
        json!(1_500_000),
        "脚本必须收到完整结果（不得截断）"
    );
    let tools_path = run_dir(dir.path(), &started.run_id).join("tools.jsonl");
    let bytes = std::fs::metadata(&tools_path)
        .expect("tools.jsonl must exist")
        .len();
    assert!(
        bytes >= 1_500_000,
        "tools.jsonl 必须完整保存工具结果（实际 {bytes} 字节）"
    );

    consumer.stop().await;
}

/// kill 中断执行中的工具：脚本等待方不虚报完成，run 标记 `unknownSideEffects`。
#[tokio::test]
#[serial]
async fn kill_during_tool_marks_unknown_side_effects() {
    if !require_explicit_engine() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let invoker: Arc<dyn WorkflowToolInvoker> = Arc::new(RecordingToolInvoker::gated(
        WorkflowToolOutcome::Ok {
            structured_content: None,
            content: "late".to_string(),
        },
        Arc::clone(&entered),
        Arc::clone(&gate),
    ));
    let (runtime, consumer, mut notifications) =
        start_real_engine_with_tools(cwd, invoker, "session-tool-kill").await;

    let script = "export const meta = { name: 'e2e-tool-kill', description: 'tool kill' }; \
                  const r = await tool('Bash', { command: 'sleep 1' }); \
                  return { r };";
    let started = runtime
        .start_run(json!({ "script": script }))
        .await
        .expect("tool workflow must start");

    tokio::time::timeout(Duration::from_secs(20), entered.notified())
        .await
        .expect("宿主必须收到 tool/task 并开始执行");
    assert!(
        runtime.kill_run(&started.run_id),
        "进行中的 run 必须可被 kill"
    );

    let result = tokio::time::timeout(Duration::from_secs(20), notifications.recv())
        .await
        .expect("kill 后必须收敛")
        .expect("completion channel must stay alive");
    assert_eq!(result.status, WorkflowRunStatus::Killed, "{result:?}");
    assert!(
        result.unknown_side_effects,
        "执行中的工具没有终态时必须标记未知副作用: {result:?}"
    );
    let state = read_run_json(dir.path(), &started.run_id, "state.json");
    assert_eq!(state["unknownSideEffects"], true);

    // 释放 gated invoker，避免消费者在 stop 时等待超时。
    gate.notify_waiters();
    consumer.stop().await;
}

/// resume 回放：第二次 run 命中 `tools.jsonl` 前缀，不再调用宿主 invoker，
/// 且把回放条目经 `tool/replayed` 记入新 run 的 `tools.jsonl`。
#[tokio::test]
#[serial]
async fn resume_replays_recorded_tool_without_reinvoking_host() {
    if !require_explicit_engine() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let recording = Arc::new(RecordingToolInvoker::new(WorkflowToolOutcome::Ok {
        structured_content: Some(json!({"value": 7})),
        content: "replayed".to_string(),
    }));
    let invoker: Arc<dyn WorkflowToolInvoker> = recording.clone();
    let (runtime, consumer, mut notifications) =
        start_real_engine_with_tools(cwd, invoker, "session-tool-resume").await;

    let script = "export const meta = { name: 'e2e-tool-resume', description: 'tool resume' }; \
                  const r = await tool('Read', { file_path: '/tmp/r' }); \
                  return { r };";
    let first = runtime
        .start_run(json!({ "script": script }))
        .await
        .expect("first run must start");
    let first_result = tokio::time::timeout(Duration::from_secs(20), notifications.recv())
        .await
        .expect("first run must converge in time")
        .expect("completion channel must stay alive");
    assert_eq!(
        first_result.status,
        WorkflowRunStatus::Completed,
        "{first_result:?}"
    );
    assert_eq!(recording.calls().len(), 1, "首次 run 必须真实执行一次");

    let second = runtime
        .start_run(json!({
            "script": script,
            "resumeFromRunId": first.run_id,
        }))
        .await
        .expect("resume run must start");
    let second_result = tokio::time::timeout(Duration::from_secs(20), notifications.recv())
        .await
        .expect("resume run must converge in time")
        .expect("completion channel must stay alive");
    assert_eq!(
        second_result.status,
        WorkflowRunStatus::Completed,
        "{second_result:?}"
    );
    assert_eq!(
        recording.calls().len(),
        1,
        "resume 回放不得重新调用宿主 invoker"
    );

    let first_state = read_run_json(dir.path(), &first.run_id, "state.json");
    let second_state = read_run_json(dir.path(), &second.run_id, "state.json");
    assert_eq!(first_state["return_value"], second_state["return_value"]);

    let replay_tools: Vec<Value> =
        std::fs::read_to_string(run_dir(dir.path(), &second.run_id).join("tools.jsonl"))
            .expect("resume run must record the replayed entry")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert_eq!(replay_tools.len(), 1, "回放条目必须落新 run 的 tools.jsonl");
    assert_eq!(replay_tools[0]["toolSeq"], 0);
    assert_eq!(replay_tools[0]["toolName"], "Read");

    consumer.stop().await;
}
