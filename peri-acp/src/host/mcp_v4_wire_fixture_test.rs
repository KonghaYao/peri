//! MCP adaptation v4-part-2（wave 1）host 侧 wire 夹具 —— 主 plan A10 / A12。
//!
//! **owner 序列**：V-06（W0：建夹具 + 录迁移前基线）→ V-02（W4：复用本夹具收口
//! 「被提升为 direct 的工具走完整审批链 + wire 事实」）。本文件只在 `#[cfg(test)]`
//! 下编译，不进入任何生产装配路径。
//!
//! ## 本夹具提供什么
//!
//! 1. [`WIRE_FIXTURE_SCRIPT`]：node 侧 stdio MCP 对端，与
//!    `peri-middlewares/tests/mcp_isolation_contract.rs` 的 `FIXTURE_SERVER_JS` 同形态 ——
//!    **每条**收到的 JSON-RPC 行先落**自己的** wire 日志（`#recv <payload>`），并且有
//!    **显式 `tools/call` 分支**（回带身份的固定结果 `{server}:{tool}:ok`）；
//!    `server/discover` 一律 `-32601`，驱动客户端 Auto 回退 legacy `initialize`。
//!    **不得**改用 `mcp_v4_startup_test.rs` 的 `FIXTURE_SCRIPT`：那个脚本没有 wire 日志、
//!    没有 `tools/call` 分支（`:101` 兜底回 `-32601`），会让 wire 断言永远不成立却看似运行。
//! 2. [`WireFixtureHarness`]：真实 `.mcp.json` + 真实 `run_initialize` + 真实
//!    `McpClientPool` + HOME 重定向；server 名是**异名** `wire_fixture`（A3 禁止占用
//!    `web` / `artifact` 等保留实例名）。
//! 3. [`WireScriptedModel`]：**复刻** `PtcScriptedModel`（`executor_flow_test.rs:2153`
//!    的私有 struct，不得 `use`、也不得改动其宿主文件）的「能返回工具调用」的 model
//!    替身；同时记录每次 `stream` 调用时模型**实际看到**的工具名。
//!
//! ## 迁移前基线（A12）
//!
//! [`baseline_first_model_request_reports_web_and_artifact_capabilities`] 在**迁移前
//! HEAD** 上录下「首个 LLM 请求里模型实际看到的工具名集合」。同一夹具、同一命令由
//! V-02（W4）与 V-04（W5）**对照重跑**，现场输出逐字记录在
//! `spec/issues/2026-09-26-mcp-adaptation-v4-part-2-acceptance.md` §2（该小节只增不改）。
//!
//! 因此该断言写成「两种命名**恰有其一**」：迁移前命中裸名（`WebSearch` / `WebFetch` /
//! `artifact`，三者迁移前均 direct：`web_fetch.rs:95`、`web_search.rs:76`、
//! `artifact/tool.rs:109`），迁移后命中 IF-D5 的冻结 effective name。断言同时排除
//! 「两者都在」（重复暴露）与「两者都不在」（能力净丢失）——这正是「可见性等价」要
//! 证伪的两种形态。

//! ## W4（V-02）新增的两个 seam
//!
//! 1. 条目可见性提升为 `pub(super)`（`host` 树内可见）：V-02 的用例落在同属 `host` 的
//!    兄弟模块 `host::mcp_v4_builtin`，需要复用同一个宿主与同一个 model 替身；这只改
//!    测试模块内部的可见性，**不进入生产装配路径**（整文件是 `#[cfg(test)]`）。
//! 2. [`WireFixtureHarness::new_with_servers`]：在夹具 `wire_fixture` 之外追加**异名**
//!    用户 server 条目（例如「system 依赖缺失」的失败注入），用于 V-02 的 fatal 路径；
//!    既有 [`WireFixtureHarness::new`] 行为逐位不变（追加集合为空）。
//! 3. [`WireScriptedModel::first_request_system_text`]：首个请求的系统消息文本，供
//!    「三个冻结 effective name 不得同时算作 deferred」断言使用。

use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Mutex, MutexGuard, OnceLock,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures::{stream, StreamExt};
use peri_acp_types::{messages::MessageContent, ports::McpPoolPort};
use peri_middlewares::mcp::{ClientStatus, McpClientPool, McpInitStatus, McpTaskOwner};
use peri_model::{
    JsonObject, Model, ModelCapabilities, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, ModelStreamEvent, StopReason, ToolCall, ToolDefinition as ModelToolDefinition,
};
use serial_test::serial;
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use super::executor_flow_tests::{
    make_session_context, make_stage_build, make_turn_input, MockEventSink,
};
use crate::session::executor::{run_session_loop, FrozenSessionData, PromptResult, SessionContext};

/// 夹具 server 名。**异名**：A3 规定 `web` / `artifact`（及预留 `cron` / `lsp` /
/// `workspace`）是保留实例名，夹具不得占用。
pub(super) const WIRE_FIXTURE_SERVER_NAME: &str = "wire_fixture";

/// 夹具脚本文件名（写在临时 workspace 内，作为 stdio `args[0]`）。
const WIRE_FIXTURE_SCRIPT_FILE: &str = "wire_fixture.js";

/// 夹具脚本声明的工具清单（argv[2] = 逗号分隔）。
const WIRE_FIXTURE_TOOLS: &str = "echo,glob";

/// 夹具 server 侧 `tools/call` 回带的身份文本模板里的两个组成（断言用）。
const WIRE_FIXTURE_TOOL: &str = "echo";

/// 夹具自身工具的模型面名字（`mcp__{server}__{tool}` 形态）。
/// 这是**本夹具 server** 的名字，与 IF-D5 的 builtin 冻结字面量无关，也不是第二张
/// builtin 反查表 —— 归一唯一入口是 IF-D15 helper（W1 由 E-01 落地）。
pub(super) const WIRE_FIXTURE_ECHO_EFFECTIVE_NAME: &str = "mcp__wire_fixture__echo";

/// 迁移前后「三个 capability 的模型面名字」两种命名（裸名 ↔ IF-D5 冻结 effective name）。
/// **只作对照目标**：本夹具不对它做任何按名判定或策略匹配，也不参与生产归一。
pub(super) const WEB_ARTIFACT_CAPABILITIES: [(&str, &str); 3] = [
    ("WebSearch", "mcp__web__WebSearch"),
    ("WebFetch", "mcp__web__WebFetch"),
    ("artifact", "mcp__artifact__artifact"),
];

/// 带 wire 日志与 `tools/call` 分支的 stdio MCP 对端（node）。
///
/// 日志与观测原语（供审批 + wire 断言）：
/// - `#boot <server> pid=<pid>`：进程起来即写，证明夹具确实被启动；
/// - `#recv <payload>`：**每条**收到的 JSON-RPC 行原样落盘；解析这些行即「wire 事实」，
///   不依赖任何 mock 计数。
///
/// 环境变量：`WIRE_FIXTURE_LOG`（日志路径）、`WIRE_FIXTURE_SERVER`（server 名）。
const WIRE_FIXTURE_SCRIPT: &str = r#"
const fs = require('node:fs');
const readline = require('node:readline');

const server = process.env.WIRE_FIXTURE_SERVER || 'wire_fixture';
const tools = (process.argv[2] || '').split(',').filter(Boolean);
const logPath = process.env.WIRE_FIXTURE_LOG;

const log = (line) => fs.appendFileSync(logPath, `${line}\n`);
log(`#boot ${server} pid=${process.pid}`);

const rl = readline.createInterface({ input: process.stdin });
rl.on('line', (line) => {
  log(`#recv ${line}`);
  let request;
  try {
    request = JSON.parse(line);
  } catch (error) {
    return;
  }
  if (request.id === undefined) return;
  const reply = (result) =>
    process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id: request.id, result })}\n`);
  const refuse = (code, message) =>
    process.stdout.write(
      `${JSON.stringify({ jsonrpc: '2.0', id: request.id, error: { code, message } })}\n`,
    );
  switch (request.method) {
    case 'initialize':
      reply({ protocolVersion: '2025-11-25', capabilities: {}, serverInfo: { name: server, version: '1.0.0-wire' } });
      break;
    case 'tools/list':
      reply({
        tools: tools.map((name) => ({
          name,
          description: `${server} fixture tool ${name}`,
          inputSchema: { type: 'object', properties: {} },
        })),
      });
      break;
    case 'tools/call': {
      const name = (request.params && request.params.name) || '';
      reply({ content: [{ type: 'text', text: `${server}:${name}:ok` }] });
      break;
    }
    case 'resources/list':
      reply({ resources: [] });
      break;
    case 'ping':
      reply({});
      break;
    default:
      refuse(-32601, 'Method not found');
  }
});
"#;

/// HOME 重定向守卫：`load_merged_config_full` 读 `~/.peri/settings.json`，测试必须走
/// 临时 HOME，避免启动开发者本机的 MCP server / 读入真实凭据（§9 规则 7）。
struct WireHomeRedirect {
    _lock: MutexGuard<'static, ()>,
    previous: Option<OsString>,
}

impl WireHomeRedirect {
    fn set(home: &Path) -> Self {
        static HOME_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        // 一个用例 panic 不得毒化 HOME 重定向，使后续用例连带失败。
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

impl Drop for WireHomeRedirect {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// 追加的**异名**用户 MCP 条目（V-02 的 fatal 路径注入）。
///
/// 只声明「名字 / 工具清单 / 必需工具」三件事：脚本一律复用 [`WIRE_FIXTURE_SCRIPT`]，
/// wire 日志写到本条目**自己的**文件（不污染 `wire_fixture` 的 wire 事实）。
pub(super) struct ExtraServer {
    /// server 名（必须是异名，不得占用 A3 的保留实例名）。
    pub(super) name: &'static str,
    /// `tools/list` 返回的工具名（argv[2]，逗号分隔）。
    pub(super) tools: &'static str,
    /// `Some` ⇒ 声明 `system_mcp: true` + `system_mcp_tools`（启动依赖）。
    pub(super) required: Option<&'static [&'static str]>,
}

/// wire 夹具宿主：真实 `.mcp.json` + 真实 `run_initialize` + 真实 pool + wire 日志。
pub(super) struct WireFixtureHarness {
    _home: WireHomeRedirect,
    _tmp: tempfile::TempDir,
    pool: Arc<McpClientPool>,
    _owner: McpTaskOwner,
    init_task: Option<tokio::task::JoinHandle<()>>,
    wire_log: PathBuf,
}

impl WireFixtureHarness {
    /// 写入夹具脚本与项目级 `.mcp.json`（`wire_fixture` 声明为 System MCP），
    /// 并在临时 HOME 下启动真实初始化。
    ///
    /// `extra` 为空时与迁移前基线（A12）的夹具**逐位相同**。
    fn new_with_servers(extra: &[ExtraServer]) -> Self {
        let tmp = tempfile::TempDir::new().expect("临时目录");
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("workspace");
        let claude_home = tmp.path().join("claude");
        for dir in [&home, &workspace, &claude_home] {
            std::fs::create_dir_all(dir).expect("创建临时目录");
        }
        std::fs::write(
            workspace.join(WIRE_FIXTURE_SCRIPT_FILE),
            WIRE_FIXTURE_SCRIPT,
        )
        .expect("写入 wire 夹具脚本");
        let wire_log = tmp.path().join("wire.log");
        let mut servers = serde_json::json!({
            WIRE_FIXTURE_SERVER_NAME: wire_fixture_server_config(&wire_log),
        });
        for server in extra {
            let entry_log = tmp.path().join(format!("{}.wire.log", server.name));
            let map = servers
                .as_object_mut()
                .expect("mcpServers 必须是 object")
                .entry(server.name)
                .or_insert_with(|| serde_json::json!({}));
            if let Some(object) = map.as_object_mut() {
                object.insert(
                    "command".to_string(),
                    serde_json::Value::String("node".to_string()),
                );
                object.insert(
                    "args".to_string(),
                    serde_json::json!([WIRE_FIXTURE_SCRIPT_FILE, server.tools]),
                );
                object.insert(
                    "env".to_string(),
                    serde_json::json!({
                        "WIRE_FIXTURE_LOG": entry_log.to_string_lossy(),
                        "WIRE_FIXTURE_SERVER": server.name,
                    }),
                );
                if let Some(required) = server.required {
                    object.insert("system_mcp".to_string(), serde_json::Value::Bool(true));
                    object.insert("system_mcp_tools".to_string(), serde_json::json!(required));
                    object.insert("system_mcp_timeout".to_string(), serde_json::json!(10_000));
                }
            }
        }
        std::fs::write(
            workspace.join(".mcp.json"),
            serde_json::json!({ "mcpServers": servers }).to_string(),
        )
        .expect("写入项目级 MCP 配置");

        let home_guard = WireHomeRedirect::set(&home);
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
            wire_log,
        }
    }

    /// 等待真实初始化收口（所有 server 都已得出连接结论）。
    pub(super) async fn initialized() -> Self {
        Self::initialized_with_servers(&[]).await
    }

    /// 同上，但额外写入 `extra` 声明的**异名**用户 server（V-02 的 fatal 路径使用）。
    pub(super) async fn initialized_with_servers(extra: &[ExtraServer]) -> Self {
        let mut harness = Self::new_with_servers(extra);
        if let Some(task) = harness.init_task.take() {
            tokio::time::timeout(Duration::from_secs(30), task)
                .await
                .expect("真实 MCP 初始化不得挂起")
                .expect("初始化任务不得 panic");
        }
        harness
    }

    /// 夹具 pool（公开可见事实：句柄状态 / `all_server_infos()` 的 transport 分类）。
    pub(super) fn pool(&self) -> &Arc<McpClientPool> {
        &self.pool
    }

    /// 注入夹具 pool 的 session 装配面（真实 assembler 会据此构造 McpMiddleware）。
    pub(super) fn session_context(&self, session_id: &str) -> SessionContext {
        let mut ctx = make_session_context(session_id);
        ctx.mcp_pool = Some(Arc::clone(&self.pool) as Arc<dyn McpPoolPort>);
        ctx
    }

    /// 有界轮询公开可见的句柄状态（不用固定 sleep 猜时序）。
    pub(super) async fn await_connected(&self, server: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(handle) = self.pool.get_client(server) {
                if matches!(handle.status, ClientStatus::Connected) {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "{server} 的公开状态在 20s 内未变成 Connected"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// 夹具对端的 wire 日志原文（`#boot` / `#recv` 行）。
    fn wire_lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.wire_log)
            .map(|text| text.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// wire 事实：按收到顺序解析 `#recv` 行（每条即客户端真实发出的 JSON-RPC 请求）。
    pub(super) fn wire_received_requests(&self) -> Vec<serde_json::Value> {
        self.wire_lines()
            .into_iter()
            .filter_map(|line| line.strip_prefix("#recv ").map(str::to_string))
            .filter_map(|payload| serde_json::from_str(&payload).ok())
            .collect()
    }
}

/// 夹具 server 的 `.mcp.json` 条目：System MCP、必列为 `echo`（`glob` 留作 deferred 对照）。
fn wire_fixture_server_config(wire_log: &Path) -> serde_json::Value {
    serde_json::json!({
        "command": "node",
        "args": [WIRE_FIXTURE_SCRIPT_FILE, WIRE_FIXTURE_TOOLS],
        "env": {
            "WIRE_FIXTURE_LOG": wire_log.to_string_lossy(),
            "WIRE_FIXTURE_SERVER": WIRE_FIXTURE_SERVER_NAME,
        },
        "system_mcp": true,
        "system_mcp_tools": [WIRE_FIXTURE_TOOL],
        "system_mcp_timeout": 10_000,
    })
}

/// 一条脚本化的工具调用。
pub(super) struct ScriptedToolCall {
    id: String,
    name: String,
    arguments: serde_json::Value,
}

impl ScriptedToolCall {
    /// `id` 由工具名推导：单次脚本调用足以覆盖本轮断言面（V-03 的 approve/reject）。
    pub(super) fn new(name: impl Into<String>, arguments: serde_json::Value) -> Self {
        let name = name.into();
        Self {
            id: format!("wire-scripted-{name}"),
            name,
            arguments,
        }
    }
}

/// 「能返回工具调用」的 model 替身（复刻 `PtcScriptedModel` 的形态；见模块文档第 3 条）。
///
/// 行为：第 `i` 次 `stream` 返回 `script[i]` 的工具调用（`StopReason::ToolUse`）；
/// 脚本耗尽后返回 [`WireScriptedModel::end_text`] + `StopReason::EndTurn`。
pub(super) struct WireScriptedModel {
    calls: AtomicUsize,
    script: Vec<ScriptedToolCall>,
    end_text: String,
    /// 每次 `stream` 调用时模型实际看到的工具名（按调用次序）；`[0]` 即首个 LLM 请求。
    request_tool_names: Mutex<Vec<Vec<String>>>,
    /// 每次 `stream` 调用时首条（system）消息文本；`[0]` 即首个 LLM 请求的 deferred 摘要面。
    request_system_texts: Mutex<Vec<String>>,
}

impl WireScriptedModel {
    pub(super) fn new(script: Vec<ScriptedToolCall>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            script,
            end_text: "wire fixture done".to_string(),
            request_tool_names: Mutex::new(Vec::new()),
            request_system_texts: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// 首个 LLM 请求的工具名集合（`ModelRequest.tools` 即模型真实收到的列表）。
    pub(super) fn first_request_tool_names(&self) -> Vec<String> {
        self.request_tool_names
            .lock()
            .unwrap()
            .first()
            .cloned()
            .unwrap_or_default()
    }

    /// 首个 LLM 请求的系统消息文本（ToolSearch deferred 摘要的断言面）。
    pub(super) fn first_request_system_text(&self) -> String {
        self.request_system_texts
            .lock()
            .unwrap()
            .first()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl Model for WireScriptedModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: true,
            supports_streaming: true,
            ..ModelCapabilities::default()
        }
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: AgentCancellationToken,
    ) -> ModelResult<ModelStream> {
        self.request_tool_names
            .lock()
            .unwrap()
            .push(request.tools.iter().map(|tool| tool.name.clone()).collect());
        self.request_system_texts.lock().unwrap().push(
            request
                .messages
                .first()
                .map(|message| message.text_content().unwrap_or_default())
                .unwrap_or_default(),
        );
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let mut events = Vec::new();
        let response = match self.script.get(call) {
            Some(scripted) => {
                let arguments = JsonObject::from_value(scripted.arguments.clone())
                    .expect("脚本化工具调用的参数必须是 JSON object");
                events.push(ModelStreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some(scripted.id.clone()),
                    name: Some(scripted.name.clone()),
                    arguments_delta: scripted.arguments.to_string(),
                });
                ModelResponse::new(
                    ModelMessage::assistant(
                        vec![],
                        vec![ToolCall::new(
                            scripted.id.clone(),
                            scripted.name.clone(),
                            arguments,
                        )],
                    ),
                    StopReason::ToolUse,
                    None,
                    None,
                )?
            }
            None => {
                events.push(ModelStreamEvent::TextDelta {
                    text: self.end_text.clone(),
                });
                ModelResponse::new(
                    ModelMessage::assistant_text(self.end_text.clone()),
                    StopReason::EndTurn,
                    None,
                    None,
                )?
            }
        };
        events.push(ModelStreamEvent::Completed(response));
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(events.into_iter().map(Ok)),
            cancellation,
        ))
    }
}

/// 跑一次真实 prompt：装配面注入脚本化 model，其余（链装配、启动闸门、终态投影）
/// 全部走生产路径。
pub(super) async fn run_wire_prompt(
    ctx: SessionContext,
    sink: &Arc<MockEventSink>,
    model: &Arc<WireScriptedModel>,
) -> PromptResult {
    run_wire_prompt_with_frozen(ctx, sink, model, None).await
}

/// 同上，但由调用方给定**会话级冻结数据**（V-02 用它在 host seam 注入
/// `disabled_middlewares`，逐面验证 IF-D10 的能力关闭）。
///
/// `frozen = None` 时与 [`run_wire_prompt`] 逐位相同（走 executor 的最小回落数据）。
pub(super) async fn run_wire_prompt_with_frozen(
    ctx: SessionContext,
    sink: &Arc<MockEventSink>,
    model: &Arc<WireScriptedModel>,
    frozen: Option<FrozenSessionData>,
) -> PromptResult {
    let model = Arc::clone(model);
    let mut ctx = ctx;
    ctx.primary_llm_factory = Some(Arc::new(move || Arc::clone(&model) as Arc<dyn Model>));
    let mut turn = make_turn_input(
        Arc::clone(sink) as Arc<dyn crate::session::event_sink::EventSink>,
        MessageContent::text("wire fixture probe"),
        false,
        vec![],
        make_stage_build(&ctx),
    );
    turn.frozen = frozen;
    tokio::time::timeout(Duration::from_secs(30), run_session_loop(ctx, turn))
        .await
        .expect("prompt 不得挂起")
}

// ── 夹具自检 1：node 脚本的 wire 日志与 tools/call 分支 ─────────────────────

/// 直接驱动 node 脚本（不经任何 MCP 客户端）：证明「每条收到的行都有 wire 记录」
/// 与「`tools/call` 有显式分支且回带身份」两条夹具前提成立 —— V-02/V-03 的 wire
/// 断言依赖它们。
#[cfg(not(windows))]
#[test]
fn wire_fixture_script_logs_wire_and_answers_tools_call() {
    let tmp = tempfile::TempDir::new().expect("临时目录");
    let script = tmp.path().join(WIRE_FIXTURE_SCRIPT_FILE);
    let wire_log = tmp.path().join("wire.log");
    std::fs::write(&script, WIRE_FIXTURE_SCRIPT).expect("写入夹具脚本");

    let mut child = Command::new("node")
        .arg(&script)
        .arg(WIRE_FIXTURE_TOOLS)
        .env("WIRE_FIXTURE_SERVER", WIRE_FIXTURE_SERVER_NAME)
        .env("WIRE_FIXTURE_LOG", &wire_log)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("node 必须可用于 wire 夹具自检");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });

    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"wire-fixture-selfcheck","version":"1"}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"query":"hello"}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"server/discover","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"no/such/method","params":{}}"#,
    ];
    for request in requests {
        writeln!(stdin, "{request}").expect("写入请求");
    }
    stdin.flush().expect("flush stdin");

    let mut replies = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while replies.len() < requests.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "夹具 30s 内未回满响应（已收到 {replies:?}）"
        );
        let line = rx
            .recv_timeout(remaining)
            .unwrap_or_else(|error| panic!("夹具未回响应: {error}"));
        replies.push(serde_json::from_str::<serde_json::Value>(&line).expect("响应必须是 JSON"));
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();

    let reply = |id: u64| {
        replies
            .iter()
            .find(|reply| reply["id"] == id)
            .unwrap_or_else(|| panic!("缺少 id={id} 的响应: {replies:?}"))
    };
    assert_eq!(
        reply(1)["result"]["serverInfo"]["name"],
        WIRE_FIXTURE_SERVER_NAME,
        "initialize 必须回带夹具身份"
    );
    let names: Vec<&str> = reply(2)["result"]["tools"]
        .as_array()
        .expect("tools/list 必须回 tools 数组")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(
        names,
        ["echo", "glob"],
        "tools/list 必须列出 argv 给出的工具"
    );
    assert_eq!(
        reply(3)["result"]["content"][0]["text"],
        format!("{WIRE_FIXTURE_SERVER_NAME}:{WIRE_FIXTURE_TOOL}:ok"),
        "tools/call 必须回带身份的固定结果"
    );
    assert_eq!(
        reply(4)["error"]["code"],
        -32601,
        "server/discover 必须 -32601，驱动 Auto 回退 legacy initialize"
    );
    assert_eq!(reply(5)["error"]["code"], -32601, "未知 method 必须 -32601");

    let log = std::fs::read_to_string(&wire_log).expect("wire 日志必须存在");
    assert!(
        log.contains(&format!("#boot {WIRE_FIXTURE_SERVER_NAME} pid=")),
        "wire 日志必须先写 #boot: {log}"
    );
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("#recv "))
            .count(),
        requests.len(),
        "每条收到的行都必须落 wire 日志: {log}"
    );
    assert!(
        log.contains(r#""method":"tools/call""#) && log.contains(r#""name":"echo""#),
        "wire 日志必须含 tools/call 请求原文（裸名 echo，wire 上不出现 effective name）: {log}"
    );
}

// ── 夹具自检 2：脚本化 model 替身 ──────────────────────────────────────────

/// 直接驱动 [`WireScriptedModel`]（不经 React loop）：证明替身的两个观测面成立 ——
/// 「按脚本返回工具调用」与「记录模型实际看到的工具名」。
#[cfg(not(windows))]
#[tokio::test]
async fn wire_scripted_model_returns_scripted_tool_call_then_ends_turn() {
    let model = WireScriptedModel::new(vec![ScriptedToolCall::new(
        "mcp__wire_fixture__echo",
        serde_json::json!({ "query": "hello" }),
    )]);
    let request = ModelRequest {
        tools: vec![ModelToolDefinition::new(
            "mcp__wire_fixture__echo",
            JsonObject::from_value(serde_json::json!({ "type": "object", "properties": {} }))
                .expect("object schema"),
        )],
        ..Default::default()
    };

    let events: Vec<_> = model
        .stream(request.clone(), AgentCancellationToken::new())
        .await
        .expect("替身必须产出流")
        .collect()
        .await;
    let events: Vec<_> = events
        .into_iter()
        .map(|event| event.expect("事件不得为错"))
        .collect();
    assert_eq!(model.call_count(), 1);
    assert_eq!(
        model.first_request_tool_names(),
        vec!["mcp__wire_fixture__echo".to_string()],
        "替身必须记录模型实际看到的工具名"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            ModelStreamEvent::ToolCallDelta { name: Some(name), .. } if name == "mcp__wire_fixture__echo"
        )),
        "首发必须是工具调用增量: {events:?}"
    );
    let completed = events
        .iter()
        .find_map(|event| match event {
            ModelStreamEvent::Completed(response) => Some(response),
            _ => None,
        })
        .expect("必须产出 Completed");
    assert_eq!(completed.stop_reason(), &StopReason::ToolUse);
    match completed.message() {
        ModelMessage::Assistant { tool_calls, .. } => {
            assert_eq!(tool_calls.len(), 1);
            assert_eq!(tool_calls[0].name(), "mcp__wire_fixture__echo");
            assert_eq!(
                tool_calls[0].arguments(),
                &JsonObject::from_value(serde_json::json!({ "query": "hello" }))
                    .expect("object 参数")
            );
        }
        other => panic!("必须是 Assistant 消息: {other:?}"),
    }

    let events: Vec<_> = model
        .stream(request, AgentCancellationToken::new())
        .await
        .expect("替身必须产出流")
        .collect()
        .await;
    let events: Vec<_> = events
        .into_iter()
        .map(|event| event.expect("事件不得为错"))
        .collect();
    let completed = events
        .iter()
        .find_map(|event| match event {
            ModelStreamEvent::Completed(response) => Some(response),
            _ => None,
        })
        .expect("必须产出 Completed");
    assert_eq!(
        completed.stop_reason(),
        &StopReason::EndTurn,
        "脚本耗尽后必须以 EndTurn 收束"
    );
    assert_eq!(model.call_count(), 2);
}

// ── 迁移前基线（A12）：首个 LLM 请求的工具名 ───────────────────────────────

/// **W0 基线**：在真实生产装配路径上，录下首个 LLM 请求里模型实际看到的工具名集合。
///
/// 断言的三层：
/// 1. 夹具自证 —— 本次会话确实经真实 wire 由 `wire_fixture` 服务（`initialize` /
///    `tools/list` 都在 wire 日志里，且本轮不产生 `tools/call`）；
/// 2. 夹具实例的 direct 提升确实生效 —— `mcp__wire_fixture__echo` 在首个请求、
///    `mcp__wire_fixture__glob` 不在（`system_mcp_tools` 只列 `echo`）；
/// 3. **基线口径** —— 三个 Web / Artifact capability 在首个请求中可见，且恰以一种
///    命名出现：迁移前 = 裸名（`WebSearch` / `WebFetch` / `artifact`），迁移后 =
///    IF-D5 冻结 effective name。两者都在（重复）或都不在（能力净丢失）都必须红。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn baseline_first_model_request_reports_web_and_artifact_capabilities() {
    let harness = WireFixtureHarness::initialized().await;
    harness.await_connected(WIRE_FIXTURE_SERVER_NAME).await;

    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![]));
    let result = run_wire_prompt(
        harness.session_context("mcp-v4-wire-baseline"),
        &sink,
        &model,
    )
    .await;

    assert!(
        result.ok,
        "准入成功后 prompt 必须正常结束: stop={:?}",
        result.stop_reason
    );
    assert_eq!(model.call_count(), 1, "首个 prompt 恰好一次模型调用");

    let names = model.first_request_tool_names();
    // 现场证据：acceptance §2 逐字记录本行输出（V-04 用同一命令对照重跑）。
    println!(
        "[V-06 迁移前基线] 首个 LLM 请求工具数 = {}；工具名 = {names:?}",
        names.len()
    );

    let methods: Vec<String> = harness
        .wire_received_requests()
        .iter()
        .filter_map(|request| request["method"].as_str().map(str::to_string))
        .collect();
    assert!(
        methods.iter().any(|method| method == "initialize"),
        "夹具必须真实收到 initialize: {methods:?}"
    );
    assert!(
        methods.iter().any(|method| method == "tools/list"),
        "夹具必须真实收到 tools/list: {methods:?}"
    );
    assert!(
        !methods.iter().any(|method| method == "tools/call"),
        "本轮模型不调用工具，wire 上不得出现 tools/call: {methods:?}"
    );

    assert!(
        names
            .iter()
            .any(|name| name == WIRE_FIXTURE_ECHO_EFFECTIVE_NAME),
        "夹具实例的 direct 工具必须经真实 system_mcp_tools 提升进入首个请求: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "mcp__wire_fixture__glob"),
        "未列入 system_mcp_tools 的同 server 工具必须保持 deferred: {names:?}"
    );

    for (bare, effective) in WEB_ARTIFACT_CAPABILITIES {
        let bare_visible = names.iter().any(|name| name == bare);
        let effective_visible = names.iter().any(|name| name == effective);
        assert!(
            bare_visible ^ effective_visible,
            "capability `{bare}` 必须恰以一种命名出现在首个 LLM 请求（裸名={bare_visible}、\
             effective={effective_visible}）：{names:?}"
        );
    }
}
