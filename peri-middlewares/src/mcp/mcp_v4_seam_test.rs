//! D-02：MCP v4-part-1 的 crate 内 seam 测试。
//!
//! 定位（主 plan §6 W5 / §8 契约 3、4；sub-plan D §7 D-02）：
//! - 断言层次是 **crate 内可观察层**：`McpMiddleware` 启动闸门的返回值、经
//!   `StartupState` 提交的候选，以及 `Middleware::collect_tools` 收集视图的
//!   direct 集合。首个 LLM 请求的 tools 入参由 B-07 在 `peri-acp` host seam
//!   断言（主 plan §5 R9）；纯函数层语义由 C-INJ-02 的 `system_tools_test.rs`
//!   覆盖，本文件只在**接线后**的层上复核。
//! - 本文件不调用 `build_session_tool_view`（`pub(super)`，只在 `peri-agent`
//!   内可断言），也不修改任何生产文件。
//!
//! 夹具与证据边界（诚实声明）：
//! - 客户端侧走真实 `serve_client_auto`：真实 rmcp lifecycle、真实 `peer_info`
//!   协商与真实 transport 关闭语义，闸门读到的协议证据不是伪造的。
//! - 工具声明取自 fixture 写入的 `McpClientHandle::tools`（真实发现路径写入的
//!   同一字段）；配置清单与本代 `DiscoveryEvidence` 由测试按 B-02 的提交契约
//!   显式发布，替代需要真实子进程 / HTTP 端点的 `initialize.rs` 路径。
//! - 因此本文件证明「闸门 → 候选 → 收集视图」这条 crate 内链路的分类与
//!   namespace 解析，不证明 transport 端到端，也不构成五个 MCP 迁移完成的证据。

use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::plugin::McpServerConfig;
use peri_agent::error::{AgentError, AgentResult};
use peri_agent::middleware::{capabilities as hook_state, r#trait::Middleware};
use peri_agent::session::tool_catalog::{StartupRequiredTool, StartupToolUpdate};
use peri_agent::tools::BaseTool;
use rmcp::model::Tool;
use rmcp::service::RoleClient;
use rmcp::transport::async_rw::AsyncRwTransport;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

use super::apps::McpCapabilityProfile;
use super::client::{
    serve_client_auto, ClientStatus, DiscoveryEvidence, McpClientHandle, McpClientPool,
    OAuthStatus, SystemMcpManifest,
};
use super::McpMiddleware;

/// server 名刻意含 `:`（与仓库内 `plugin:p1:srv1` 形态一致）：namespace
/// 解析必须经净化，不能靠裸拼接。
const PLUGIN_SERVER: &str = "plugin:p1:workspace";
/// 该 server 的净化后 namespace 前缀。
const PLUGIN_NAMESPACE: &str = "mcp__plugin_p1_workspace__";
/// 原始工具名含 `.`（净化后为 `read_file`），用于验证「配置匹配原始名」。
const PLUGIN_REQUIRED_TOOL: &str = "read.file";

/// `collect_tools` 固定追加的两个工具（非 MCP 静态 bridge）。
const APPENDED_TOOLS: [&str; 2] = ["mcp_read_resource", "DiscoverMCP"];

// ─── 夹具 ────────────────────────────────────────────────────────────────────

type SeamTransport = AsyncRwTransport<RoleClient, ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

fn seam_transport(client: DuplexStream) -> SeamTransport {
    let (read, write) = tokio::io::split(client);
    AsyncRwTransport::new(read, write)
}

/// 最小假 MCP 对端：`initialize` 成功；其余带 id 的请求（含 `server/discover`）
/// 回 Method not found，驱动 rmcp Auto lifecycle 回退 legacy initialize；通知
/// 无 id，不回响应。
fn spawn_fake_peer(server: DuplexStream) -> tokio::task::JoinHandle<()> {
    let (server_read, mut server_write) = tokio::io::split(server);
    tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let response = match request["method"].as_str() {
                Some("initialize") => serde_json::json!({
                    "jsonrpc": "2.0", "id": request["id"], "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": { "name": "mcp-seam-fixture", "version": "1" }
                    }
                }),
                _ if request["id"].is_null() => continue,
                _ => serde_json::json!({
                    "jsonrpc": "2.0", "id": request["id"],
                    "error": { "code": -32601, "message": "Method not found" }
                }),
            };
            if server_write
                .write_all(format!("{response}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
            if server_write.flush().await.is_err() {
                break;
            }
        }
    })
}

fn system_config(required_tools: Option<Vec<String>>, timeout_ms: Option<u64>) -> McpServerConfig {
    McpServerConfig {
        command: Some("mcp-seam-fixture".to_string()),
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        protocol_version: None,
        subscriptions: None,
        system_mcp: Some(true),
        system_mcp_tools: required_tools,
        system_mcp_timeout: timeout_ms,
        source: None,
    }
}

fn ordinary_config() -> McpServerConfig {
    McpServerConfig {
        system_mcp: None,
        system_mcp_tools: None,
        system_mcp_timeout: None,
        ..system_config(None, None)
    }
}

fn fixture_tool(name: &str, input_schema: serde_json::Value) -> Tool {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "description": "seam fixture",
        "inputSchema": input_schema
    }))
    .expect("fixture tool 必须能被 rmcp Tool 接收")
}

fn object_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"]
    })
}

fn connected_handle(name: &str, tools: Vec<Tool>) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        name: name.to_string(),
        version: None,
        cache_version: None,
        peer: None,
        tools,
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: None,
        url: None,
        skills_capable: false,
        channel_capable: false,
    })
}

struct SeamFixture {
    pool: Arc<McpClientPool>,
    peers: Vec<tokio::task::JoinHandle<()>>,
}

impl SeamFixture {
    fn new() -> Self {
        Self {
            pool: Arc::new(McpClientPool::new_empty()),
            peers: Vec::new(),
        }
    }

    fn middleware(&self) -> McpMiddleware {
        McpMiddleware::new(Arc::clone(&self.pool))
    }

    fn config(&self, name: &str, config: McpServerConfig) {
        self.pool.configs.write().insert(name.to_string(), config);
    }

    /// 真实握手并提交连接；返回（句柄，pool 登记代际）。
    async fn connect(&mut self, name: &str, tools: Vec<Tool>) -> (Arc<McpClientHandle>, u64) {
        let (client, server) = tokio::io::duplex(4096);
        self.peers.push(spawn_fake_peer(server));
        let service = serve_client_auto(
            seam_transport(client),
            None,
            None,
            &McpCapabilityProfile::default(),
            Duration::from_secs(5),
        )
        .await
        .expect("fixture 握手不得超时")
        .expect("fixture 握手不得失败");
        let service = self.pool.retain_service(service);
        let peer = service.peer().clone();
        let handle = Arc::new(McpClientHandle {
            name: name.to_string(),
            version: None,
            cache_version: None,
            peer: Some(peer),
            tools,
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
            channel_capable: false,
        });
        assert!(
            handle
                .peer
                .as_ref()
                .and_then(|peer| peer.peer_info())
                .is_some(),
            "fixture 必须完成真实 peer_info 协商"
        );
        assert!(
            self.pool
                .try_commit_connection(name.to_string(), Arc::clone(&handle), service)
                .is_ok(),
            "fixture 连接必须被 pool 接受"
        );
        let generation = self.pool.handle_generation(&handle);
        (handle, generation)
    }

    /// 发布配置清单并提交本代发现证据（等价于 B-02 在 initialize / reconnect /
    /// OAuth 成功路径上的提交点；`generation` 取自当前已提交句柄）。
    fn ready(&self, name: &str, generation: u64) {
        self.pool.publish_system_manifest(SystemMcpManifest::Loaded);
        self.pool
            .commit_discovery_evidence(name, DiscoveryEvidence::discovered(generation));
    }

    async fn shutdown(self) {
        self.pool.begin_shutdown();
        let _ = self.pool.shutdown().await;
        for task in self.peers {
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("假 server 必须随连接关闭退出")
                .expect("假 server 任务不得 panic");
        }
    }
}

/// 闸门状态探针：候选只经 `StartupState` 传递，不落 middleware 内部字段。
///
/// `set_active_middleware` 由 chain 在调用 hook 前写入（`peri-agent` 侧），本文件
/// 直接调用 middleware hook，故只实现为空操作。
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

// ─── 视图读取 helpers ────────────────────────────────────────────────────────

/// 切出收集视图中的静态 bridge 前缀：尾部两个是固定追加的 resource / discover 工具。
fn static_bridges(view: &[Box<dyn BaseTool>]) -> &[Box<dyn BaseTool>] {
    let split = view
        .len()
        .checked_sub(APPENDED_TOOLS.len())
        .expect("收集视图必须包含追加的 resource / discover 工具");
    let (static_tools, appended) = view.split_at(split);
    let names: Vec<&str> = appended.iter().map(|tool| tool.name()).collect();
    assert_eq!(names, APPENDED_TOOLS, "追加工具与顺序必须保持原样");
    static_tools
}

/// 收集顺序来自 pool 的 `HashMap` 遍历，**不是**契约的一部分：比较集合时先排序。
fn sorted(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names
}

fn tool_names(tools: &[Box<dyn BaseTool>]) -> Vec<String> {
    tools
        .iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>()
}

fn direct_names_of<'a>(tools: impl Iterator<Item = &'a dyn BaseTool>) -> Vec<String> {
    sorted(
        tools
            .filter(|tool| tool.is_direct())
            .map(|tool| tool.name().to_string())
            .collect(),
    )
}

fn all_deferred<'a>(tools: impl Iterator<Item = &'a dyn BaseTool>) -> bool {
    tools.into_iter().all(|tool| !tool.is_direct())
}

fn count_named(tools: &[Box<dyn BaseTool>], name: &str) -> usize {
    tools.iter().filter(|tool| tool.name() == name).count()
}

// ─── 契约 3：required 工具经所属 namespace 解析后直接进入工具视图 ─────────────

/// required 工具在**所属 server 的原始工具名**上解析，落到净化后的 namespace
/// 名，并在收集视图的 direct 集合中恰好出现一次；同 server 的其它工具与其它
/// MCP 的工具保持 deferred，且闸门候选与收集视图的 direct 集合一致。
#[tokio::test]
async fn required_tool_resolves_through_server_namespace_into_direct_view() {
    let mut fixture = SeamFixture::new();
    fixture.config(
        PLUGIN_SERVER,
        system_config(Some(vec![PLUGIN_REQUIRED_TOOL.to_string()]), None),
    );
    fixture.config("aux", ordinary_config());
    let (_, generation) = fixture
        .connect(
            PLUGIN_SERVER,
            vec![
                fixture_tool(PLUGIN_REQUIRED_TOOL, object_schema()),
                fixture_tool("glob.file", object_schema()),
            ],
        )
        .await;
    fixture
        .connect("aux", vec![fixture_tool("write.file", object_schema())])
        .await;
    fixture.ready(PLUGIN_SERVER, generation);

    let mw = fixture.middleware();
    let mut probe = StartupProbe::default();
    Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect("ready 后闸门必须放行");
    assert_eq!(probe.stage_calls, 1, "一次准入只提交一个候选");
    let update = probe.staged.expect("System 依赖就绪必须提交候选");

    let effective = format!("{PLUGIN_NAMESPACE}read_file");
    assert_eq!(
        update.required,
        vec![StartupRequiredTool {
            server_name: PLUGIN_SERVER.to_string(),
            original_tool_name: PLUGIN_REQUIRED_TOOL.to_string(),
            effective_tool_name: effective.clone(),
        }],
        "必需工具身份必须用净化后的 namespace 名，原始名单独保留"
    );
    assert_eq!(
        direct_names_of(update.tools.iter().map(|tool| tool.as_ref())),
        vec![effective.clone()],
        "候选内只有必需工具被提升 direct"
    );
    assert_eq!(
        update.tools.len(),
        3,
        "候选是整批静态工具（含非 System server 的 deferred 工具）"
    );

    let view = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    let bridges = static_bridges(&view);
    assert_eq!(bridges.len(), 3, "整批静态 bridge 各注册一次");
    for name in [
        effective.clone(),
        format!("{PLUGIN_NAMESPACE}glob_file"),
        "mcp__aux__write_file".to_string(),
    ] {
        assert_eq!(count_named(bridges, &name), 1, "{name} 必须恰好注册一次");
    }
    assert_eq!(
        direct_names_of(bridges.iter().map(|tool| tool.as_ref())),
        direct_names_of(update.tools.iter().map(|tool| tool.as_ref())),
        "闸门候选与收集视图的 direct 集合必须一致"
    );
    assert_eq!(
        sorted(tool_names(bridges)),
        vec![
            "mcp__aux__write_file".to_string(),
            format!("{PLUGIN_NAMESPACE}glob_file"),
            effective.clone(),
        ]
    );
    assert!(
        all_deferred(
            bridges
                .iter()
                .map(|tool| tool.as_ref())
                .filter(|tool| tool.name() != effective)
        ),
        "非 required 的 MCP 工具必须保持 deferred"
    );

    fixture.shutdown().await;
}

/// 配置项只在原始工具名上精确匹配：既不做净化折叠（`read_file`），也不解析
/// effective name 前缀（`mcp__…__read_file`）。两种写法都必须 fatal 且不注入 direct。
#[tokio::test]
async fn required_tool_matching_uses_original_name_not_effective_name() {
    for configured in ["read_file", "mcp__plugin_p1_workspace__read_file"] {
        let mut fixture = SeamFixture::new();
        fixture.config(
            PLUGIN_SERVER,
            system_config(Some(vec![configured.to_string()]), None),
        );
        let (_, generation) = fixture
            .connect(
                PLUGIN_SERVER,
                vec![fixture_tool(PLUGIN_REQUIRED_TOOL, object_schema())],
            )
            .await;
        fixture.ready(PLUGIN_SERVER, generation);

        let mw = fixture.middleware();
        let mut probe = StartupProbe::default();
        let error = Middleware::before_react_start(&mw, &mut probe)
            .await
            .expect_err("非原始工具名不得解析");
        assert!(
            matches!(
                &error,
                AgentError::MiddlewareError { reason, .. }
                    if reason.contains("未提供必需工具") && reason.contains(configured)
            ),
            "{configured} 期望 MissingTool 投影: {error:?}"
        );
        assert!(probe.staged.is_none(), "失败不得提交候选");

        let view = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
        let bridges = static_bridges(&view);
        assert_eq!(
            count_named(bridges, &format!("{PLUGIN_NAMESPACE}read_file")),
            1,
            "工具本身仍在集合内（只是没有 direct 提升）"
        );
        assert!(
            all_deferred(bridges.iter().map(|tool| tool.as_ref())),
            "校验失败不得留下部分 direct: {:?}",
            tool_names(bridges)
        );

        fixture.shutdown().await;
    }
}

/// 必需工具只在**所属 server** 的 namespace 内解析：另一台 server 提供的同名
/// 原始工具不得被借用，错误必须指回声明方。
#[tokio::test]
async fn required_tool_does_not_resolve_across_server_namespaces() {
    let mut fixture = SeamFixture::new();
    fixture.config(
        "alpha",
        system_config(Some(vec!["remote_only".to_string()]), None),
    );
    // beta 是 System 但只要求 ready：它提供的 remote_only 不得被 alpha 借用。
    fixture.config("beta", system_config(Some(vec![]), None));
    let (_, alpha_generation) = fixture
        .connect("alpha", vec![fixture_tool("local_only", object_schema())])
        .await;
    let (_, beta_generation) = fixture
        .connect("beta", vec![fixture_tool("remote_only", object_schema())])
        .await;
    fixture.ready("alpha", alpha_generation);
    fixture.ready("beta", beta_generation);

    let mw = fixture.middleware();
    let mut probe = StartupProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("跨 server namespace 不得命中");
    assert!(
        matches!(
            &error,
            AgentError::MiddlewareError { reason, .. }
                if reason.contains("\"alpha\"") && reason.contains("remote_only")
        ),
        "错误必须指回声明方 server: {error:?}"
    );
    assert!(probe.staged.is_none(), "失败不得提交候选");

    let view = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    let bridges = static_bridges(&view);
    assert_eq!(
        sorted(tool_names(bridges)),
        vec![
            "mcp__alpha__local_only".to_string(),
            "mcp__beta__remote_only".to_string(),
        ]
    );
    assert!(
        all_deferred(bridges.iter().map(|tool| tool.as_ref())),
        "一台失败不得让另一台留下 direct"
    );

    fixture.shutdown().await;
}

// ─── 契约 3/4：错误路径与空数组在视图层零注入 ────────────────────────────────

/// 工具缺失与 schema 结构非法两条错误路径：闸门 fatal、不提交候选，收集视图
/// 零 direct，且相关工具仍被收集（deferred，不是删除）。
#[tokio::test]
async fn missing_and_invalid_schema_required_tools_leave_zero_direct_in_view() {
    let cases = [
        (
            "工具缺失",
            vec![fixture_tool("glob.file", object_schema())],
            "未提供必需工具",
        ),
        (
            "schema 非法",
            vec![fixture_tool(
                PLUGIN_REQUIRED_TOOL,
                serde_json::json!({ "type": "object", "properties": 42 }),
            )],
            "input schema 结构非法",
        ),
    ];

    for (label, tools, expected_reason) in cases {
        let mut fixture = SeamFixture::new();
        fixture.config(
            PLUGIN_SERVER,
            system_config(Some(vec![PLUGIN_REQUIRED_TOOL.to_string()]), None),
        );
        let (_, generation) = fixture.connect(PLUGIN_SERVER, tools).await;
        fixture.ready(PLUGIN_SERVER, generation);

        let mw = fixture.middleware();
        let mut probe = StartupProbe::default();
        let error = Middleware::before_react_start(&mw, &mut probe)
            .await
            .expect_err("错误路径必须阻止启动");
        assert!(
            !matches!(error, AgentError::Interrupted),
            "{label}: 校验失败不是取消"
        );
        assert!(
            matches!(
                &error,
                AgentError::MiddlewareError { reason, .. } if reason.contains(expected_reason)
            ),
            "{label} 期望固定文案: {error:?}"
        );
        assert!(probe.staged.is_none(), "{label}: 失败不得提交候选");

        let view = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
        let bridges = static_bridges(&view);
        assert_eq!(bridges.len(), 1, "{label}: 工具本身不得被删除");
        assert!(
            all_deferred(bridges.iter().map(|tool| tool.as_ref())),
            "{label}: 不得留下部分 direct"
        );

        fixture.shutdown().await;
    }
}

/// 契约 4：`system_mcp_tools: []` 只要求 ready，不注入额外工具 —— 候选与收集
/// 视图的 direct 增量都为 0，该 server 的普通工具仍被收集且保持 deferred。
#[tokio::test]
async fn empty_required_array_injects_zero_direct_tools() {
    let mut fixture = SeamFixture::new();
    fixture.config("sys", system_config(Some(vec![]), None));
    let (_, generation) = fixture
        .connect(
            "sys",
            vec![
                fixture_tool(PLUGIN_REQUIRED_TOOL, object_schema()),
                fixture_tool("glob.file", object_schema()),
            ],
        )
        .await;
    fixture.ready("sys", generation);

    let mw = fixture.middleware();
    let mut probe = StartupProbe::default();
    Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect("空数组只验证 ready，不阻塞启动");
    let update = probe.staged.expect("System 依赖就绪必须提交候选");
    assert!(update.required.is_empty(), "空数组不得产生必需工具身份");
    assert_eq!(update.tools.len(), 2, "整批静态工具仍必须发布");
    assert!(
        all_deferred(update.tools.iter().map(|tool| tool.as_ref())),
        "空数组不得提升任何 direct 工具"
    );

    let view = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    let bridges = static_bridges(&view);
    assert_eq!(
        sorted(tool_names(bridges)),
        vec![
            "mcp__sys__glob_file".to_string(),
            "mcp__sys__read_file".to_string(),
        ],
        "零注入不等于删除该 MCP 的工具"
    );
    assert!(
        all_deferred(bridges.iter().map(|tool| tool.as_ref())),
        "收集视图的 direct 集合必须为空"
    );

    fixture.shutdown().await;
}

/// 边界面：配置清单未发布（Pending）时 `configs` 不是可信的 System 依赖事实源，
/// 即使句柄已 `Connected` 且有工具声明，也不得提升 direct。
#[test]
fn pending_manifest_never_promotes_direct_tools_in_view() {
    let pool = Arc::new(McpClientPool::new_empty());
    pool.configs.write().insert(
        PLUGIN_SERVER.to_string(),
        system_config(Some(vec![PLUGIN_REQUIRED_TOOL.to_string()]), None),
    );
    pool.clients.write().insert(
        PLUGIN_SERVER.to_string(),
        connected_handle(
            PLUGIN_SERVER,
            vec![fixture_tool(PLUGIN_REQUIRED_TOOL, object_schema())],
        ),
    );

    let mw = McpMiddleware::new(Arc::clone(&pool));
    let view = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    let bridges = static_bridges(&view);
    assert_eq!(
        sorted(tool_names(bridges)),
        vec![format!("{PLUGIN_NAMESPACE}read_file")],
        "deferred bridge 仍应被收集"
    );
    assert!(
        all_deferred(bridges.iter().map(|tool| tool.as_ref())),
        "清单未发布不得提升 direct"
    );
}
