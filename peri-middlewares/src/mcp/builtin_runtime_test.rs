//! Builtin runtime test fixtures; scenario modules below share this local test scope.
use std::sync::Arc;

use peri_agent::middleware::r#trait::Middleware;

use crate::mcp::builtin::closed_instances;
use crate::mcp::builtin::context::BuiltinInstanceContext;
use crate::mcp::{ClientStatus, McpClientHandle, McpClientPool, McpMiddleware};
// V-01（W4）：真实启动路径 / 审批 approve+reject + wire 计数 / 关闭矩阵四面 /
// 无 orphan / 大 payload（A16）/ 实例隔离可观察断言（A13）
//
// 证据边界（诚实声明，§9 规则 9 / A13；不得把本节读成「全部链路已端到端验证」）：
//
// **A. 真实启动路径夹具**（`StartupFixture`）：`run_initialize` → 生产 loader（含 step 6.5
// 默认层）→ 生产 transport（`spawn_builtin_transport`）→ 生产 handler → 真实握手与
// live `tools/list`。承担：连接事实、`peer_info`、builtin 身份、`system_mcp_tools ==
// 声明 direct`、`transport_type`、启动闸门候选（关闭矩阵面①）、实例隔离与 reconnect
// （A13 ①②）、无 orphan、以及经**生产 artifact handler** 的一次真实 `tools/call` 往返。
//
// **B. 线路观测夹具**（`TappedLink`，`spawn_builtin_transport_with_tap`——E-03 为
// A13 ④ 提供的 `#[cfg(test)]` per-instance wire 面）：承担线路 method 序列（modern
// 握手、线路无 `initialize`）、审批 approve/reject 两态的 `tools/call` **线路计数**、
// wire 上出现的工具名、大 payload（A16）、两实例 wire 不串（A13 ④/⑤）。
// 其 server handler 是**测试替身**（生产 handler 类型在私有模块 `mcp::builtin::web` /
// `artifact` 内，crate 内不可命名）：替身与生产 handler 同形——不覆写 `discover`、
// `get_info` / `list_tools` / `call_tool` 三个覆写点一致、工具表按**注册表声明**构造、
// `tools/call` 按**原始名**路由并按 IF-D14 的三形态简化复刻（未知 → `invalid_params`、
// `Ok` → success、`Err` → error 文本）；因此「名字 / direct / 审批 / 线路」这条链路与
// 生产同构，**替身只替换工具核心**（Web 两个工具的后端地址在生产恒为编译期常量；真工具体
// 指向本地回环桩的形态由 `mcp::builtin::web` 的
// `web_handler_tools_call_reaches_real_http_stub_over_wire` 覆盖）。生产 handler 的协议行为由 `mcp::builtin::web` / `mcp::builtin::artifact` /
// `mcp::builtin::runtime` 覆盖。
//
// **不覆盖（UNVERIFIED，A13 强制项）**：capability root 隔离与凭据隔离在 builtin 形态下
// **不可证伪**——`capability_profile` 是 pool 级字段（`mcp/apps.rs`，构造点
// `McpClientPool::new_pending_with_spawner_and_profile`）、`bind_execution_cwd` 也是
// pool 级（`mcp/initialize.rs`），`McpClientHandle` 无凭据字段。本文件**不**用「两个实例
// 构造参数不同」「两个 handler 是不同 struct」这类代码阅读结论替代运行时证据。
// 同样未覆盖：真实网络调用（Web 两个工具与 artifact 真实上传都依赖外部服务）。
// ══════════════════════════════════════════════════════════════════════════════════

use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::builtin_mcp::{find, BuiltinMcpInstance, BUILTIN_MCP_INSTANCES};
use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use peri_agent::agent::react::ToolCall;
use peri_agent::agent::AgentCancellationToken;
use peri_agent::error::{AgentError, AgentResult};
use peri_agent::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, UserInteractionBroker,
};
use peri_agent::middleware::capabilities as hook_state;
use peri_agent::session::tool_catalog::{StartupRequiredTool, StartupToolUpdate};
use peri_agent::tools::{BaseTool, EffectiveToolError, EffectiveToolErrorCode, ToolContext};
use peri_mcp_lsp::config::{LspConfigFile, LspServerConfig};
use peri_mcp_lsp::pool::LspServerPool;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode,
    Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo,
    Tool as RmcpTool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde_json::{json, Value};
use tokio::sync::Notify;

use crate::assembly::{default_workflow_middleware_factory_with_pool, open_builtin_bridges};
use crate::mcp::builtin::context::{CronInstanceInput, LspInstanceInput};
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_tap, BuiltinServerExit, BuiltinServerTask, BuiltinWireLog,
    TickCloseOutcome, BUILTIN_CONVERGE_TIMEOUT, BUILTIN_TICK_INTERVAL,
};
use crate::mcp::builtin::BUILTIN_INJECTION_ENV;
use crate::mcp::client::{
    serve_client_auto, McpConnectionKey, McpInitStatus, McpServiceWrapper, SystemMcpManifest,
};
use crate::mcp::initialize::list_discovered_tools;
use crate::mcp::tool_bridge::{build_typed_tool_bridges, McpToolBridge};
use crate::permission::{default_requires_approval, PermissionMiddleware};
use peri_mcp_cron::CronScheduler;

/// 握手/关闭上界：同进程链路，取值远大于实测（与 `runtime_test` 同口径）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(1000);
/// 大请求帧（A16 的反向；大**结果**方向见 `mcp::builtin::runtime::tests` 的
/// `large_payload_round_trips_intact`）：参数里的字符串长度，远大于
/// `BUILTIN_DUPLEX_BUF`（8 KiB）。
const LARGE_INPUT_BYTES: usize = 200 * 1024;

// ─── 夹具环境隔离 ────────────────────────────────────────────────────────────────

/// `HOME` / `USERPROFILE` / `PERI_MCP_BUILTIN` 的显式隔离 guard（drop 时还原）。
///
/// 生产 loader 的全局配置路径由 `dirs_next::home_dir()` 推导（`mcp/config.rs` 的
/// `load_merged_config_full`）：不重定向就会读到运行者本机的 `~/.peri/settings.json`
/// ——用例会变成环境依赖（本机装了同名 server 时还会触发 A3 的保留名 typed error）。
/// `PERI_MCP_BUILTIN` 同时置空：默认层注入语义 = 缺省（注入全部已实现实例）。
///
/// 与 `mcp::builtin::tests` 的 `BuiltinEnvGuard` / `hooks::loader_test::HomeGuard` 同一
/// 模式（进程级 env + 文件排他锁）。断言消息**不得**回显 env 取值（§9 规则 7）。
struct LoaderEnvGuard {
    _lock: peri_mcp_common::process_env::EnvLockFile,
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl LoaderEnvGuard {
    fn redirect(home: &Path) -> Self {
        Self::redirect_with_builtin_env(home, None)
    }

    /// 同 [`Self::redirect`]，但 `PERI_MCP_BUILTIN` 取显式值（`None` = 移除该变量）。
    ///
    /// 既有 `redirect` 逐位不变（委托到本函数、`builtin_env = None`）；WP-MW 的
    /// `PERI_MCP_BUILTIN=off` 零注入用例需要这一份取值。
    fn redirect_with_builtin_env(home: &Path, builtin_env: Option<&str>) -> Self {
        let lock = peri_mcp_common::process_env::lock().expect("进程环境锁");
        let home = home.to_string_lossy().into_owned();
        let mut previous = Vec::new();
        for (key, value) in [
            ("HOME", Some(home.clone())),
            ("USERPROFILE", Some(home)),
            (
                BUILTIN_INJECTION_ENV,
                builtin_env.map(|value| value.to_string()),
            ),
        ] {
            previous.push((key, std::env::var_os(key)));
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for LoaderEnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

// ─── 注册表派生 helpers（断言一律从注册表派生，不硬编码名字）────────────────────

/// 实例声明为 direct 的原始工具名（顺序 = 声明顺序）。
fn declared_direct_original_names(instance: &BuiltinMcpInstance) -> Vec<String> {
    instance
        .tools
        .iter()
        .filter(|tool| tool.direct)
        .map(|tool| tool.original_name.to_string())
        .collect()
}

/// 实例全部工具的 effective name（顺序 = 声明顺序）。
fn declared_effective_names(instance: &BuiltinMcpInstance) -> Vec<String> {
    instance
        .tools
        .iter()
        .map(|tool| tool.effective_name.to_string())
        .collect()
}

/// 实例**声明为 direct** 的工具 effective name（顺序 = 声明顺序）。
///
/// 与 [`declared_effective_names`] 严格区分：`cron` / `lsp` 的工具一律 `direct: false`
/// （A4/A5），因此它们的名字**不**出现在 direct 面（也不出现在 `required` 侧——`required`
/// 由 `system_mcp_tools` 派生，等价于本函数的集合）。
fn declared_direct_effective_names(instance: &BuiltinMcpInstance) -> Vec<String> {
    instance
        .tools
        .iter()
        .filter(|tool| tool.direct)
        .map(|tool| tool.effective_name.to_string())
        .collect()
}

/// 全部已实现实例的 direct 工具 effective name（升序）。
fn all_direct_effective_names() -> Vec<String> {
    let mut names: Vec<String> = BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter().filter(|tool| tool.direct))
        .map(|tool| tool.effective_name.to_string())
        .collect();
    names.sort();
    names
}

/// 实例的 builtin 配置条目（与默认层规则 1 同形：无 command / url，身份来自 `source`）。
///
/// 只用于夹具侧驱动 `list_discovered_tools` 的 System 分支（强制 live round-trip）；
/// 生产默认层的构造是 `builtin::apply_builtin_overlay`，本文件不复制它的覆盖规则。
fn builtin_entry(instance: &BuiltinMcpInstance) -> McpServerConfig {
    McpServerConfig {
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        // 必须为 None：显式版本会跳过 Auto 的 `server/discover` 探测。
        protocol_version: None,
        subscriptions: None,
        system_mcp: Some(true),
        system_mcp_tools: Some(declared_direct_original_names(instance)),
        system_mcp_timeout: None,
        source: Some(ConfigSource::Builtin {
            instance: instance.instance.to_string(),
        }),
    }
}

fn sorted(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names
}

fn tool_names(tools: &[Box<dyn BaseTool>]) -> Vec<String> {
    tools.iter().map(|tool| tool.name().to_string()).collect()
}

/// 收集视图 / 候选视图里的 direct 名字（两处形态不同：`Box<dyn BaseTool>` 与
/// `Arc<dyn BaseTool>`，因此按迭代器收口；与 `mcp::mcp_v4_seam_tests` 同形）。
fn direct_names_of<'a>(tools: impl Iterator<Item = &'a dyn BaseTool>) -> Vec<String> {
    sorted(
        tools
            .filter(|tool| tool.is_direct())
            .map(|tool| tool.name().to_string())
            .collect(),
    )
}

// ─── 替身工具 / 网络策略 ────────────────────────────────────────────────────────

/// 替身工具：**只有工具核心是替身**（见文件头「证据边界 B」）。
///
/// 计数与入参记录供两条断言使用：
/// - `call_count()`：server 侧「工具真的被执行了」的次数；
/// - `seen_inputs()`：**server 侧**看到的入参（wire 真的把参数带过去的证据）。
struct StubTool {
    name: String,
    calls: Arc<AtomicUsize>,
    inputs: Arc<Mutex<Vec<Value>>>,
    reply: Arc<dyn Fn(&Value) -> String + Send + Sync>,
}

impl StubTool {
    fn new(name: &str, reply: Arc<dyn Fn(&Value) -> String + Send + Sync>) -> Self {
        Self {
            name: name.to_string(),
            calls: Arc::new(AtomicUsize::new(0)),
            inputs: Arc::new(Mutex::new(Vec::new())),
            reply,
        }
    }

    /// 固定正文的替身。
    fn fixed(name: &str, text: &str) -> Self {
        let text = text.to_string();
        Self::new(name, Arc::new(move |_input: &Value| text.clone()))
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn seen_inputs(&self) -> Vec<Value> {
        self.inputs.lock().expect("stub inputs 未被 poison").clone()
    }
}

#[async_trait]
impl BaseTool for StubTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "builtin runtime 夹具替身工具（链路 / handler / 映射 / bridge 均为生产实现）"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let reply = (self.reply)(&input);
        self.inputs
            .lock()
            .expect("stub inputs 未被 poison")
            .push(input);
        Ok(reply)
    }
}

/// **闸门**替身工具（acceptance §7 第 5 条：builtin 工具体内在飞取消的观测点）。
///
/// `invoke` 进入即 `entered.notify_one()`——这是「server 侧已经在处理这次 `tools/call`」
/// 的观测点，取消必须发生在这个点**之后**才有「在飞」语义；随后调用停在 `release` 上，
/// 直到用例显式放行（模拟一个慢工具）。
///
/// 两个与「只等一个 `Notify`」不同的设计点，都是为了让断言**确定**而不引入时序假绿/假红：
/// - `released` 是**粘滞**的：第一次放行之后，后续调用直接通过。否则「取消后同一 pool
///   仍可服务」的第二次 `invoke` 会再次停在闸门上，观测点退化成「又一次等待」而不是
///   「服务仍然可用」。
/// - `finished` 在真正离开闸门后 `notify_one()`：用例据此确认**被弃置**的那次在飞
///   handler 已经收敛，再去发第二次请求（`Notify` 的许可可暂存，因此早到/晚到都确定）。
struct GatedTool {
    name: String,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    finished: Arc<Notify>,
    released: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}

impl GatedTool {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            finished: Arc::new(Notify::new()),
            released: Arc::new(AtomicBool::new(false)),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// server 侧「工具真的被执行了」的次数（取消**不得**让它变成 2）。
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl BaseTool for GatedTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "builtin runtime 夹具替身工具（链路 / handler / 映射 / bridge 均为生产实现）"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn invoke(
        &self,
        _input: Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        if !self.released.load(Ordering::SeqCst) {
            self.release.notified().await;
            self.released.store(true, Ordering::SeqCst);
        }
        self.finished.notify_one();
        Ok("gated".to_string())
    }
}

/// 测试用 builtin handler：与生产 handler 同形（**不覆写 `discover`**，§10 R2）。
#[derive(Clone)]
struct FixtureBuiltinHandler {
    info_name: &'static str,
    tools: Arc<Vec<Arc<dyn BaseTool>>>,
    /// server 侧观测：按到达顺序记录每次 `tools/call` 的**请求名**。
    served_calls: Arc<Mutex<Vec<String>>>,
}

impl FixtureBuiltinHandler {
    fn new(info_name: &'static str, tools: Vec<Arc<dyn BaseTool>>) -> Self {
        Self {
            info_name,
            tools: Arc::new(tools),
            served_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn served_calls(&self) -> Vec<String> {
        self.served_calls
            .lock()
            .expect("served_calls 未被 poison")
            .clone()
    }
}

/// `BaseTool::definition()` → `rmcp::model::Tool`（与生产 handler 的映射同形）。
fn rmcp_tool_of(tool: &dyn BaseTool) -> RmcpTool {
    let definition = tool.definition();
    let schema = definition
        .parameters
        .as_object()
        .cloned()
        .unwrap_or_default();
    RmcpTool::new(definition.name, definition.description, schema)
}

impl ServerHandler for FixtureBuiltinHandler {
    // 不覆写 `discover`：rmcp 默认实现走 modern（inline `server/discover`）。

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(self.info_name, "runtime-fixture"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(
            self.tools
                .iter()
                .map(|tool| rmcp_tool_of(tool.as_ref()))
                .collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        self.served_calls
            .lock()
            .expect("served_calls 未被 poison")
            .push(name.clone());
        // 按**原始名**路由（与 IF-D14 的生产映射同形）：未命中 → `invalid_params`。
        let Some(tool) = self
            .tools
            .iter()
            .find(|tool| tool.name() == request.name.as_ref())
        else {
            return Err(McpError::invalid_params(
                format!("unknown tool: {name}"),
                None,
            ));
        };
        let input = Value::Object(request.arguments.clone().unwrap_or_default());
        match tool.invoke(input, ToolContext::new(&[], "")).await {
            Ok(text) => Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                ContentBlock::text(text),
            ]))),
            Err(_error) => Ok(CallToolResponse::Complete(CallToolResult::error(vec![
                ContentBlock::text(format!("tool `{name}` failed to execute")),
            ]))),
        }
    }
}

// ─── 审批 broker / 启动闸门探针 ─────────────────────────────────────────────────

/// 记录型审批 broker：记录每次审批请求的**工具名与入参**，按构造时的决定回应。
///
/// 只记录工具名与入参（不含 env / headers / 凭据；断言消息也不打印它们）。
struct RecordingBroker {
    approve: bool,
    requests: AtomicUsize,
    seen: Mutex<Vec<(String, Value)>>,
}

impl RecordingBroker {
    fn approving() -> Self {
        Self {
            approve: true,
            requests: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn rejecting() -> Self {
        Self {
            approve: false,
            requests: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn seen(&self) -> Vec<(String, Value)> {
        self.seen.lock().expect("broker seen 未被 poison").clone()
    }
}

#[async_trait]
impl UserInteractionBroker for RecordingBroker {
    async fn request(&self, context: InteractionContext) -> InteractionResponse {
        self.requests.fetch_add(1, Ordering::SeqCst);
        match context {
            InteractionContext::Approval { items } => {
                {
                    let mut seen = self.seen.lock().expect("broker seen 未被 poison");
                    seen.extend(
                        items
                            .iter()
                            .map(|item| (item.tool_name.clone(), item.tool_input.clone())),
                    );
                }
                InteractionResponse::Decisions(
                    items
                        .iter()
                        .map(|_| {
                            if self.approve {
                                ApprovalDecision::Approve { source: None }
                            } else {
                                ApprovalDecision::Reject {
                                    reason: "用户拒绝".to_string(),
                                    source: None,
                                }
                            }
                        })
                        .collect(),
                )
            }
            _ => InteractionResponse::Decisions(Vec::new()),
        }
    }
}

/// 启动闸门探针：候选只经 `StartupState` 传递（与 `mcp::mcp_v4_seam_tests` 同形），
/// 不读 middleware 内部字段。
#[derive(Default)]
struct StartupProbe {
    staged: Option<StartupToolUpdate>,
    stage_calls: usize,
}

impl hook_state::StartupState for StartupProbe {
    fn set_active_middleware(&mut self, _middleware_name: &str) {}

    fn stage_startup_tools(&mut self, update: StartupToolUpdate) -> AgentResult<()> {
        self.stage_calls += 1;
        if self.staged.is_none() {
            self.staged = Some(update);
        }
        Ok(())
    }

    fn take_startup_tools(&mut self) -> Option<StartupToolUpdate> {
        self.staged.take()
    }
}

// ─── 夹具 A：真实启动路径 ───────────────────────────────────────────────────────

/// 生产启动路径夹具：`run_initialize`（生产 loader 含 step 6.5 默认层）+ 真实连接。
///
/// `HOME` 被重定向到空临时目录，`PERI_MCP_BUILTIN` 置空 ⇒ 唯一注入的 server 是**四个**
/// builtin 实例（web / artifact / cron / lsp）；断言不依赖运行者本机配置。
struct StartupFixture {
    _fixture: tempfile::TempDir,
    _env: LoaderEnvGuard,
    pool: Arc<McpClientPool>,
    project: PathBuf,
    /// cron 触发通道的接收端：夹具持有它（与宿主同形），使 `CronScheduler` 的发送端
    /// 始终有对端。本夹具 `tick_enabled = false`，因此不会有 tick 真的送出触发。
    _cron_triggers: tokio::sync::mpsc::UnboundedReceiver<peri_acp_types::cron::CronTrigger>,
}

/// 夹具的 lsp 输入：**生效配置非空**（惰性池：`LspServerPool::new` 不拉任何 language
/// server 进程），使 `lsp` 实例的工具面按 `has_servers()` 快照出注册表声明的 `LSP`。
///
/// 空配置（`LspConfigFile::default()`）会让 `lsp` 面为空表——那是 L-01 的「可见但空」
/// 语义，由 `mcp::builtin::lsp` 的用例覆盖；本文件的启动路径断言是「live `tools/list`
/// 等于注册表声明」（逐实例），因此夹具给的是有配置形态。
fn fixture_lsp_pool(cwd: &str) -> Arc<LspServerPool> {
    let config = LspConfigFile {
        lsp_servers: std::collections::HashMap::from([(
            "fixture-lsp".to_string(),
            LspServerConfig {
                name: "fixture-lsp".to_string(),
                // 惰性池不会 spawn 该命令：本文件不调用任何 LSP 工具。
                command: "fixture-lsp-unused".to_string(),
                args: Vec::new(),
                env: None,
                extension_to_language: std::collections::HashMap::new(),
                initialization_options: None,
                disabled: None,
                max_restarts: None,
                startup_timeout: None,
                source: None,
            },
        )]),
    };
    Arc::new(LspServerPool::new(cwd, config))
}

impl StartupFixture {
    async fn start() -> Self {
        let fixture = tempfile::tempdir().expect("tempdir");
        let home = fixture.path().join("home");
        let project = fixture.path().join("project");
        std::fs::create_dir_all(&home).expect("home 可创建");
        std::fs::create_dir_all(&project).expect("project 可创建");
        let env = LoaderEnvGuard::redirect(&home);
        let cwd = project.to_string_lossy().to_string();

        let pool = Arc::new(McpClientPool::new_pending());
        // A33：实例上下文必须由宿主装配在 `run_initialize` **之前**注入（本夹具扮演宿主）。
        // 四个已实现实例的输入一次给齐：`web` / `artifact` 不需要额外输入（后者的解析根是
        // `cwd`），`cron` 要 scheduler（`tick_enabled = false`：本夹具不起 tick，tick 归属
        // 由 `mcp::builtin::runtime` / `cron` 的驱动用例与
        // `pool_spawn_point_drives_cron_tick_and_stops_on_generation_close` 覆盖），`lsp`
        // 要 pool（见 `fixture_lsp_pool`）。
        let (cron_trigger_tx, cron_triggers) = tokio::sync::mpsc::unbounded_channel();
        let scheduler = Arc::new(parking_lot::Mutex::new(CronScheduler::new(cron_trigger_tx)));
        pool.set_builtin_instance_context(Arc::new(
            BuiltinInstanceContext::new(cwd.clone())
                .with_cron(CronInstanceInput {
                    scheduler,
                    tick_enabled: false,
                })
                .with_lsp(LspInstanceInput {
                    pool: fixture_lsp_pool(&cwd),
                }),
        ))
        .expect("夹具首次注入上下文必须成功");
        let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        McpClientPool::run_initialize(pool.clone(), &project, &home, status_tx, None, None).await;
        Self {
            _fixture: fixture,
            _env: env,
            pool,
            project,
            _cron_triggers: cron_triggers,
        }
    }

    /// 夹具收尾：pool 关闭后 builtin task 表必须已排空（无 orphan）。
    async fn shutdown(self) {
        self.pool.begin_shutdown();
        let report = self.pool.shutdown().await;
        assert!(report.is_complete(), "pool 关闭必须收敛: {report:?}");
        assert_eq!(
            self.pool.builtin_task_count(),
            0,
            "pool 关闭后不得残留 builtin server task"
        );
    }
}

// ─── 夹具 B：线路观测（per-instance wire，A13 ④）─────────────────────────────────

/// 一条经**生产** `serve_client_auto` 握手的 builtin 链路 + 线路观测 + task 归属。
///
/// server 半边是真实 `rmcp::serve_server`（handler 为测试替身，见文件头「证据边界 B」）；
/// client 半边是生产 `serve_client_auto`（Auto lifecycle）。
///
/// 归属口径：句柄写进 `pool.clients`（`build_typed_tool_bridges` 的唯一输入）；
/// client service 与 server task **默认都由夹具持有**（不登记 pool 的 task 表），以便显式
/// 断言「关闭 client → server task 靠 EOF 自然收敛（`Quit`，不是 abort）」。
/// [`Self::hand_service_to_pool`] 可把 client 半边**移交**给 pool 的生产表（`services`），
/// 使 `pool.reconnect` / `pool.shutdown` 真的会关闭它——「重连只动被点名实例」由此获得
/// 失败模式（见同 pool 用例）。
/// 生产归属（task 登记进 pool、随 pool 关闭排空）由 [`StartupFixture::shutdown`] 与
/// `mcp::builtin::runtime` 覆盖。
struct TappedLink {
    instance: &'static BuiltinMcpInstance,
    pool: Arc<McpClientPool>,
    /// 夹具自持的 client 半边；[`Self::hand_service_to_pool`] 之后为 `None`。
    service: Option<McpServiceWrapper>,
    server_task: BuiltinServerTask,
    wire: Arc<BuiltinWireLog>,
    handler: FixtureBuiltinHandler,
}

impl TappedLink {
    /// 单实例链路：自建**独立** pool（既有用例口径逐位不变；本函数只是
    /// [`Self::connect_into`] 的薄包装）。
    async fn connect(
        instance: &'static BuiltinMcpInstance,
        info_name: &'static str,
        tools: Vec<Arc<dyn BaseTool>>,
    ) -> Self {
        Self::connect_into(
            Arc::new(McpClientPool::new_empty()),
            instance,
            info_name,
            tools,
        )
        .await
    }

    /// 把链路建进**调用方给定的** pool（同一容器内的多实例观测面：A13 ④/⑤ 的
    /// 「同 pool 不串」与「重连只动被点名实例」需要它）。
    ///
    /// 步骤与 [`Self::connect`] 原口径逐字相同（真实握手 → 配置侧 builtin 条目 →
    /// live `tools/list` → 句柄写进 `pool.clients`），唯一差别是 pool 由参数注入：
    /// 调用方与返回的链路共享同一个 pool 实例。
    async fn connect_into(
        pool: Arc<McpClientPool>,
        instance: &'static BuiltinMcpInstance,
        info_name: &'static str,
        tools: Vec<Arc<dyn BaseTool>>,
    ) -> Self {
        let handler = FixtureBuiltinHandler::new(info_name, tools);
        let (transport, wire) =
            spawn_builtin_transport_with_tap(instance.instance, handler.clone());
        // 夹具自持 server task（不登记进 pool）：只取 io 与 server 半边，tick 归属
        // （生产恒 `None`）与本次夹具无关。
        let crate::mcp::builtin::runtime::BuiltinTransport {
            io, server_task, ..
        } = transport;
        let service =
            serve_client_auto(io, None, None, &pool.capability_profile, HANDSHAKE_TIMEOUT)
                .await
                .expect("builtin 握手不得超时（同进程链路）")
                .expect("builtin 握手不得失败");

        // 配置侧：与默认层同形的 builtin 条目（`system_mcp = true` ⇒ live round-trip）。
        let config = builtin_entry(instance);
        pool.configs
            .write()
            .insert(instance.name.to_string(), config.clone());
        let peer = service.peer().clone();
        let discovered = list_discovered_tools(&pool, instance.name, &peer, &config)
            .await
            .expect("live tools/list 必须成功");
        let handle = Arc::new(McpClientHandle {
            name: instance.name.to_string(),
            version: None,
            cache_version: None,
            peer: Some(peer),
            tools: discovered,
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: Default::default(),
            source: Some(ConfigSource::Builtin {
                instance: instance.instance.to_string(),
            }),
            url: None,
            skills_capable: false,
            channel_capable: false,
        });
        pool.clients
            .write()
            .insert(instance.name.to_string(), handle);
        Self {
            instance,
            pool,
            service: Some(service),
            server_task,
            wire,
            handler,
        }
    }

    /// 把 client 半边移交 pool 的**生产表**（`services`）：此后它的关闭只由 pool 的动作
    /// 驱动（`reconnect` 关被点名实例的旧 service、`shutdown` 关全部）。
    ///
    /// 这一步是「重连只动被点名实例」可证伪的前提：未移交时 `reconnect` 的「关旧
    /// service」对夹具链路是**空操作**，artifact 侧的「逐字不变」在任何实现下都成立。
    fn hand_service_to_pool(&mut self) {
        let service = self.service.take().expect("client service 只能移交一次");
        let previous = self
            .pool
            .services
            .lock()
            .insert(self.instance.name.to_string(), service);
        assert!(
            previous.is_none(),
            "pool 的 services 表内不得已有同名 client service"
        );
    }

    /// 收敛**夹具自持**的 server task，返回退出事实。
    ///
    /// 在 [`Self::hand_service_to_pool`] 之后，收敛的触发者是 **pool 的动作**
    /// （`reconnect` / `shutdown` 关闭 client 半边 ⇒ 本 task 收到 EOF）；调用方据此把
    /// `Quit` 归因到 pool 的那次动作，而不是夹具自己的收尾。
    async fn converge_task(&mut self) -> BuiltinServerExit {
        self.server_task.converge(BUILTIN_CONVERGE_TIMEOUT).await
    }

    /// 线路级 method 序列（server 读半观测）。
    fn wire_methods(&self) -> Vec<String> {
        self.wire.methods()
    }

    /// 线路上 `tools/call` 请求的条数（审批 approve/reject 两态的计数口径）。
    fn wire_call_tool_count(&self) -> usize {
        self.wire_methods()
            .iter()
            .filter(|method| method.as_str() == "tools/call")
            .count()
    }

    /// server handler 实际收到并路由过的 `tools/call` 请求名（按到达顺序）。
    fn served_calls(&self) -> Vec<String> {
        self.handler.served_calls()
    }

    /// 本链路所在 pool 的类型化 bridge（生产构造：声明 direct 生效）。
    fn bridge(&self, effective_name: &str) -> McpToolBridge {
        build_typed_tool_bridges(&self.pool)
            .into_iter()
            .find(|bridge| bridge.name() == effective_name)
            .unwrap_or_else(|| panic!("必须存在 {effective_name} 的 typed bridge"))
    }

    /// peer（用于「effective name 不是 wire 名」的反证）。
    fn peer(&self) -> rmcp::service::Peer<rmcp::service::RoleClient> {
        self.pool
            .get_client(self.instance.name)
            .and_then(|handle| handle.peer.clone())
            .expect("夹具链路必须有 peer")
    }

    /// 夹具收尾：关闭 client → server task 必须靠 EOF 自然收敛（无 orphan）。
    ///
    /// 只适用于**夹具自持** client 半边的链路（未调用 [`Self::hand_service_to_pool`]）；
    /// 已移交的链路由其新归属者（pool）关闭，收敛断言改用 [`Self::converge_task`]。
    async fn shutdown(mut self) {
        let mut service = self
            .service
            .take()
            .expect("夹具自持形态才可用 shutdown；已移交 pool 的链路请用 converge_task");
        let _ = service.close_with_timeout(CLOSE_TIMEOUT).await;
        let exit = self.converge_task().await;
        assert!(
            matches!(exit, BuiltinServerExit::Quit(_)),
            "正常关闭必须靠 EOF 自然收敛，实际: {exit:?}"
        );
        assert!(
            self.server_task.is_finished(),
            "收敛后 server task 必须已结束"
        );
    }
}

/// 模型侧工具调用（name = effective name，input = 模型给出的参数）。
fn tool_call(name: &str, input: Value) -> ToolCall {
    ToolCall {
        id: format!("call-{}", name),
        name: name.to_string(),
        input,
    }
}

/// 真实审批链：`PermissionMiddleware`（生产判定函数）+ 注入 broker → 批准/拒绝结果。
async fn run_approval_chain(
    middleware: &PermissionMiddleware,
    call: &ToolCall,
) -> Vec<AgentResult<ToolCall>> {
    middleware.process_batch(std::slice::from_ref(call)).await
}

// ══════════════════════════════════════════════════════════════════════════════════

#[path = "builtin_runtime_collection_test.rs"]
mod collection;

#[path = "builtin_runtime_startup_test.rs"]
mod startup;

#[path = "builtin_runtime_wire_test.rs"]
mod wire;

#[path = "builtin_runtime_cancel_test.rs"]
mod cancel;

#[path = "builtin_runtime_wave3_test.rs"]
mod wave3;
