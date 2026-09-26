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
use crate::mcp::builtin::runtime::{
    spawn_builtin_transport_with_tap, BuiltinServerExit, BuiltinServerTask, BuiltinWireLog,
    BUILTIN_CONVERGE_TIMEOUT,
};
use crate::mcp::builtin::BUILTIN_INJECTION_ENV;
use crate::mcp::client::{
    serve_client_auto, McpConnectionKey, McpInitStatus, McpServiceWrapper, SystemMcpManifest,
};
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
            "正常关闭必须靠 EOF 自然收敛（spike Q2 证据），实际: {exit:?}"
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

/// A13 ④/⑤ 的**同 pool 容器**面：两条 builtin 实例的链路（client 半边都移交进
/// `pool.services`）建在**同一个** `McpClientPool` 里时，pool 的生命周期动作
/// （`reconnect` / `shutdown`）**只**作用于被点名的实例。
///
/// 与既有 [`per_instance_wire_does_not_cross_between_instances`] 的差别：那条用例的两条
/// 链路各自 new 一个 pool，且两条 client 半边都留在夹具手里，因此**不覆盖**「同一 pool
/// 容器内，重连 / 关闭的作用域是否只限于被点名实例」。本用例经
/// [`TappedLink::connect_into`] 参数化 pool，并由 [`TappedLink::hand_service_to_pool`] 把
/// 两条 client 半边移交生产表，闭合该缺口。
///
/// 断言口径（逐条）：
/// 1. web 调用一次 ⇒ web 线路 `tools/call == 1`、artifact 线路 `tools/call == 0`；
/// 2. 取 artifact 的 method 快照后调用 artifact ⇒ web 仍 `1`、artifact `1`；
/// 3. **method 序列逐字对照**（不是只比长度）：web 动作前后各取快照，artifact 的 log
///    必须**逐字不变**、web 的 log 必须**只追加一帧 `tools/call`**（前缀逐字相等）；
///    artifact 侧反向同理；
/// 4. **移交前提**：两条 client 半边都在 `pool.services` 里；未移交时 `reconnect` 的
///    「关旧 service」对夹具链路是**空操作**，第 5 条②的「逐字不变」在任何实现下都成立
///    （没有失败模式）；
/// 5. **重连隔离**（`pool.reconnect("web", None)`，先 `bind_execution_cwd`）：
///    ① 被点名的 web：其旧 client 半边由 pool 关闭 ⇒ **夹具自持**的 web server task 靠
///    EOF **自然**收敛 `Quit`——收敛的触发者是 pool 的动作，不是夹具自己的收尾；
///    ② 未被点名的 artifact：句柄 `Arc` 同一、server task **未**结束、method 序列**逐字
///    等于**重连前快照、server 侧计数不变，且 bridge 仍能完成一次完整 `tools/call`
///    （把无关实例一并关掉的实现会让这一步红）；
///    ③ 重连新建的 web 链路走**生产** handler（对端名不再是夹具替身）且登记进 pool task 表；
/// 6. 收尾：pool 关闭时两条 server task 均靠 EOF 自然收敛 `Quit`（web 那条由重连触发、
///    artifact 那条由 `pool.shutdown()` 关闭其 client 半边触发），
///    `builtin_task_count() == 0`，不留 orphan。
///
/// 证据边界（诚实标注）：
/// - 第 1–3 条的「不串」在本用例里**不是路由隔离断言**：工具调用不经 pool 转发（每个
///   bridge 自带 peer），pool 容器只承载「表」与生命周期。它们排除的是 bridge↔实例
///   **绑定写错**与观测面串台，**增量可证伪力有限**；本用例的实质内容在第 4–6 条
///   （pool 生命周期动作的作用域）。
/// - server 半边是 [`FixtureBuiltinHandler`] **替身**（原因见文件头「证据边界 B」）。
/// - 「生产 handler 在真 loader 下同 pool 的 wire 序列隔离」**无证据面**：生产 transport
///   没有 per-instance tap，[`StartupFixture`] 系用例只观察握手 / 目录 / 真实往返，不看
///   线路帧序列。本记录**不宣称**该命题（acceptance §12.5）。
#[tokio::test]
async fn same_pool_instances_never_cross_wires_and_reconnect_touches_one_link() {
    let web = find("web").expect("web 已实现");
    let artifact = find("artifact").expect("artifact 已实现");
    let web_stub = Arc::new(StubTool::fixed(web.tools[0].original_name, "web-reply"));
    let artifact_stub = Arc::new(StubTool::fixed(
        artifact.tools[0].original_name,
        "artifact-reply",
    ));
    let web_tool: Arc<dyn BaseTool> = Arc::clone(&web_stub) as Arc<dyn BaseTool>;
    let artifact_tool: Arc<dyn BaseTool> = Arc::clone(&artifact_stub) as Arc<dyn BaseTool>;

    // **同一个** pool 容器里两条链路：本用例与 `per_instance_wire_*` 的唯一结构差别。
    let pool = Arc::new(McpClientPool::new_empty());
    let mut web_link = TappedLink::connect_into(
        Arc::clone(&pool),
        web,
        "runtime-fixture-web",
        vec![web_tool],
    )
    .await;
    let mut artifact_link = TappedLink::connect_into(
        Arc::clone(&pool),
        artifact,
        "runtime-fixture-artifact",
        vec![artifact_tool],
    )
    .await;

    // 移交前提（本用例的实质所在）：两条 client 半边都进 pool 的生产表，此后
    // `reconnect` 的「关旧 service」不再是被点名实例上的空操作，而 artifact 侧的
    // 「逐字不变 + 仍可往返」也才有了失败模式（把无关实例一并关掉的实现会红）。
    web_link.hand_service_to_pool();
    artifact_link.hand_service_to_pool();
    assert_eq!(
        pool.services.lock().len(),
        2,
        "两条夹具链路的 client 半边都必须登记进 pool.services"
    );

    let web_bridge = web_link.bridge(web.tools[0].effective_name);
    let artifact_bridge = artifact_link.bridge(artifact.tools[0].effective_name);
    assert_eq!(web_bridge.mcp_server_name(), Some("web"));
    assert_eq!(artifact_bridge.mcp_server_name(), Some("artifact"));

    // 两条链路各自走完握手 + live `tools/list`；此后各自的 log 只应因**自己**的动作增长。
    let web_log_0 = web_link.wire_methods();
    let artifact_log_0 = artifact_link.wire_methods();
    assert!(
        !web_log_0.is_empty() && !artifact_log_0.is_empty(),
        "用例前提：两条链路都已产生握手 / tools/list 帧: web={web_log_0:?} artifact={artifact_log_0:?}"
    );
    assert_eq!(
        web_link.wire_call_tool_count(),
        0,
        "用例前提：web 链路尚无 tools/call"
    );
    assert_eq!(
        artifact_link.wire_call_tool_count(),
        0,
        "用例前提：artifact 链路尚无 tools/call"
    );
    assert_eq!(web_link.served_calls(), Vec::<String>::new());
    assert_eq!(artifact_link.served_calls(), Vec::<String>::new());

    // ── 动作 1：只碰 web ────────────────────────────────────────────────────────
    let text = web_bridge
        .invoke(json!({}), ToolContext::new(&[], "/tmp"))
        .await
        .expect("web 调用必须成功");
    assert_eq!(text, "web-reply");
    assert_eq!(web_stub.call_count(), 1);
    assert_eq!(
        artifact_stub.call_count(),
        0,
        "web 的动作不得执行 artifact 的工具"
    );
    assert_eq!(web_link.wire_call_tool_count(), 1);
    assert_eq!(
        web_link.served_calls(),
        vec![web.tools[0].original_name.to_string()],
        "web 的 server 侧只应见过一次自己的原始名 tools/call"
    );
    assert_eq!(
        artifact_link.wire_call_tool_count(),
        0,
        "发往 web 的请求不得出现在 artifact 的 wire 上: {:?}",
        artifact_link.wire_methods()
    );

    // method 序列逐字对照（不是只比长度）。
    let web_log_1 = web_link.wire_methods();
    let artifact_log_1 = artifact_link.wire_methods();
    assert_eq!(
        artifact_log_1, artifact_log_0,
        "web 的动作不得向 artifact 的 log 追加任何帧"
    );
    assert_eq!(
        artifact_link.served_calls(),
        Vec::<String>::new(),
        "web 的动作不得让 artifact 的 server 侧看到 tools/call"
    );
    assert_eq!(
        web_log_1.len(),
        web_log_0.len() + 1,
        "web 的 log 只应追加一帧: {web_log_1:?}"
    );
    assert_eq!(
        &web_log_1[..web_log_0.len()],
        web_log_0.as_slice(),
        "web 的 log 只允许追加，不得重排 / 重写既有帧"
    );
    assert_eq!(
        web_log_1.last().map(String::as_str),
        Some("tools/call"),
        "web 追加的帧必须是 tools/call: {web_log_1:?}"
    );

    // ── 动作 2：artifact 侧对称（快照 A = `artifact_log_1`）─────────────────────
    let text = artifact_bridge
        .invoke(json!({}), ToolContext::new(&[], "/tmp"))
        .await
        .expect("artifact 调用必须成功");
    assert_eq!(text, "artifact-reply");
    assert_eq!(artifact_stub.call_count(), 1);
    assert_eq!(
        web_stub.call_count(),
        1,
        "artifact 的动作不得执行 web 的工具"
    );
    assert_eq!(artifact_link.wire_call_tool_count(), 1);
    assert_eq!(
        artifact_link.served_calls(),
        vec![artifact.tools[0].original_name.to_string()]
    );
    assert_eq!(
        web_link.wire_call_tool_count(),
        1,
        "发往 artifact 的请求不得出现在 web 的 wire 上"
    );

    let web_log_2 = web_link.wire_methods();
    let artifact_log_2 = artifact_link.wire_methods();
    assert_eq!(
        web_log_2, web_log_1,
        "artifact 的动作不得向 web 的 log 追加任何帧"
    );
    assert_eq!(
        artifact_log_2.len(),
        artifact_log_1.len() + 1,
        "artifact 的 log 只应追加一帧: {artifact_log_2:?}"
    );
    assert_eq!(
        &artifact_log_2[..artifact_log_1.len()],
        artifact_log_1.as_slice(),
        "artifact 的 log 只允许追加，不得重排 / 重写既有帧"
    );
    assert_eq!(
        artifact_log_2.last().map(String::as_str),
        Some("tools/call"),
        "artifact 追加的帧必须是 tools/call: {artifact_log_2:?}"
    );

    // ── 动作 3：reconnect 只动被点名的 web ─────────────────────────────────────
    // builtin 重连分支要求 pool 级 `execution_cwd` 已绑定，否则直接
    // `ConnectionFailed{ "MCP execution directory is not initialized" }`
    // （`mcp/reconnect.rs:98-104`）。两条 client 半边已移交 `pool.services`（见上文
    // 「移交前提」），因此重连会真的关闭被点名实例的旧 service；未被点名实例的 service
    // 留在表里，仍是其后端（下面既断言它仍在表内，也用它完成一次真实往返）。
    let cwd = tempfile::tempdir().expect("tempdir");
    pool.bind_execution_cwd(cwd.path())
        .expect("reconnect 前提：pool 级 execution_cwd 绑定必须成功");
    assert_eq!(
        pool.builtin_task_count(),
        0,
        "用例前提：两条夹具链路不登记 pool task 表（server 半边归属在夹具手里）"
    );
    assert!(
        !web_link.server_task.is_finished() && !artifact_link.server_task.is_finished(),
        "用例前提：重连前两条夹具 server task 都在运行"
    );

    let web_handle_before = pool.get_client("web").expect("web 句柄必须存在");
    let artifact_handle_before = pool.get_client("artifact").expect("artifact 句柄必须存在");
    let artifact_log_before_reconnect = artifact_link.wire_methods();
    let artifact_served_before_reconnect = artifact_link.served_calls();
    let peer_name_of = |handle: &Arc<McpClientHandle>| -> Option<String> {
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
    // 判别基线：重连**前**的 web 对端确实是夹具替身。没有这一条，「重连后不再是替身」
    // 的断言就可能在任何对端名上碰巧成立（假绿）。
    assert_eq!(
        peer_name_of(&web_handle_before).as_deref(),
        Some("runtime-fixture-web"),
        "用例前提：重连前的 web 对端是夹具替身"
    );

    pool.reconnect("web", None)
        .await
        .expect("builtin 实例必须能重连");

    // 反证「重连真的发生了」：否则下面「artifact 逐字不变」可能只是整体 no-op 的假绿。
    let web_handle_after = pool.get_client("web").expect("重连后必须留下新句柄");
    assert!(
        !Arc::ptr_eq(&web_handle_before, &web_handle_after),
        "重连必须换新句柄（旧代证据不得继续有效）"
    );
    assert!(
        matches!(web_handle_after.status, ClientStatus::Connected),
        "重连后 web 必须重新 Connected，实际: {:?}",
        web_handle_after.status
    );
    assert!(
        Arc::ptr_eq(
            &artifact_handle_before,
            &pool.get_client("artifact").expect("artifact 必须仍在")
        ),
        "重连 web 不得触碰 artifact 的句柄"
    );
    assert_eq!(
        pool.builtin_task_count(),
        1,
        "重连新建的 web 链路必须登记进 pool task 表（与夹具链路不同）"
    );
    // 重连走**生产** handler（`spawn_builtin_transport`），不是夹具替身：这是「重连真的
    // 重建了链路」的旁证，也说明此后只有 artifact 侧（仍是替身）被断言。
    let web_peer_name = peer_name_of(&web_handle_after);
    assert!(
        web_peer_name
            .as_deref()
            .is_some_and(|name| name != "runtime-fixture-web"),
        "重连后的 web 必须已重新握手到生产 handler（不是夹具替身），实际: {web_peer_name:?}"
    );

    // 被点名的 web：pool 关闭了它登记的**夹具** client 半边（`reconnect.rs:57-60` 的
    // remove + `close_with_timeout`）⇒ 夹具自持的 server task 靠 EOF **自然**收敛。
    // 收敛的触发者是 pool 的动作（本用例在此之前只调用了 `reconnect`），因此这一条是
    // 「重连真的关掉了旧链路」的因果证据，而不是夹具自己的收尾。
    let web_exit = web_link.converge_task().await;
    assert!(
        matches!(web_exit, BuiltinServerExit::Quit(_)),
        "重连必须关掉被点名实例的旧 client 半边（其 server task 随之自然收敛），实际: {web_exit:?}"
    );

    // artifact 侧：service 仍在 pool 表内 + server task 未结束 + method 序列**逐字等于**
    // 重连前快照 + 仍可完成一次完整往返。（移交之前，「逐字不变」在任何实现下都成立；
    // 移交之后，若重连把无关实例一并关掉，下面的往返会失败。）
    assert!(
        pool.services.lock().contains_key("artifact"),
        "重连 web 不得把 artifact 的 client 半边移出 pool.services"
    );
    assert!(
        !artifact_link.server_task.is_finished(),
        "重连 web 不得让 artifact 的 server task 结束"
    );
    assert_eq!(
        artifact_link.wire_methods(),
        artifact_log_before_reconnect,
        "重连 web 不得向 artifact 的 log 追加任何帧"
    );
    assert_eq!(
        artifact_link.served_calls(),
        artifact_served_before_reconnect,
        "重连 web 不得让 artifact 的 server 侧多一次 tools/call"
    );

    let text = artifact_bridge
        .invoke(
            json!({ "after": "reconnect" }),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .expect("重连 web 后 artifact 必须仍可调用");
    assert_eq!(text, "artifact-reply");
    assert_eq!(artifact_stub.call_count(), 2);
    assert_eq!(artifact_link.wire_call_tool_count(), 2);
    assert_eq!(
        &artifact_link.wire_methods()[..artifact_log_before_reconnect.len()],
        artifact_log_before_reconnect.as_slice(),
        "重连后 artifact 的 log 仍只允许追加"
    );

    // ── 收尾：两条夹具链路各自 `Quit` 收敛 + pool 归属 task 排空 ────────────────
    // `TappedLink::shutdown` 内部断言 `BuiltinServerExit::Quit`（不得 `AbortedAfterTimeout`）
    // 与 `task.is_finished()`。
    // web 夹具链路的 server task 已在上文断言收敛（触发者 = reconnect）；artifact 夹具
    // 链路的 client 半边仍在 pool 表里，由 `pool.shutdown()` 关闭，其 server task 随之收敛。
    pool.begin_shutdown();
    // 重连新建的 web 链路**登记在 pool 上**（`reconnect.rs:118-122` 的
    // `register_builtin_task`），与两条夹具链路归属不同：它必须由 pool 侧收敛，且同样
    // 落在 `Quit`。（它的 client 半边也在 `services` 里，由下面的 `pool.shutdown()` 关闭；
    // 此处先按 key 取出 task 并断言其收敛事实。）
    let exit = pool
        .close_builtin_task("web")
        .await
        .expect("重连注册的 builtin task 必须还在 pool task 表里");
    assert!(
        matches!(exit, BuiltinServerExit::Quit(_)),
        "pool 关闭时重连链路也必须靠 EOF 自然收敛，实际: {exit:?}"
    );
    let report = pool.shutdown().await;
    assert!(report.is_complete(), "pool 关闭必须收敛: {report:?}");
    let artifact_exit = artifact_link.converge_task().await;
    assert!(
        matches!(artifact_exit, BuiltinServerExit::Quit(_)),
        "pool 关闭必须关掉 artifact 的 client 半边（其 server task 随之自然收敛），实际: {artifact_exit:?}"
    );
    assert_eq!(
        pool.builtin_task_count(),
        0,
        "pool 关闭后不得残留 builtin server task（含重连新建的那条）"
    );
}

/// acceptance §7 第 5 条（builtin 工具体内**在飞取消**）的**降级口径**：外层取消语义
/// ——无重放、不 panic、pool 仍可服务——**不**断言 IF-D14 的 error 文本。
///
/// 降级理由（现状代码事实，不是取舍）：agent loop 的取消是
/// `tokio::select! { biased; _ = cancel.cancelled() => Err(EffectiveToolError::new(Cancelled,
/// "interrupted by user")) … }`（`peri-agent/src/agent/stages/tool_dispatch/execution.rs:327-334`）
/// ——命中即 **drop 掉 `invoke` future**。rmcp 不在 future drop 时自动发
/// `notifications/cancelled`，builtin server handler 也收不到取消
/// （`mcp/builtin/web.rs:164-171` / `artifact.rs:81-86` 把 `RequestContext` 丢弃）。
/// 因此「取消 ⇒ 工具体内的 IF-D14 error 文本」在现状下**无因果**，本用例只断言
/// 外层可观察事实。
///
/// 断言口径（逐条）：
/// 1. 取消分支命中（`EffectiveToolErrorCode::Cancelled` + 文本 `interrupted by user`）；
/// 2. **无重放**：wire 上 `tools/call` 恰好 1 条、server 侧恰好见过 1 次、工具体恰好进入 1 次；
/// 3. 放行被弃置的在飞 handler 后，**同一个** bridge 再走一次完整往返仍成功
///    （pool / client service 仍可服务，不是「取消一次之后永久坏掉」）；
/// 4. 收尾 `shutdown()` 收敛 `Quit` + task 排空（断言在 [`TappedLink::shutdown`] 内）。
///
/// race 的写法刻意与 `execution.rs:327-334` **同形**（`biased` + 取消分支在前 +
/// 取消分支产出同一份 `EffectiveToolError`），因此本用例观测的是**这份 race 形状**在真实
/// builtin 链路（真实 transport / 真实 client service / 真实 handler 路由 / 真实 bridge）
/// 上的语义。**边界**：它不驱动 `execution.rs` 本身（那是 agent loop 的调用点，本层不可
/// 命名），也不覆盖「取消通知真的传到工具体内」（现状做不到，见上）。
#[tokio::test]
async fn builtin_handler_in_flight_cancel_has_no_replay_and_keeps_pool_serving() {
    let web = find("web").expect("web 已实现");
    let declaration = &web.tools[0];
    let gated = Arc::new(GatedTool::new(declaration.original_name));
    let gated_tool: Arc<dyn BaseTool> = Arc::clone(&gated) as Arc<dyn BaseTool>;
    let link = TappedLink::connect(web, "runtime-fixture-web", vec![gated_tool]).await;
    let bridge = link.bridge(declaration.effective_name);
    let cancel = AgentCancellationToken::new();

    // 等「server 侧已进入本次 tools/call」再取消：这是「在飞」的定义点。
    let canceller = {
        let entered = Arc::clone(&gated.entered);
        let cancel = cancel.clone();
        async move {
            entered.notified().await;
            cancel.cancel();
        }
    };
    // 复刻 agent loop 的 race（`execution.rs:327-334` 同形）。
    let racer = async {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(EffectiveToolError::new(
                EffectiveToolErrorCode::Cancelled,
                "interrupted by user",
            )),
            result = bridge.invoke(
                json!({ "payload": "in-flight" }),
                ToolContext::new(&[], "/tmp"),
            ) => result.map_err(|error| {
                EffectiveToolError::new(EffectiveToolErrorCode::ToolFailed, error.to_string())
            }),
        }
    };

    let ((), outcome) = tokio::join!(canceller, racer);

    // ① 取消形态。
    let error = outcome.expect_err("取消必须命中 race 的取消分支（不得让调用跑完）");
    assert_eq!(
        error.code,
        EffectiveToolErrorCode::Cancelled,
        "取消分支必须是 Cancelled: {error:?}"
    );
    assert!(
        error.message.contains("interrupted by user"),
        "取消文本必须与 agent loop 同文案: {error:?}"
    );

    // ② 无重放：取消不得在 wire 上产生第二次 tools/call，也不得让工具体再进入一次。
    assert_eq!(
        link.wire_call_tool_count(),
        1,
        "取消不得重放：wire 上只应有一次 tools/call，实际 methods={:?}",
        link.wire_methods()
    );
    assert_eq!(
        link.served_calls(),
        vec![declaration.original_name.to_string()],
        "server 侧只应见过一次 tools/call"
    );
    assert_eq!(
        gated.call_count(),
        1,
        "工具体只应被进入一次（取消不重放、不重试）"
    );

    // ③ 放行被弃置的在飞 handler，再确认 pool 仍可服务。
    gated.release.notify_one();
    gated.finished.notified().await;
    assert_eq!(
        gated.call_count(),
        1,
        "放行只收敛那一次在飞调用，不得引出新调用"
    );

    let text = bridge
        .invoke(
            json!({ "payload": "after-cancel" }),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .expect("取消后同一条 wire / 同一 client service 必须仍可服务");
    assert_eq!(text, "gated");
    assert_eq!(gated.call_count(), 2);
    assert_eq!(link.wire_call_tool_count(), 2);
    assert_eq!(link.served_calls().len(), 2);

    // ④ 收尾：`Quit` 收敛 + task 排空（在 `shutdown` 内断言）。
    link.shutdown().await;
}

/// acceptance §7 第 8 条（**启动期取消 ⇒ `Interrupted`**）在 middleware 层的证据
/// （sub-plan H 冻结名口径）。
///
/// 构造：一个 `system_mcp = true` 的 builtin 实例、清单已发布（闸门**真的有** System
/// 依赖可等）、`system_mcp_timeout` 取远大于用例时长的 60s、**不**提交任何连接 /
/// discovery evidence ⇒ 闸门会停在等待；token **预先**取消。
///
/// 断言：`before_react_start` 返回 `Err(AgentError::Interrupted)`（**不是**
/// `MiddlewareError`）、不暂存候选、耗时远小于 `system_mcp_timeout`（排除 timeout 路径）。
///
/// 两条口径必须与断言一起读：
/// ① 本用例驱动的是**闸门内**取消，命中 `mcp/client/readiness.rs:395-397`（循环入口判
///    cancel）或 `:619-624`（`wait_for_readiness_change` 的 `tokio::select!`，取消分支
///    在 `:620`）**之一**；用例**不区分**这两条分支——它只有「返回值 + 未暂存候选」两个
///    观测点，两条分支产出同一个 `SystemReadinessError::Cancelled`。按预取消 token 的
///    **静态读法**应先命中 `:395-397`，但这是代码阅读结论，本用例不拿它当断言，也不
///    声称覆盖另一条。
/// ② host 层「prompt 启动后、闸门等待中被取消」的路径当前**没有确定性门闩**可断言
///    （`peri-agent/src/agent/stages/mod.rs:691-695` 会在 Receive 前先早退），因此本用例
///    是这套语义在 **middleware 层**的证据，不等于 host 层已端到端验证。
#[tokio::test]
async fn builtin_instance_cancellation_maps_to_interrupted() {
    /// 用例前提量：远大于本用例的实测时长，使「耗时远小于它」即排除 timeout 路径。
    const STARTUP_TIMEOUT_MS: u64 = 60_000;

    let web = find("web").expect("web 已实现");
    let pool = Arc::new(McpClientPool::new_pending());
    let mut config = builtin_entry(web);
    config.system_mcp_timeout = Some(STARTUP_TIMEOUT_MS);
    pool.configs.write().insert(web.name.to_string(), config);
    // 清单已发布（requirements 才非空）但**不提交任何连接 / discovery evidence**：
    // 闸门若无取消就必须一直等到 `system_mcp_timeout`。
    pool.publish_system_manifest(SystemMcpManifest::Loaded);

    let requirements = pool.system_requirements();
    assert_eq!(
        requirements.len(),
        1,
        "用例前提：闸门必须真的有 System 依赖可等（否则「取消」无等待可言）"
    );
    assert_eq!(requirements[0].server, web.name);
    assert_eq!(
        requirements[0].timeout,
        Duration::from_millis(STARTUP_TIMEOUT_MS),
        "用例前提：等待上界必须是本用例声明的 60s（timeout 路径的排除基线）"
    );
    assert!(
        !requirements[0].required_tools.is_empty(),
        "用例前提：builtin 实例必须声明必需工具"
    );

    let cancel = AgentCancellationToken::new();
    cancel.cancel();
    let middleware =
        McpMiddleware::new(Arc::clone(&pool)).with_skill_discovery(None, cancel.clone());
    let mut probe = StartupProbe::default();

    let started_at = std::time::Instant::now();
    let error = Middleware::before_react_start(&middleware, &mut probe)
        .await
        .expect_err("取消必须中断本次启动");
    let elapsed = started_at.elapsed();

    assert!(
        matches!(error, AgentError::Interrupted),
        "闸门内取消必须映射 Interrupted（不是 MiddlewareError / 不是 fatal）: {error:?}"
    );
    assert!(
        probe.staged.is_none(),
        "取消不得暂存启动候选（不发布 ready、不写宿主共享工具表）"
    );
    assert_eq!(probe.stage_calls, 0, "取消路径不得调用 stage_startup_tools");
    assert!(
        elapsed < Duration::from_millis(STARTUP_TIMEOUT_MS / 10),
        "启动取消必须立即返回，耗时不得接近 system_mcp_timeout（排除 timeout 路径）: {elapsed:?}"
    );

    pool.begin_shutdown();
    let report = pool.shutdown().await;
    assert!(report.is_complete(), "pool 关闭必须收敛: {report:?}");
    assert_eq!(
        pool.builtin_task_count(),
        0,
        "本用例不建 builtin 链路，关闭后 task 表必须为空（不留 orphan）"
    );
}
