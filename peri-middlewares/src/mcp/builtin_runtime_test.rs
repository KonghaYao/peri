//! builtin 实例的**运行时 / 关闭面** crate 内验收。
//!
//! owner 传递：W3 由 I-02 建挂载点并落地关闭面（IF-D10 面②：目录收集路径）断言；
//! W4 由 **V-01** 在本文件内扩展为：启动路径（modern 握手 + `tools/list` 提交）、
//! 审批 approve/reject 双断言、wire 计数、关闭矩阵四面、无 orphan task、
//! 大 payload（A16）、隔离可观察四项（A13）。
//!
//! 本文件断言只落在**可观察能力面**（`Middleware::collect_tools` 产出，即链工具集合），
//! 不依赖中间量；测试只用注入的本地假 handle / 假 token，不含真实凭据，也不打印 env。

use std::sync::Arc;

use peri_agent::middleware::r#trait::Middleware;

use crate::mcp::builtin::closed_instances;
use crate::mcp::{ClientStatus, McpClientHandle, McpClientPool, McpMiddleware, Tool};

/// 构造一个「已连接」的假 MCP client（工具名按传入列表）。
fn connected_handle(server: &str, tools: &[&str]) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: server.to_string(),
        version: None,
        cache_version: None,
        peer: None,
        tools: tools.iter().map(|tool| make_tool(tool)).collect(),
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: Default::default(),
        source: None,
        url: None,
        skills_capable: false,
        channel_capable: false,
    })
}

fn make_tool(name: &str) -> Tool {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "description": "builtin tool",
        "inputSchema": { "type": "object", "properties": {} }
    }))
    .unwrap()
}

/// 一个含两个 builtin 实例（web / artifact）+ 一个外部 server 的 deployment pool。
///
/// 工具清单与 builtin 注册表一致：`web` 提供 WebSearch / WebFetch，`artifact` 提供
/// artifact；外部 server 提供一个 deferred 工具，用于验证关闭过滤**只**作用于
/// builtin 实例。
fn pool_with_builtin_instances() -> Arc<McpClientPool> {
    let pool = Arc::new(McpClientPool::new_empty());
    pool.clients.write().insert(
        "web".to_string(),
        connected_handle("web", &["WebSearch", "WebFetch"]),
    );
    pool.clients.write().insert(
        "artifact".to_string(),
        connected_handle("artifact", &["artifact"]),
    );
    pool.clients.write().insert(
        "external".to_string(),
        connected_handle("external", &["Read"]),
    );
    pool
}

/// 链工具集合（可观察能力面）：`McpMiddleware::collect_tools` 的产出。
fn collected_tool_names(pool: &Arc<McpClientPool>, disabled: &[&str]) -> Vec<String> {
    let disabled: std::collections::HashSet<String> =
        disabled.iter().map(|name| name.to_string()).collect();
    let middleware = McpMiddleware::new(Arc::clone(pool))
        .with_tool_pool(Arc::clone(pool))
        .with_builtin_closures(closed_instances(&disabled));
    middleware
        .collect_tools("/tmp/contract-test")
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect()
}

/// 未关闭任何实例：三个 builtin 工具都在目录里（web 两个 + artifact 一个），
/// 且声明的 direct 生效（IF-D13）；外部 server 的工具照旧 deferred 存在。
#[test]
fn builtin_tools_are_collected_with_declared_direct() {
    let pool = pool_with_builtin_instances();
    let disabled: std::collections::HashSet<String> = std::collections::HashSet::new();
    let middleware = McpMiddleware::new(Arc::clone(&pool))
        .with_tool_pool(Arc::clone(&pool))
        .with_builtin_closures(closed_instances(&disabled));
    let tools = middleware.collect_tools("/tmp/contract-test");

    let direct_of = |name: &str| -> Option<bool> {
        tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(|tool| tool.is_direct())
    };
    for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
        for declaration in instance.tools {
            assert_eq!(
                direct_of(declaration.effective_name),
                Some(declaration.direct),
                "{} 的 direct 必须等于注册表声明（IF-D13）",
                declaration.effective_name
            );
        }
    }
    assert_eq!(
        direct_of("mcp__external__Read"),
        Some(false),
        "外部 server 的工具保持 deferred（类型化构造只对注册表声明的 builtin 提升）"
    );
}

/// IF-D10 面②：关闭实例的 bridge 不得进入目录；未关闭实例与外部 server 不受影响。
#[test]
fn closed_builtin_instance_disappears_from_collected_tools() {
    // 关闭 web：两个 web 工具消失，artifact 与外部 server 保留。
    let web_closed = collected_tool_names(&pool_with_builtin_instances(), &["WebMiddleware"]);
    assert!(
        !web_closed.iter().any(|name| name.contains("mcp__web__")),
        "关闭 WebMiddleware 后 web 实例的工具必须归零: {web_closed:?}"
    );
    assert!(
        web_closed
            .iter()
            .any(|name| name == "mcp__artifact__artifact"),
        "另一个实例不受影响: {web_closed:?}"
    );
    assert!(
        web_closed.iter().any(|name| name == "mcp__external__Read"),
        "外部 server 不受关闭影响: {web_closed:?}"
    );

    // 关闭 artifact：只有 artifact 消失。
    let artifact_closed =
        collected_tool_names(&pool_with_builtin_instances(), &["ArtifactMiddleware"]);
    assert!(artifact_closed
        .iter()
        .any(|name| name == "mcp__web__WebSearch"));
    assert!(
        !artifact_closed
            .iter()
            .any(|name| name.contains("mcp__artifact__")),
        "关闭 ArtifactMiddleware 后 artifact 必须归零: {artifact_closed:?}"
    );

    // 两者都关闭：builtin 工具一个不剩。
    let both_closed = collected_tool_names(
        &pool_with_builtin_instances(),
        &["WebMiddleware", "ArtifactMiddleware"],
    );
    assert!(
        !both_closed
            .iter()
            .any(|name| name.starts_with("mcp__web__") || name.starts_with("mcp__artifact__")),
        "两个实例都关闭后 builtin 工具必须归零: {both_closed:?}"
    );
    assert!(both_closed.iter().any(|name| name == "mcp__external__Read"));
}

/// 关闭语义必须**只**由注册表 `policy_key` 驱动：未知键不起作用（也不 panic）。
#[test]
fn unknown_policy_key_does_not_close_any_instance() {
    let tools = collected_tool_names(&pool_with_builtin_instances(), &["NotABuiltinPolicyKey"]);
    assert!(tools.iter().any(|name| name == "mcp__web__WebFetch"));
    assert!(tools.iter().any(|name| name == "mcp__artifact__artifact"));
}

/// 空关闭集与不注入关闭集等价（既有调用点语义逐位不变）。
#[test]
fn empty_closures_keep_every_builtin_tool() {
    let pool = pool_with_builtin_instances();
    let with_empty_closures = collected_tool_names(&pool, &[]);
    let middleware = McpMiddleware::new(Arc::clone(&pool)).with_tool_pool(Arc::clone(&pool));
    let without_closures: Vec<String> = middleware
        .collect_tools("/tmp/contract-test")
        .into_iter()
        .map(|tool| tool.name().to_string())
        .collect();
    assert_eq!(with_empty_closures, without_closures);
}

// ══════════════════════════════════════════════════════════════════════════════════
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
// 生产同构，**替身只替换工具核心**（Web 两个工具的后端地址是编译期常量，单测无法指向
// 本地桩）。生产 handler 的协议行为由 `mcp::builtin::web` / `mcp::builtin::artifact` /
// `mcp::builtin::runtime` 覆盖。
//
// **不覆盖（UNVERIFIED，A13 强制项）**：capability root 隔离与凭据隔离在 builtin 形态下
// **不可证伪**——`capability_profile` 是 pool 级字段（`mcp/apps.rs`，构造点
// `McpClientPool::new_pending_with_spawner_and_profile`）、`bind_execution_cwd` 也是
// pool 级（`mcp/initialize.rs`），`McpClientHandle` 无凭据字段。本文件**不**用「两个实例
// 构造参数不同」「两个 handler 是不同 struct」这类代码阅读结论替代运行时证据。
// 同样未覆盖：真实网络调用（Web 两个工具与 artifact 真实上传都依赖外部服务）。
// ══════════════════════════════════════════════════════════════════════════════════

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::builtin_mcp::{find, BuiltinMcpInstance, BUILTIN_MCP_INSTANCES};
use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use peri_agent::agent::react::ToolCall;
use peri_agent::error::{AgentError, AgentResult};
use peri_agent::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, UserInteractionBroker,
};
use peri_agent::middleware::capabilities as hook_state;
use peri_agent::session::tool_catalog::{StartupRequiredTool, StartupToolUpdate};
use peri_agent::tools::{BaseTool, ToolContext};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode,
    Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo,
    Tool as RmcpTool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde_json::{json, Value};

use crate::assembly::{default_workflow_middleware_factory_with_pool, open_builtin_bridges};
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_tap, BuiltinServerExit, BuiltinServerTask, BuiltinWireLog,
    BUILTIN_CONVERGE_TIMEOUT,
};
use crate::mcp::builtin::BUILTIN_INJECTION_ENV;
use crate::mcp::client::{serve_client_auto, McpConnectionKey, McpInitStatus, McpServiceWrapper};
use crate::mcp::initialize::list_discovered_tools;
use crate::mcp::tool_bridge::{build_typed_tool_bridges, McpToolBridge};
use crate::permission::{default_requires_approval, PermissionMiddleware};

/// 握手/关闭上界：同进程链路，取值远大于实测（spike 与 `runtime_test` 同口径）。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(1000);
/// 大 payload 正文（A16）：单行、~300 KiB，远大于 `BUILTIN_DUPLEX_BUF`（8 KiB）。
/// 单行是刻意的：`McpToolBridge::invoke` 的**行数**截断（`MAX_MCP_LINES`）不在本用例
/// 的观测面内，本用例只问「远大于 duplex 容量的一帧能否完整往返」。
const LARGE_BODY_BYTES: usize = 300 * 1024;
/// 大请求帧（反向）：参数里的字符串长度。
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
    _lock: crate::process_env::EnvLockFile,
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl LoaderEnvGuard {
    fn redirect(home: &Path) -> Self {
        let lock = crate::process_env::lock().expect("进程环境锁");
        let home = home.to_string_lossy().into_owned();
        let mut previous = Vec::new();
        for (key, value) in [
            ("HOME", Some(home.clone())),
            ("USERPROFILE", Some(home)),
            (BUILTIN_INJECTION_ENV, None),
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
/// `HOME` 被重定向到空临时目录，`PERI_MCP_BUILTIN` 置空 ⇒ 唯一注入的 server 是两个
/// builtin 实例；断言不依赖运行者本机配置。
struct StartupFixture {
    _fixture: tempfile::TempDir,
    _env: LoaderEnvGuard,
    pool: Arc<McpClientPool>,
    project: PathBuf,
}

impl StartupFixture {
    async fn start() -> Self {
        let fixture = tempfile::tempdir().expect("tempdir");
        let home = fixture.path().join("home");
        let project = fixture.path().join("project");
        std::fs::create_dir_all(&home).expect("home 可创建");
        std::fs::create_dir_all(&project).expect("project 可创建");
        let env = LoaderEnvGuard::redirect(&home);

        let pool = Arc::new(McpClientPool::new_pending());
        let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        McpClientPool::run_initialize(pool.clone(), &project, &home, status_tx, None, None).await;
        Self {
            _fixture: fixture,
            _env: env,
            pool,
            project,
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
/// 归属口径：句柄写进 `pool.clients`（`build_typed_tool_bridges` 的唯一输入），但
/// client service 与 server task **都由夹具持有**（不登记 pool 的 task 表），以便显式
/// 断言「关闭 client → server task 靠 EOF 自然收敛（`Quit`，不是 abort）」。
/// 生产归属（task 登记进 pool、随 pool 关闭排空）由 [`StartupFixture::shutdown`] 与
/// `mcp::builtin::runtime` 覆盖。
struct TappedLink {
    instance: &'static BuiltinMcpInstance,
    pool: Arc<McpClientPool>,
    service: McpServiceWrapper,
    server_task: BuiltinServerTask,
    wire: Arc<BuiltinWireLog>,
    handler: FixtureBuiltinHandler,
}

impl TappedLink {
    async fn connect(
        instance: &'static BuiltinMcpInstance,
        info_name: &'static str,
        tools: Vec<Arc<dyn BaseTool>>,
    ) -> Self {
        let pool = Arc::new(McpClientPool::new_empty());
        let handler = FixtureBuiltinHandler::new(info_name, tools);
        let (transport, wire) =
            spawn_builtin_transport_with_tap(instance.instance, handler.clone());
        let crate::mcp::builtin::runtime::BuiltinTransport { io, server_task } = transport;
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
            service,
            server_task,
            wire,
            handler,
        }
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
    async fn shutdown(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        let mut task = self.server_task;
        let exit = task.converge(BUILTIN_CONVERGE_TIMEOUT).await;
        assert!(
            matches!(exit, BuiltinServerExit::Quit(_)),
            "正常关闭必须靠 EOF 自然收敛（spike Q2 证据），实际: {exit:?}"
        );
        assert!(task.is_finished(), "收敛后 server task 必须已结束");
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
// A. 真实启动路径（生产 loader + 生产 transport + 生产 handler）
// ══════════════════════════════════════════════════════════════════════════════════

/// 生产启动路径：两个 builtin 实例经真实 loader（step 6.5 默认层）落地为 **Connected**
/// 句柄，live `tools/list` 的工具清单等于注册表声明，且配置侧声明与注册表一致。
#[tokio::test]
async fn production_startup_path_connects_both_builtin_instances() {
    let fixture = StartupFixture::start().await;
    let pool = Arc::clone(&fixture.pool);

    assert_eq!(
        pool.builtin_task_count(),
        BUILTIN_MCP_INSTANCES.len(),
        "每个 builtin 实例必须登记一条 server task（关闭时有归属）"
    );
    for instance in BUILTIN_MCP_INSTANCES {
        let handle = pool
            .get_client(instance.name)
            .unwrap_or_else(|| panic!("{} 必须完成连接", instance.name));
        assert!(
            matches!(handle.status, ClientStatus::Connected),
            "{} 必须经同一条处理链提交 Connected，实际: {:?}",
            instance.name,
            handle.status
        );
        let peer_info = handle
            .peer
            .as_ref()
            .and_then(|peer| peer.peer_info())
            .unwrap_or_else(|| panic!("{} 的 modern 握手必须留下 peer_info", instance.name));
        assert!(
            peer_info.server_info.is_some(),
            "{} 必须协商出 server_info（真实 handler 的 get_info）",
            instance.name
        );

        let declared: Vec<&str> = instance
            .tools
            .iter()
            .map(|tool| tool.original_name)
            .collect();
        let served: Vec<&str> = handle.tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert_eq!(
            served, declared,
            "{} 的 live tools/list 必须按声明顺序返回注册表工具",
            instance.name
        );
        assert!(
            matches!(handle.source, Some(ConfigSource::Builtin { .. })),
            "{} 的句柄必须保留 builtin 身份",
            instance.name
        );
        assert!(handle.url.is_none(), "builtin 实例无 URL / 无凭据");

        let config = pool
            .configs
            .read()
            .get(instance.name)
            .cloned()
            .unwrap_or_else(|| panic!("{} 必须写入 pool.configs", instance.name));
        assert_eq!(
            config.system_mcp,
            Some(true),
            "{} 必须是 system 依赖（IF-D9）",
            instance.name
        );
        assert_eq!(
            config.system_mcp_tools.as_deref(),
            Some(declared_direct_original_names(instance).as_slice()),
            "{} 的 system_mcp_tools 必须等于声明 direct 集合（A5/A17）",
            instance.name
        );
        assert!(
            pool.discovery_evidence(instance.name)
                .is_some_and(|evidence| evidence.is_complete()),
            "{} 必须提交可核对的完整发现证据",
            instance.name
        );
    }
    assert!(
        matches!(*pool.init_status.read(), McpInitStatus::Ready { total } if total == 2),
        "收口必须是 Ready{{total:2}}，实际: {:?}",
        pool.init_status.read()
    );

    fixture.shutdown().await;
}

/// `transport_type` 三分类（IF-D11 / sub-plan H §6.4）：两个 builtin 实例在面板投影里
/// 都必须报 `"builtin"`（不是 stdio，也不是 http）。
#[tokio::test]
async fn builtin_instances_report_builtin_transport_type() {
    let fixture = StartupFixture::start().await;
    let infos = fixture.pool.all_server_infos();

    for instance in BUILTIN_MCP_INSTANCES {
        let info = infos
            .iter()
            .find(|info| info.name == instance.name)
            .unwrap_or_else(|| panic!("面板投影必须含 {}", instance.name));
        assert_eq!(
            info.transport_type, "builtin",
            "{} 的 transport_type 必须是 builtin 分类，实际: {}",
            instance.name, info.transport_type
        );
    }

    fixture.shutdown().await;
}

/// 关闭矩阵**面①**（启动提交的必需工具选择）：真实 ready 的 pool 上，闸门候选的
/// required 与 direct 集合都必须等于注册表声明（逐实例逐工具，含双层身份）。
#[tokio::test]
async fn startup_gate_stages_declared_direct_tools_and_required_set() {
    let fixture = StartupFixture::start().await;
    let middleware = McpMiddleware::new(Arc::clone(&fixture.pool));

    let mut probe = StartupProbe::default();
    Middleware::before_react_start(&middleware, &mut probe)
        .await
        .expect("两个 builtin 实例 ready 后闸门必须放行");
    assert_eq!(probe.stage_calls, 1, "一次准入只提交一个候选");
    let update = probe.staged.expect("System 依赖就绪必须提交候选");

    let expected_required: Vec<(String, String, String)> = BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| {
            instance
                .tools
                .iter()
                .filter(|tool| tool.direct)
                .map(move |tool| {
                    (
                        instance.name.to_string(),
                        tool.original_name.to_string(),
                        tool.effective_name.to_string(),
                    )
                })
        })
        .collect();
    let mut actual_required: Vec<(String, String, String)> = update
        .required
        .iter()
        .map(|required: &StartupRequiredTool| {
            (
                required.server_name.clone(),
                required.original_tool_name.clone(),
                required.effective_tool_name.clone(),
            )
        })
        .collect();
    actual_required.sort();
    let mut expected_required_sorted = expected_required;
    expected_required_sorted.sort();
    assert_eq!(
        actual_required, expected_required_sorted,
        "候选的必需工具身份必须逐项等于注册表声明（原始名与 effective name 双层）"
    );
    assert_eq!(
        direct_names_of(update.tools.iter().map(|tool| tool.as_ref())),
        all_direct_effective_names(),
        "候选内 direct 集合必须等于注册表声明的 direct 工具集合"
    );

    fixture.shutdown().await;
}

/// 关闭矩阵（IF-D10 面①/②/③/④）：`WebMiddleware` / `ArtifactMiddleware` / 两者 /
/// `McpMiddleware` 四种输入下，四个可观察面同时变化。
///
/// 面① = 闸门候选的 required 与 direct 集合；面② = `McpMiddleware::collect_tools`；
/// 面③ = `open_builtin_bridges`（`assembly/preparation.rs` 的 `parent_tools` 与
/// `assembly/workflow.rs` 的 `builtin_tools` 调用的是**同一个** helper）；面④ =
/// 生产 workflow 工厂（`default_workflow_middleware_factory_with_pool`）的 `build_tools`。
///
/// `McpMiddleware=false` 时主链不构造 MCP 槽位：该输入的面①②③属链级，由
/// `assembly::tests` 的同一矩阵覆盖（主 plan §8 第 11 行的命令清单）；本文件断言它
/// 在**本层**可见的两件事——关闭键不进入 builtin 关闭集、workflow 面彻底没有 MCP 工具。
#[tokio::test]
async fn closure_matrix_four_faces_on_real_builtin_pool() {
    let fixture = StartupFixture::start().await;
    let pool = Arc::clone(&fixture.pool);
    let cwd = fixture.project.to_string_lossy().into_owned();

    let bare_names: Vec<&'static str> = BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter().map(|tool| tool.original_name))
        .collect();

    let cases: Vec<Vec<String>> = vec![
        vec!["WebMiddleware".to_string()],
        vec!["ArtifactMiddleware".to_string()],
        vec![
            "WebMiddleware".to_string(),
            "ArtifactMiddleware".to_string(),
        ],
    ];
    for disabled_list in cases {
        let disabled: HashSet<String> = disabled_list.iter().cloned().collect();
        let closed = closed_instances(&disabled);
        assert!(
            !closed.is_empty(),
            "合法策略键必须映射到至少一个关闭实例: {disabled_list:?}"
        );

        let open_instances: Vec<&BuiltinMcpInstance> = BUILTIN_MCP_INSTANCES
            .iter()
            .filter(|instance| !closed.contains(instance.name))
            .collect();
        let expected_open_direct: Vec<String> = sorted(
            open_instances
                .iter()
                .flat_map(|instance| declared_effective_names(instance))
                .collect(),
        );

        // 面①：闸门候选。
        let middleware = McpMiddleware::new(Arc::clone(&pool))
            .with_tool_pool(Arc::clone(&pool))
            .with_builtin_closures(closed.clone());
        let mut probe = StartupProbe::default();
        Middleware::before_react_start(&middleware, &mut probe)
            .await
            .unwrap_or_else(|error| {
                panic!("[{disabled_list:?}] 关闭实例后闸门仍必须放行: {error}")
            });
        let update = probe.staged.expect("仍有 System 依赖时必须提交候选");
        let mut staged_servers: Vec<String> = update
            .required
            .iter()
            .map(|required| required.server_name.clone())
            .collect();
        staged_servers.sort();
        staged_servers.dedup();
        let mut expected_servers: Vec<String> = open_instances
            .iter()
            .map(|instance| instance.name.to_string())
            .collect();
        expected_servers.sort();
        assert_eq!(
            staged_servers, expected_servers,
            "[{disabled_list:?}] 面① required 只允许未关闭实例"
        );
        assert_eq!(
            direct_names_of(update.tools.iter().map(|tool| tool.as_ref())),
            expected_open_direct,
            "[{disabled_list:?}] 面① direct 集合必须随关闭集收缩"
        );

        // 面②：deferred 目录（链工具集合）。
        let mut collected = tool_names(&middleware.collect_tools(&cwd));
        collected.retain(|name| !name.starts_with("mcp_read_resource") && name != "DiscoverMCP");
        for instance in BUILTIN_MCP_INSTANCES {
            for tool in declared_effective_names(instance) {
                assert_eq!(
                    collected.contains(&tool.to_string()),
                    !closed.contains(instance.name),
                    "[{disabled_list:?}] 面② {tool} 的出现必须与关闭集一致: {collected:?}"
                );
            }
        }
        for bare in &bare_names {
            assert!(
                !collected.contains(&bare.to_string()),
                "[{disabled_list:?}] 面② 不得出现裸名 {bare}: {collected:?}"
            );
        }

        // 面③：`open_builtin_bridges`（parent_tools 与 workflow builtin_tools 的共同 helper）。
        let parent_face = tool_names(&open_builtin_bridges(&pool, &disabled));
        assert_eq!(
            sorted(parent_face.clone()),
            expected_open_direct,
            "[{disabled_list:?}] 面③ parent_tools 提供面必须与 direct 集合一致"
        );

        // 面④：生产 workflow 工厂。
        let workflow = default_workflow_middleware_factory_with_pool(Some(Arc::clone(&pool)))
            .build_tools(&cwd, &disabled, None);
        let workflow_names = tool_names(&workflow);
        for name in &expected_open_direct {
            assert!(
                workflow_names.contains(name),
                "[{disabled_list:?}] 面④ workflow agent 工具列表必须含 {name}: {workflow_names:?}"
            );
        }
        for instance in BUILTIN_MCP_INSTANCES
            .iter()
            .filter(|i| closed.contains(i.name))
        {
            for tool in declared_effective_names(instance) {
                assert!(
                    !workflow_names.contains(&tool.to_string()),
                    "[{disabled_list:?}] 面④ 关闭实例的 {tool} 不得出现在 workflow: {workflow_names:?}"
                );
            }
        }
        for bare in &bare_names {
            assert!(
                !workflow_names.contains(&bare.to_string()),
                "[{disabled_list:?}] 面④ 不得出现裸名 {bare}: {workflow_names:?}"
            );
        }
    }

    // `McpMiddleware=false`：链槽位键不是 builtin 策略键。
    let chain_off: HashSet<String> = ["McpMiddleware".to_string()].into_iter().collect();
    assert!(
        closed_instances(&chain_off).is_empty(),
        "McpMiddleware 是链槽位键，不是 builtin 实例策略键（两表语义不重叠，A7）"
    );
    let workflow_off = default_workflow_middleware_factory_with_pool(Some(Arc::clone(&pool)))
        .build_tools(&cwd, &chain_off, None);
    let workflow_off_names = tool_names(&workflow_off);
    assert!(
        !workflow_off_names
            .iter()
            .any(|name| name.starts_with("mcp__")),
        "McpMiddleware 关闭后 workflow 面不得含任何 MCP 工具: {workflow_off_names:?}"
    );

    fixture.shutdown().await;
}

/// 无 orphan（pool 归属面）：关闭后 builtin task 表排空、pool 关闭报告完整。
///
/// 「server task 靠 EOF 自然收敛（`Quit`，不是 abort）」这条更强的证据由两条路径覆盖：
/// [`TappedLink::shutdown`]（每条线路用例的收尾断言）与 `mcp::builtin::runtime` 的
/// 收敛/超时用例。本用例只断言 pool 级的排空（生产归属下的关闭语义）。
#[tokio::test]
async fn shutdown_drains_builtin_tasks_without_orphan() {
    let fixture = StartupFixture::start().await;
    assert_eq!(
        fixture.pool.builtin_task_count(),
        BUILTIN_MCP_INSTANCES.len()
    );

    fixture.shutdown().await;
}

/// A13 ①②：两个实例是**不同的**连接对象（句柄非同源、连接键不同、对端 server 实例
/// 不同），且重连 `web` 只推进 `web` 的代际，`artifact` 的句柄与代际不变。
#[tokio::test]
async fn builtin_instances_are_distinct_and_reconnect_touches_one_generation() {
    let fixture = StartupFixture::start().await;
    let pool = Arc::clone(&fixture.pool);

    let web_before = pool.get_client("web").expect("web 必须已连接");
    let artifact_before = pool.get_client("artifact").expect("artifact 必须已连接");
    assert!(
        !Arc::ptr_eq(&web_before, &artifact_before),
        "两个实例的 Arc<McpClientHandle> 不得是同一份"
    );
    assert_ne!(
        McpConnectionKey::static_server("web"),
        McpConnectionKey::static_server("artifact"),
        "scoped connection identity 不得相同（pool 按 server name 派生，oauth.rs 同源）"
    );
    let server_info_name = |handle: &Arc<McpClientHandle>| {
        handle
            .peer
            .as_ref()
            .and_then(|peer| peer.peer_info())
            .and_then(|info| {
                info.server_info
                    .as_ref()
                    .map(|server| server.name.to_string())
            })
    };
    assert_ne!(
        server_info_name(&web_before),
        server_info_name(&artifact_before),
        "两条链路必须握到不同的 server 实例（各自的 rmcp ServerInfo 不同）"
    );

    let web_generation_before = pool.handle_generation(&web_before);
    let artifact_generation_before = pool.handle_generation(&artifact_before);
    pool.reconnect("web", None)
        .await
        .expect("builtin 实例必须能重连");

    let web_after = pool.get_client("web").expect("重连后必须留下新句柄");
    let artifact_after = pool.get_client("artifact").expect("artifact 必须仍在");
    assert!(
        !Arc::ptr_eq(&web_before, &web_after),
        "重连必须换新句柄（旧代证据不得继续有效）"
    );
    assert!(
        pool.handle_generation(&web_after) > web_generation_before,
        "重连后 web 的代际必须递增: {web_generation_before} -> {}",
        pool.handle_generation(&web_after)
    );
    assert!(
        Arc::ptr_eq(&artifact_before, &artifact_after),
        "重连 web 不得触碰 artifact 的句柄"
    );
    assert_eq!(
        pool.handle_generation(&artifact_after),
        artifact_generation_before,
        "重连 web 不得推进 artifact 的代际"
    );
    assert!(
        matches!(web_after.status, ClientStatus::Connected),
        "重连后 web 必须重新 Connected，实际: {:?}",
        web_after.status
    );
    assert_eq!(
        pool.builtin_task_count(),
        BUILTIN_MCP_INSTANCES.len(),
        "重连只替换本实例的 task（不新增、不残留）"
    );

    fixture.shutdown().await;
}

/// A13 ③：关闭 `web` 后 `artifact` 仍完成一次**真实** `tools/call`（经生产 handler），
/// 且能力面按关闭集收缩。
///
/// 该往返用真实 `ArtifactMcpServer`（`ArtifactTool::new(cwd)`）：不存在的文件在**网络之前**
/// 失败，因此结果是 IF-D14 的固定规则文本（错误形态、无路径 / 无凭据泄漏），而**往返本身**
/// 是完整的（请求真的上了链路、结果真的回来了）。成功上载形态由 `mcp::builtin::artifact`
/// 用注入客户端覆盖（需要网络或本地桩，不在本文件重复）。
#[tokio::test]
async fn closing_web_keeps_artifact_capability_and_real_call() {
    let fixture = StartupFixture::start().await;
    let pool = Arc::clone(&fixture.pool);
    let cwd = fixture.project.to_string_lossy().into_owned();
    let disabled: HashSet<String> = ["WebMiddleware".to_string()].into_iter().collect();
    let closed = closed_instances(&disabled);
    assert_eq!(
        closed,
        std::collections::BTreeSet::from(["web".to_string()])
    );

    // 能力面：目录与 parent_tools 提供面都只剩 artifact。
    let middleware = McpMiddleware::new(Arc::clone(&pool))
        .with_tool_pool(Arc::clone(&pool))
        .with_builtin_closures(closed.clone());
    let collected = tool_names(&middleware.collect_tools(&cwd));
    let web_tools = declared_effective_names(find("web").expect("web 已实现"));
    let artifact_tools = declared_effective_names(find("artifact").expect("artifact 已实现"));
    for tool in &web_tools {
        assert!(
            !collected.contains(&tool.to_string()),
            "关闭 WebMiddleware 后能力面不得含 {tool}: {collected:?}"
        );
    }
    for tool in &artifact_tools {
        assert!(
            collected.contains(&tool.to_string()),
            "另一个实例必须不受影响: {tool} 不在 {collected:?}"
        );
    }

    // 真实往返：经 parent_tools / workflow 面的同一 helper 取 bridge。
    let bridges = open_builtin_bridges(&pool, &disabled);
    let bridge_names = tool_names(&bridges);
    assert_eq!(sorted(bridge_names), sorted(artifact_tools.clone()));
    let bridge = bridges
        .into_iter()
        .find(|bridge| bridge.name() == artifact_tools[0])
        .expect("artifact 的 direct bridge 必须存在");
    assert!(
        bridge.is_direct(),
        "artifact 声明的 direct 必须生效（IF-D13）"
    );

    let error = bridge
        .invoke(
            json!({ "file_path": "missing-artifact-fixture.html" }),
            ToolContext::new(&[], &cwd),
        )
        .await
        .expect_err("不存在的文件必须在建立网络请求之前失败");
    let message = error.to_string();
    assert!(
        message.contains("withheld by policy"),
        "结果必须是 IF-D14 的固定规则文本，实际: {message}"
    );
    assert!(
        !message.contains(&cwd),
        "IF-D14 文本不得泄漏路径，实际: {message}"
    );

    fixture.shutdown().await;
}

// ══════════════════════════════════════════════════════════════════════════════════
// B. 线路观测（per-instance wire，A13 ④）：现代握手 / 审批两态 / 大 payload / 不串
// ══════════════════════════════════════════════════════════════════════════════════

/// 线路级证据（§10 R2）：`web` 实例的链路必须走 modern（`server/discover` 起手、
/// **线路全程无 `initialize`**），且 `tools/list` 真的上了线路、清单等于注册表声明。
#[tokio::test]
async fn web_instance_wire_uses_modern_discover_and_lists_tools() {
    let web = find("web").expect("web 已实现");
    let stubs: Vec<Arc<dyn BaseTool>> = web
        .tools
        .iter()
        .map(|declaration| {
            let stub = Arc::new(StubTool::fixed(declaration.original_name, "wire-fixture"));
            stub as Arc<dyn BaseTool>
        })
        .collect();
    let link = TappedLink::connect(web, "runtime-fixture-web", stubs).await;

    let methods = link.wire_methods();
    assert_eq!(
        methods.first().map(String::as_str),
        Some("server/discover"),
        "builtin 握手首帧必须是 server/discover（modern），实际: {methods:?}"
    );
    assert!(
        !methods.iter().any(|method| method == "initialize"),
        "modern 路径下线路不得出现 initialize 帧，实际: {methods:?}"
    );
    assert!(
        methods.iter().any(|method| method == "tools/list"),
        "tools/list 必须真的到达 server，实际: {methods:?}"
    );

    let served: Vec<String> = link
        .pool
        .get_client("web")
        .expect("web 必须已连接")
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();
    let declared: Vec<String> = web
        .tools
        .iter()
        .map(|declaration| declaration.original_name.to_string())
        .collect();
    assert_eq!(
        served, declared,
        "live tools/list 必须按声明顺序返回注册表工具"
    );

    link.shutdown().await;
}

/// 契约 6 缺口闭合（approve）：被提升为 direct 的 builtin 工具走**完整审批链**，批准后
/// 内置 server 的工具执行计数恰好 +1、线路上恰好一条 `tools/call`、结果内容可辨认。
///
/// 审批 broker 收到的名字是**模型面 effective name**；线路上的工具名是**原始名**
/// （归一不得泄漏到 wire，§3 IF-D15 / §9 规则 11）。
#[tokio::test]
async fn direct_builtin_tool_call_passes_approval_then_touches_wire_once() {
    let web = find("web").expect("web 已实现");
    let declaration = &web.tools[0];
    assert!(declaration.direct, "本用例要求该工具声明为 direct");
    let stub = Arc::new(StubTool::fixed(
        declaration.original_name,
        "builtin-runtime-ok",
    ));
    let stub_tool: Arc<dyn BaseTool> = Arc::clone(&stub) as Arc<dyn BaseTool>;
    let link = TappedLink::connect(web, "runtime-fixture-web", vec![stub_tool]).await;

    let call = tool_call(
        declaration.effective_name,
        json!({ "query": "builtin runtime approval" }),
    );
    let broker = Arc::new(RecordingBroker::approving());
    let middleware = PermissionMiddleware::new(
        Arc::clone(&broker) as Arc<dyn UserInteractionBroker>,
        default_requires_approval,
    );

    let results = run_approval_chain(&middleware, &call).await;
    assert_eq!(broker.requests(), 1, "恰好一次审批请求");
    assert_eq!(
        broker.seen(),
        vec![(call.name.clone(), call.input.clone())],
        "审批 broker 必须收到 effective name 与模型给出的入参"
    );
    let approved = match results.into_iter().next().expect("一个审批结果") {
        Ok(call) => call,
        Err(error) => panic!("批准后必须放行，实际: {error}"),
    };
    assert_eq!(approved.name, call.name, "批准不得改写模型侧工具名");

    let bridge = link.bridge(declaration.effective_name);
    assert!(
        bridge.is_direct(),
        "注册表声明为 direct 的 builtin 工具必须在类型化构造点生效（IF-D13）"
    );

    let text = bridge
        .invoke(approved.input.clone(), ToolContext::new(&[], "/tmp"))
        .await
        .expect("批准后的调用必须成功");
    assert_eq!(text, "builtin-runtime-ok", "工具结果内容必须可辨认");
    assert_eq!(
        stub.call_count(),
        1,
        "内置 server 的工具执行计数必须恰好 +1"
    );
    assert_eq!(
        link.served_calls(),
        vec![declaration.original_name.to_string()],
        "到达 handler 的工具名必须是原始名"
    );
    assert_eq!(
        link.wire_call_tool_count(),
        1,
        "线路上恰好一条 tools/call，实际: {:?}",
        link.wire_methods()
    );
    assert_eq!(
        stub.seen_inputs(),
        vec![call.input.clone()],
        "server 侧必须看到模型给出的入参（参数真的过了 wire）"
    );

    link.shutdown().await;
}

/// 契约 6 缺口闭合（reject）：拒绝后内置 server 的工具执行计数为 0、线路 `tools/call`
/// 为 0（链路本身是活的，handshake 帧在），结果为拒绝语义，且**不重试**（只审批一次）。
#[tokio::test]
async fn rejected_direct_tool_call_never_reaches_the_wire() {
    let web = find("web").expect("web 已实现");
    let declaration = &web.tools[0];
    let stub = Arc::new(StubTool::fixed(declaration.original_name, "must-not-run"));
    let stub_tool: Arc<dyn BaseTool> = Arc::clone(&stub) as Arc<dyn BaseTool>;
    let link = TappedLink::connect(web, "runtime-fixture-web", vec![stub_tool]).await;

    let call = tool_call(
        declaration.effective_name,
        json!({ "query": "builtin runtime rejection" }),
    );
    let broker = Arc::new(RecordingBroker::rejecting());
    let middleware = PermissionMiddleware::new(
        Arc::clone(&broker) as Arc<dyn UserInteractionBroker>,
        default_requires_approval,
    );

    let results = run_approval_chain(&middleware, &call).await;
    assert_eq!(broker.requests(), 1, "拒绝路径不得触发第二次审批");
    assert_eq!(broker.seen().len(), 1, "拒绝路径只记录一次审批项");
    let rejection = match results.into_iter().next().expect("一个审批结果") {
        Err(error) => error,
        Ok(call) => panic!("拒绝不得放行工具调用: {call:?}"),
    };
    match rejection {
        AgentError::ToolRejected { tool, .. } => {
            assert_eq!(tool, call.name, "拒绝语义必须指名被拒的 effective name")
        }
        other => panic!("拒绝必须映射为 ToolRejected，实际: {other:?}"),
    }

    assert_eq!(stub.call_count(), 0, "被拒的调用不得执行工具");
    assert!(link.served_calls().is_empty(), "不得有请求到达 handler");
    assert_eq!(link.wire_call_tool_count(), 0, "线路不得出现 tools/call");
    assert!(
        link.wire_methods()
            .iter()
            .any(|method| method == "tools/list"),
        "0 条 tools/call 不是因为链路没起来: {:?}",
        link.wire_methods()
    );

    link.shutdown().await;
}

/// 归一的反证（IF-D15 / §9 规则 11）：effective name **不是** wire 上的工具名——
/// 直接把它发到真实 handler 上会被按未知工具拒绝（生产 handler 同样只认原始名）。
#[tokio::test]
async fn effective_name_is_not_a_wire_name_on_real_handler() {
    let web = find("web").expect("web 已实现");
    let declaration = &web.tools[0];
    let stub = Arc::new(StubTool::fixed(declaration.original_name, "unused"));
    let stub_tool: Arc<dyn BaseTool> = Arc::clone(&stub) as Arc<dyn BaseTool>;
    let link = TappedLink::connect(web, "runtime-fixture-web", vec![stub_tool]).await;

    let error = link
        .peer()
        .call_tool(CallToolRequestParams::new(declaration.effective_name))
        .await
        .expect_err("effective name 不得是 wire 名");
    assert!(
        link.served_calls()
            .contains(&declaration.effective_name.to_string()),
        "handler 必须真的收到该请求（拒绝发生在路由层）"
    );
    match error {
        rmcp::ServiceError::McpError(data) => {
            assert_eq!(
                data.code.0,
                ErrorCode::INVALID_PARAMS.0,
                "未知工具名必须回 invalid_params"
            );
            assert!(
                data.message.contains("unknown tool"),
                "错误文本必须点明未知工具，实际: {}",
                data.message
            );
        }
        other => panic!("期望 McpError(invalid_params)，实际: {other:?}"),
    }
    assert_eq!(stub.call_count(), 0, "路由失败不得执行工具");

    link.shutdown().await;
}

/// 大 payload（A16）：`BUILTIN_DUPLEX_BUF` 只影响背压，**不是单帧上限**。
///
/// 两个方向都试：① 大**结果**（~300 KiB 正文，WebFetch 级）逐字节完整返回；
/// ② 大**请求**（~200 KiB 参数）完整到达 server。
#[tokio::test]
async fn large_payload_crosses_builtin_instance_intact() {
    let web = find("web").expect("web 已实现");
    let declaration = &web.tools[0];
    let body = format!("HEAD:{}:TAIL", "x".repeat(LARGE_BODY_BYTES));
    assert_eq!(body.lines().count(), 1, "用例前提：正文单行");
    assert!(
        body.len() > LARGE_BODY_BYTES,
        "用例前提：正文必须远大于 duplex 容量"
    );

    let reply_body = body.clone();
    let stub = Arc::new(StubTool::new(
        declaration.original_name,
        Arc::new(
            move |input: &Value| match input.get("payload").and_then(Value::as_str) {
                Some(payload) => format!("received-bytes:{}", payload.len()),
                None => reply_body.clone(),
            },
        ),
    ));
    let stub_tool: Arc<dyn BaseTool> = Arc::clone(&stub) as Arc<dyn BaseTool>;
    let link = TappedLink::connect(web, "runtime-fixture-web", vec![stub_tool]).await;
    let bridge = link.bridge(declaration.effective_name);

    let text = bridge
        .invoke(json!({}), ToolContext::new(&[], "/tmp"))
        .await
        .expect("大正文必须完整往返");
    assert_eq!(text.len(), body.len(), "大正文长度不得被截断");
    assert_eq!(text, body, "大正文必须逐字节完整");
    assert_eq!(link.wire_call_tool_count(), 1);

    let payload = "y".repeat(LARGE_INPUT_BYTES);
    let echo = bridge
        .invoke(
            json!({ "payload": payload.clone() }),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .expect("大请求帧必须完整到达 server");
    assert_eq!(
        echo,
        format!("received-bytes:{}", payload.len()),
        "server 侧必须收到完整的大参数"
    );
    assert_eq!(link.wire_call_tool_count(), 2);
    assert_eq!(stub.call_count(), 2);

    link.shutdown().await;
}

/// A13 ④/⑤：两个实例的 wire 互不串（发给 `web` 的请求只出现在 `web` 的线路上），
/// namespace 路由正确——某实例的 effective name bridge 只落该实例的 handler
/// （由 `mcp_server_name()` 与各自的 server 侧观测共同锁定）。
#[tokio::test]
async fn per_instance_wire_does_not_cross_between_instances() {
    let web = find("web").expect("web 已实现");
    let artifact = find("artifact").expect("artifact 已实现");
    let web_stub = Arc::new(StubTool::fixed(web.tools[0].original_name, "web-reply"));
    let artifact_stub = Arc::new(StubTool::fixed(
        artifact.tools[0].original_name,
        "artifact-reply",
    ));
    let web_tool: Arc<dyn BaseTool> = Arc::clone(&web_stub) as Arc<dyn BaseTool>;
    let artifact_tool: Arc<dyn BaseTool> = Arc::clone(&artifact_stub) as Arc<dyn BaseTool>;
    let web_link = TappedLink::connect(web, "runtime-fixture-web", vec![web_tool]).await;
    let artifact_link =
        TappedLink::connect(artifact, "runtime-fixture-artifact", vec![artifact_tool]).await;

    let web_bridge = web_link.bridge(web.tools[0].effective_name);
    let artifact_bridge = artifact_link.bridge(artifact.tools[0].effective_name);
    assert_eq!(web_bridge.mcp_server_name(), Some("web"));
    assert_eq!(artifact_bridge.mcp_server_name(), Some("artifact"));

    let text = web_bridge
        .invoke(json!({}), ToolContext::new(&[], "/tmp"))
        .await
        .expect("web 调用必须成功");
    assert_eq!(text, "web-reply");
    assert_eq!(web_stub.call_count(), 1);
    assert_eq!(web_link.wire_call_tool_count(), 1);
    assert_eq!(
        artifact_link.wire_call_tool_count(),
        0,
        "发往 web 的请求不得出现在 artifact 的 wire 上: {:?}",
        artifact_link.wire_methods()
    );
    assert_eq!(artifact_stub.call_count(), 0);

    let text = artifact_bridge
        .invoke(json!({}), ToolContext::new(&[], "/tmp"))
        .await
        .expect("artifact 调用必须成功");
    assert_eq!(text, "artifact-reply");
    assert_eq!(artifact_stub.call_count(), 1);
    assert_eq!(artifact_link.wire_call_tool_count(), 1);
    assert_eq!(
        web_link.wire_call_tool_count(),
        1,
        "发往 artifact 的请求不得出现在 web 的 wire 上: {:?}",
        web_link.wire_methods()
    );
    assert_eq!(web_stub.call_count(), 1);

    web_link.shutdown().await;
    artifact_link.shutdown().await;
}
