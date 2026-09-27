//! B-07 宿主 seam 终审：System MCP 启动准入（主 plan 契约 2 / 3 / 4）。
//!
//! 断言层次（主 plan §5 R9 把跨层断言上移到本层；crate 内可观察层由 D-02 与
//! `peri-middlewares/src/mcp/middleware_test.rs` 承担）：
//!
//! - **真实装配**：`crate::host::stage_builder::build_stage_context` +
//!   `ProductionChainAssembler`（与生产 host/prompt.rs 同一条链）；
//! - **受控 transport**：真实子进程 + 真实 rmcp stdio 客户端。对端行为由 fixture
//!   脚本 mode 决定（不响应 / initialize 失败 / tools/list 失败 / 断链 / 正常列出），
//!   因此「未 ready」「失败」「timeout」「peer 断开」都由真实协议路径产生，不是手工
//!   写状态；等待只轮询公开可见事实（`get_client().status`），不用 sleep 猜时序；
//! - **counting model**：`Model::stream` 调用计数就是「React loop 是否进入 Reason」
//!   的事实——Reason 阶段必然调用模型，0 次即未进入。
//!
//! 配置经真实项目级 `.mcp.json` 与真实 `run_initialize` 装载，HOME 重定向到临时
//! 目录，测试不读取也不启动用户自己的 MCP 配置。

use std::{
    ffi::OsString,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard, OnceLock,
    },
};

use async_trait::async_trait;
use futures::stream;
use peri_acp_types::{
    event::{ExecutorEvent, TurnStatus},
    messages::{BaseMessage, MessageContent},
    ports::McpPoolPort,
    session::ExecutionFailureKind,
};
use peri_middlewares::mcp::{ClientStatus, McpClientPool, McpInitStatus, McpTaskOwner};
use peri_model::{
    Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult, ModelStream,
    ModelStreamEvent, StopReason,
};
use serial_test::serial;
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::executor_flow_tests::{
    make_session_context, make_stage_build, make_turn_input, MockEventSink,
};
use crate::session::executor::{run_session_loop, PromptStopReason, SessionContext};

/// 受控 stdio MCP 对端。只实现启动准入涉及的方法：
/// `initialize` / `tools/list` / `resources/list` / `ping`；其余请求（含
/// `server/discover`）回 `-32601`，驱动 rmcp Auto 生命周期回退 legacy initialize。
///
/// mode：`hang`（永不响应）/ `peer_exit`（收到请求即退出，模拟 peer 断开）/
/// `init_error`（initialize 报错）/ `list_error`（tools/list 报错）/
/// `hang_list`（initialize 成功但 `tools/list` 永不回应）/ `tools`（正常列出）。
const FIXTURE_SCRIPT: &str = r#"
const mode = process.argv[2] || 'hang';
const tools = (process.argv[3] || '').split(',').filter(Boolean);
const readline = require('node:readline').createInterface({ input: process.stdin });
const reply = payload => process.stdout.write(JSON.stringify(payload) + '\n');
readline.on('line', line => {
  let request;
  try { request = JSON.parse(line); } catch (error) { return; }
  if (request.id === undefined) return; // 通知无 id，不回响应
  if (mode === 'hang') return;
  if (mode === 'hang_list' && request.method === 'tools/list') return;
  if (mode === 'peer_exit') process.exit(3);
  if (request.method === 'initialize') {
    if (mode === 'init_error') {
      reply({ jsonrpc: '2.0', id: request.id, error: { code: -32000, message: 'fixture initialize failure' } });
      return;
    }
    reply({ jsonrpc: '2.0', id: request.id, result: {
      protocolVersion: '2025-11-25',
      capabilities: {},
      serverInfo: { name: 'mcp-v4-fixture', version: '1' },
    }});
    return;
  }
  if (request.method === 'tools/list') {
    if (mode === 'list_error') {
      reply({ jsonrpc: '2.0', id: request.id, error: { code: -32000, message: 'fixture tools/list failure' } });
      return;
    }
    reply({ jsonrpc: '2.0', id: request.id, result: {
      tools: tools.map(name => ({
        name,
        description: 'fixture tool ' + name,
        inputSchema: { type: 'object', properties: {} },
      })),
    }});
    return;
  }
  if (request.method === 'resources/list') {
    reply({ jsonrpc: '2.0', id: request.id, result: { resources: [] } });
    return;
  }
  if (request.method === 'ping') {
    reply({ jsonrpc: '2.0', id: request.id, result: {} });
    return;
  }
  reply({ jsonrpc: '2.0', id: request.id, error: { code: -32601, message: 'Method not found' } });
});
"#;

/// HOME 重定向守卫：`load_merged_config_full` 读取 `~/.peri/settings.json`，
/// 测试必须走临时 HOME，避免启动用户自己的 MCP server。
struct HomeRedirect {
    _lock: MutexGuard<'static, ()>,
    previous: Option<OsString>,
}

impl HomeRedirect {
    fn set(home: &Path) -> Self {
        static HOME_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        // 一个用例 panic 不得毒化 HOME 重定向，使后续用例连带失败（HOME 由 Drop
        // 恢复，中毒锁不影响环境事实）。
        let lock = HOME_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for HomeRedirect {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// counting model：`stream` 调用次数即 Reason 进入次数，同时保留请求快照
/// （首个请求的 tools 入参是契约 3 的终审对象）。
struct CountingModel {
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRequest>>,
}

impl CountingModel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// 首个 LLM 请求的 tools 入参工具名（`ModelRequest.tools` 即模型真实收到的列表）。
    fn first_request_tool_names(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .first()
            .map(|request| request.tools.iter().map(|tool| tool.name.clone()).collect())
            .unwrap_or_default()
    }

    /// 首个 LLM 请求的系统消息文本（deferred 摘要的断言面）。
    fn first_request_system_text(&self) -> String {
        self.requests
            .lock()
            .unwrap()
            .first()
            .and_then(|request| request.messages.first())
            .map(|message| message.text_content().unwrap_or_default())
            .unwrap_or_default()
    }
}

#[async_trait]
impl Model for CountingModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: AgentCancellationToken,
    ) -> ModelResult<ModelStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request);
        let response = ModelResponse::new(
            ModelMessage::assistant_text("done"),
            StopReason::EndTurn,
            None,
            None,
        )?;
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

/// System MCP fixture 宿主：真实 `.mcp.json` + 真实 `run_initialize` + 真实 pool。
struct McpStartupHarness {
    _home: HomeRedirect,
    _tmp: tempfile::TempDir,
    pool: Arc<McpClientPool>,
    _owner: McpTaskOwner,
    init_task: Option<tokio::task::JoinHandle<()>>,
}

impl McpStartupHarness {
    /// 写入受控对端脚本与项目级配置；`servers` 是 `mcpServers` 的原始 JSON。
    fn new(servers: serde_json::Value) -> Self {
        let tmp = tempfile::TempDir::new().expect("临时目录");
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("workspace");
        let claude_home = tmp.path().join("claude");
        for dir in [&home, &workspace, &claude_home] {
            std::fs::create_dir_all(dir).expect("创建临时目录");
        }
        std::fs::write(workspace.join("mcp_fixture.js"), FIXTURE_SCRIPT)
            .expect("写入 fixture 脚本");
        std::fs::write(
            workspace.join(".mcp.json"),
            serde_json::json!({ "mcpServers": servers }).to_string(),
        )
        .expect("写入项目级 MCP 配置");

        let home_guard = HomeRedirect::set(&home);
        let (owner, spawner) = McpTaskOwner::new();
        let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
        let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        let init_pool = Arc::clone(&pool);
        let init_task = tokio::spawn(async move {
            McpClientPool::run_initialize(
                init_pool,
                &workspace,
                &claude_home,
                status_tx,
                None,
                None,
            )
            .await;
        });

        Self {
            _home: home_guard,
            _tmp: tmp,
            pool,
            _owner: owner,
            init_task: Some(init_task),
        }
    }

    /// 等待真实初始化收口（所有 server 都已得出连接结论）。
    async fn initialized(servers: serde_json::Value) -> Self {
        let mut harness = Self::new(servers);
        if let Some(task) = harness.init_task.take() {
            tokio::time::timeout(std::time::Duration::from_secs(30), task)
                .await
                .expect("真实 MCP 初始化不得挂起")
                .expect("初始化任务不得 panic");
        }
        harness
    }

    /// 注入 fixture pool 的 session 装配面（真实 assembler 会据此构造 McpMiddleware）。
    fn session_context(&self, session_id: &str) -> SessionContext {
        let mut ctx = make_session_context(session_id);
        ctx.mcp_pool = Some(Arc::clone(&self.pool) as Arc<dyn McpPoolPort>);
        ctx
    }

    /// 有界轮询公开可见的句柄状态；到点仍不满足即失败（避免固定 sleep 猜时序）。
    async fn await_status(&self, server: &str, expect: &str, predicate: fn(&ClientStatus) -> bool) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Some(handle) = self.pool.get_client(server) {
                if predicate(&handle.status) {
                    return;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{server} 的公开状态在 20s 内未变成 {expect}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    async fn await_connected(&self, server: &str) {
        self.await_status(server, "Connected", |status| {
            matches!(status, ClientStatus::Connected)
        })
        .await;
    }

    async fn await_failed(&self, server: &str) {
        self.await_status(server, "Failed", |status| {
            matches!(status, ClientStatus::Failed(_))
        })
        .await;
    }
}

fn system_server(
    mode: &str,
    tools: &str,
    required: serde_json::Value,
    timeout_ms: u64,
) -> serde_json::Value {
    serde_json::json!({
        "command": "node",
        "args": ["mcp_fixture.js", mode, tools],
        "system_mcp": true,
        "system_mcp_tools": required,
        "system_mcp_timeout": timeout_ms,
    })
}

fn ordinary_server(mode: &str, tools: &str) -> serde_json::Value {
    serde_json::json!({
        "command": "node",
        "args": ["mcp_fixture.js", mode, tools],
    })
}

/// 跑一次真实 prompt：装配面注入 counting model，其余（链装配、闸门、终态投影）
/// 全部走生产路径；结果与观测面由调用方持有的 `sink` / `model` 提供。
async fn run_prompt(
    ctx: SessionContext,
    sink: &Arc<MockEventSink>,
    model: &Arc<CountingModel>,
) -> crate::session::executor::PromptResult {
    let model = Arc::clone(model);
    let mut ctx = ctx;
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model) as Arc<dyn Model>));
    let turn = make_turn_input(
        Arc::clone(sink) as Arc<dyn crate::session::event_sink::EventSink>,
        MessageContent::text("system mcp startup probe"),
        false,
        vec![],
        make_stage_build(&ctx),
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        run_session_loop(ctx, turn),
    )
    .await
    .expect("prompt 不得挂起")
}

/// fatal 终态的统一断言：模型 0 次调用、failure=Internal、事件序列
/// `AgentExecutionFailed → TurnEnded(Error) → done` 各一次，并核对真实失败经
/// 标准 ACP 投影后的 wire 形态（-32000 / `data.kind = internal`，无 status /
/// diagnostic 附加字段）。
fn assert_fatal_without_reason(
    result: &crate::session::executor::PromptResult,
    sink: &MockEventSink,
    model: &CountingModel,
    expected_fragment: &str,
) {
    assert!(!result.ok, "启动准入失败必须使本次 prompt 失败");
    assert_eq!(model.call_count(), 0, "闸门失败时模型调用次数必须为 0");
    assert_eq!(model.request_count(), 0, "闸门失败时不得产生任何 LLM 请求");
    let failure = result.failure.as_ref().expect("fatal 必须产生 failure");
    assert_eq!(failure.kind, ExecutionFailureKind::Internal);
    assert!(
        failure.public_message.contains("McpMiddleware"),
        "失败必须归属于 McpMiddleware: {}",
        failure.public_message
    );
    assert!(
        failure.public_message.contains(expected_fragment),
        "失败文案缺少准入事实「{expected_fragment}」: {}",
        failure.public_message
    );

    // 真实闸门失败 → 标准 ACP fatal 投影（同一 `ExecutionFailure` 走生产转换函数）。
    let wire = crate::host::prompt::execution_failure_to_acp_error(failure);
    assert_eq!(
        wire.code,
        crate::host::prompt::ACP_TURN_EXECUTION_FAILED_CODE
    );
    assert_eq!(
        wire.data,
        Some(serde_json::json!({ "kind": "internal" })),
        "MCP 准入失败只能投影为 internal 类别"
    );
    assert!(wire.message.contains("McpMiddleware"));
    assert!(wire.message.contains(expected_fragment));

    let operations = sink.operations();
    let failure_index = operations
        .iter()
        .position(|operation| operation.contains("agent_execution_failed"))
        .expect("必须发射 AgentExecutionFailed");
    let ended: Vec<usize> = operations
        .iter()
        .enumerate()
        .filter(|(_, operation)| operation.contains("\"turn_ended\""))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(ended.len(), 1, "terminal 事件必须唯一");
    let done_index = operations
        .iter()
        .position(|operation| operation.starts_with("done:"))
        .expect("必须 push_done");
    assert!(failure_index < ended[0] && ended[0] < done_index);
    let turn_end: ExecutorEvent = serde_json::from_str(&operations[ended[0]]).unwrap();
    assert!(
        matches!(
            turn_end,
            ExecutorEvent::TurnEnded {
                status: TurnStatus::Error,
                ..
            }
        ),
        "准入失败必须是 Error 终态，不是 Interrupted: {turn_end:?}"
    );
    assert_eq!(sink.push_done_count(), 1, "push_done 恰好一次");
}

// ── 契约 2：未 ready / 失败 / timeout 的首个 prompt 必须是 fatal，且不进入 Reason ──

/// transport / initialize 失败（`System MCP` 已 Failed）→ 首个 prompt fatal，
/// 模型 0 次调用。文案只保留阶段类别，不含对端原文。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn system_mcp_transport_failure_fails_first_prompt_without_model_call() {
    let harness = McpStartupHarness::initialized(serde_json::json!({
        "sys": system_server("init_error", "", serde_json::json!([]), 5_000),
    }))
    .await;
    harness.await_failed("sys").await;

    let sink = Arc::new(MockEventSink::new());
    let model = CountingModel::new();
    let result = run_prompt(
        harness.session_context("mcp-v4-init-failure"),
        &sink,
        &model,
    )
    .await;

    assert_fatal_without_reason(
        &result,
        &sink,
        &model,
        "System MCP \"sys\" 启动失败：transport 或协议初始化失败",
    );
    let failure = result.failure.expect("fatal failure");
    assert!(
        !failure
            .public_message
            .contains("fixture initialize failure"),
        "错误文案不得泄漏对端原文: {}",
        failure.public_message
    );
}

/// 连接与协商都好、但 live `tools/list` 未完成 → 仍不得 ready：
/// 这是「`Connected` + 无工具清单」被读成 ready 的原始风险面，只有真实成功的
/// `tools/list` 才算发现证据（空数组是成功结果，未回应不是）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn system_mcp_connected_without_tool_discovery_is_not_ready() {
    let harness = McpStartupHarness::new(serde_json::json!({
        "sys": system_server("hang_list", "", serde_json::json!([]), 800),
    }));

    let sink = Arc::new(MockEventSink::new());
    let model = CountingModel::new();
    let result = run_prompt(
        harness.session_context("mcp-v4-no-discovery"),
        &sink,
        &model,
    )
    .await;

    assert_fatal_without_reason(&result, &sink, &model, "启动超时（800ms），未发布 ready");
}

/// `tools/list` 失败 → 不得被读成「空工具列表」：即使 `system_mcp_tools = []`
/// 也必须 fatal（契约 4 的两条分支之一）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn system_mcp_tool_discovery_failure_is_not_an_empty_tool_list() {
    let harness = McpStartupHarness::initialized(serde_json::json!({
        "sys": system_server("list_error", "", serde_json::json!([]), 5_000),
    }))
    .await;
    harness.await_failed("sys").await;

    let sink = Arc::new(MockEventSink::new());
    let model = CountingModel::new();
    let result = run_prompt(
        harness.session_context("mcp-v4-list-failure"),
        &sink,
        &model,
    )
    .await;

    assert_fatal_without_reason(
        &result,
        &sink,
        &model,
        "System MCP \"sys\" 启动失败：tools/list 失败",
    );
}

/// 对端不响应 → deadline 到期是**终态失败**，不是取消：模型 0 次调用、
/// 不是 `Cancelled` stop reason、不是 `Interrupted` 终态。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn system_mcp_timeout_is_fatal_not_cancelled() {
    let harness = McpStartupHarness::new(serde_json::json!({
        "sys": system_server("hang", "", serde_json::json!([]), 800),
    }));

    let sink = Arc::new(MockEventSink::new());
    let model = CountingModel::new();
    let result = run_prompt(harness.session_context("mcp-v4-timeout"), &sink, &model).await;

    assert_fatal_without_reason(&result, &sink, &model, "启动超时（800ms），未发布 ready");
    assert_ne!(
        result.stop_reason,
        PromptStopReason::Cancelled,
        "timeout 不是取消"
    );
    assert!(
        !result
            .failure
            .as_ref()
            .expect("fatal failure")
            .public_message
            .contains("已取消"),
        "timeout 不得映射为取消文案"
    );
}

/// peer 断开（子进程收到请求即退出）→ 同样在 Reason 之前 fatal。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn system_mcp_disconnected_peer_fails_first_prompt() {
    let harness = McpStartupHarness::initialized(serde_json::json!({
        "sys": system_server("peer_exit", "", serde_json::json!([]), 5_000),
    }))
    .await;
    harness.await_failed("sys").await;

    let sink = Arc::new(MockEventSink::new());
    let model = CountingModel::new();
    let result = run_prompt(harness.session_context("mcp-v4-peer-exit"), &sink, &model).await;

    assert_fatal_without_reason(
        &result,
        &sink,
        &model,
        "System MCP \"sys\" 启动失败：transport 或协议初始化失败",
    );
}

/// 连接与 `tools/list` 都成功但缺必需工具 → 仍不得放行（all-or-nothing）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn system_mcp_missing_required_tool_fails_before_reason() {
    let harness = McpStartupHarness::initialized(serde_json::json!({
        "sys": system_server("tools", "other", serde_json::json!(["echo"]), 5_000),
    }))
    .await;
    harness.await_connected("sys").await;

    let sink = Arc::new(MockEventSink::new());
    let model = CountingModel::new();
    let result = run_prompt(
        harness.session_context("mcp-v4-missing-tool"),
        &sink,
        &model,
    )
    .await;

    assert_fatal_without_reason(&result, &sink, &model, "未提供必需工具 \"echo\"");
}

// ── 契约 3 / 4：ready 后首个 LLM 请求的 tools 入参 ──────────────────────────────

/// 必需工具在第一个真实 LLM 请求中就直接可见（无需 ToolSearch），而普通 MCP 工具
/// 仍是 deferred（只出现在 deferred 摘要里）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn system_mcp_ready_exposes_required_tools_on_first_model_request() {
    let harness = McpStartupHarness::initialized(serde_json::json!({
        "sys": system_server("tools", "echo,glob", serde_json::json!(["echo"]), 5_000),
        "ord": ordinary_server("tools", "ping"),
    }))
    .await;
    harness.await_connected("sys").await;
    harness.await_connected("ord").await;

    let sink = Arc::new(MockEventSink::new());
    let model = CountingModel::new();
    let result = run_prompt(harness.session_context("mcp-v4-ready-tools"), &sink, &model).await;

    assert!(
        result.ok,
        "准入成功后 prompt 必须正常结束: stop={:?}",
        result.stop_reason
    );
    assert!(
        result.failure.is_none(),
        "准入成功后不得有 failure: {:?}",
        result.failure
    );
    assert_eq!(model.call_count(), 1, "首个 prompt 恰好一次模型调用");

    let tools = model.first_request_tool_names();
    assert!(
        tools.iter().any(|name| name == "mcp__sys__echo"),
        "必需工具必须直接出现在首个 LLM 请求: {tools:?}"
    );
    assert!(
        !tools.iter().any(|name| name == "mcp__sys__glob"),
        "非必需的同 server 工具不得被提升为 direct: {tools:?}"
    );
    assert!(
        !tools.iter().any(|name| name == "mcp__ord__ping"),
        "普通 MCP 工具必须保持 deferred: {tools:?}"
    );

    let system = model.first_request_system_text();
    assert!(
        system.contains("## Deferred Tools") && system.contains("mcp__ord__ping"),
        "所有 deferred 工具（含普通 MCP）仍必须经 ToolSearch 摘要可见"
    );
}

/// 契约 4：`system_mcp_tools = []` 只要求 ready，不注入额外工具——
/// 同一台 server 的工具全部保持 deferred。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn system_mcp_empty_required_tools_ready_without_injection() {
    let harness = McpStartupHarness::initialized(serde_json::json!({
        "sys": system_server("tools", "echo", serde_json::json!([]), 5_000),
    }))
    .await;
    harness.await_connected("sys").await;

    let sink = Arc::new(MockEventSink::new());
    let model = CountingModel::new();
    let result = run_prompt(
        harness.session_context("mcp-v4-empty-required"),
        &sink,
        &model,
    )
    .await;

    assert!(
        result.ok,
        "空必需工具数组必须放行: stop={:?}",
        result.stop_reason
    );
    assert_eq!(model.call_count(), 1, "ready 后正常进入 Reason");
    let tools = model.first_request_tool_names();
    assert!(
        !tools.iter().any(|name| name == "mcp__sys__echo"),
        "空数组不得注入任何 direct 工具: {tools:?}"
    );
    let system = model.first_request_system_text();
    assert!(
        system.contains("mcp__sys__echo"),
        "未提升的工具仍应在 deferred 摘要中可见"
    );
}

// ── 契约 2 例外：普通 MCP 永不阻塞启动 ─────────────────────────────────────────

/// ordinary MCP 永久 pending（对端不响应）时，prompt 仍到达模型；system 依赖满足
/// 即可放行。断言时 ordinary 仍**在连接中**，证明确实没有被等待。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn ordinary_mcp_pending_does_not_block_startup() {
    let harness = McpStartupHarness::new(serde_json::json!({
        "sys": system_server("tools", "echo", serde_json::json!(["echo"]), 5_000),
        "ord": ordinary_server("hang", ""),
    }));
    harness.await_connected("sys").await;

    let sink = Arc::new(MockEventSink::new());
    let model = CountingModel::new();
    let result = run_prompt(
        harness.session_context("mcp-v4-ordinary-pending"),
        &sink,
        &model,
    )
    .await;

    assert!(
        result.ok,
        "普通 MCP pending 不得阻塞启动: stop={:?}",
        result.stop_reason
    );
    assert_eq!(model.call_count(), 1, "模型必须被调用");
    assert!(
        model
            .first_request_tool_names()
            .iter()
            .any(|name| name == "mcp__sys__echo"),
        "system 依赖满足后必需工具仍须直接可见"
    );
    assert!(
        harness
            .init_task
            .as_ref()
            .is_some_and(|task| !task.is_finished()),
        "ordinary 此时仍应在连接中——启动准入没有等它"
    );
}

/// ordinary MCP 直接初始化失败（未声明 `system_mcp`）同样不得阻塞启动：只有
/// `system_mcp == Some(true)` 的 server 是启动依赖。与「同一 fixture 声明为 system
/// 即 fatal」形成对照，固定 fatal 由启动依赖判定产生，而非 fixture 失败本身。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn ordinary_mcp_failure_does_not_block_startup() {
    let harness = McpStartupHarness::initialized(serde_json::json!({
        "sys": system_server("tools", "echo", serde_json::json!(["echo"]), 5_000),
        "ord": ordinary_server("init_error", ""),
    }))
    .await;
    harness.await_connected("sys").await;
    harness.await_failed("ord").await;

    let sink = Arc::new(MockEventSink::new());
    let model = CountingModel::new();
    let result = run_prompt(
        harness.session_context("mcp-v4-ordinary-failure"),
        &sink,
        &model,
    )
    .await;

    assert!(
        result.ok,
        "普通 MCP 失败不得阻碍启动: stop={:?} failure={:?}",
        result.stop_reason, result.failure
    );
    assert_eq!(model.call_count(), 1, "模型必须被调用");
}

/// 闸门位置固定：输入已被 Receive 接纳（transcript 出现该 human 消息），但既无
/// assistant 输出也无线工具调用——即「Receive 之后、Compact / Reason 之前」。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn system_mcp_gate_runs_after_receive_and_before_reason() {
    let harness = McpStartupHarness::initialized(serde_json::json!({
        "sys": system_server("init_error", "", serde_json::json!([]), 5_000),
    }))
    .await;
    harness.await_failed("sys").await;

    let sink = Arc::new(MockEventSink::new());
    let model = CountingModel::new();
    let result = run_prompt(
        harness.session_context("mcp-v4-receive-order"),
        &sink,
        &model,
    )
    .await;

    assert!(!result.ok);
    assert_eq!(model.call_count(), 0, "Compact / Reason 都不得发生");
    assert!(
        result.messages.iter().any(|message| matches!(
            message,
            BaseMessage::Human { content, .. } if content.text_content().contains("system mcp startup probe")
        )),
        "首个 prompt 的输入必须已被 Receive 接纳: {:?}",
        result.messages
    );
    assert!(
        !result
            .messages
            .iter()
            .any(|message| matches!(message, BaseMessage::Ai { .. })),
        "未进入 Reason 就不得有 assistant 消息: {:?}",
        result.messages
    );
    assert_eq!(
        sink.operations()
            .iter()
            .filter(|operation| operation.contains("agent_execution_failed"))
            .count(),
        1,
        "失败事件恰好一次"
    );
}
