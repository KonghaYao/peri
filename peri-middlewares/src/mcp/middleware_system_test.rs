use super::*;

// ─── System MCP 启动闸门（before_react_start，IF-M3 / IF-M4）──────────────────
//
// 夹具策略：只把外部对端换成内存 JSON-RPC 假 server，客户端侧走真实
// `serve_client_auto`（真实 rmcp lifecycle / peer_info / transport 关闭语义）；
// 配置清单与本代发现证据由测试显式发布，等价于 B-02 在 initialize / reconnect /
// OAuth 路径上的提交点。既有 `peer: None` + 手工 `Connected` 的夹具在闸门用例里
// **不构成 ready 证据**，只用于负向断言。
//
// 断言范围：本文件覆盖 crate 内可观察层（闸门返回值、候选、bridge 分类、收集
// 结果）。首个 LLM 请求的 tools 入参与宿主终态由 B-07 在 `peri-acp` host seam
// 承担（主 plan §5 R9）。

use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::client::DiscoveryEvidence;
use peri_acp_types::plugin::McpServerConfig;
use rmcp::{service::RoleClient, transport::async_rw::AsyncRwTransport};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

/// 闸门状态探针：候选只经 `StartupState` 传递，不落 middleware 内部字段。
#[derive(Default)]
struct StartupGateProbe {
    staged: Option<StartupToolUpdate>,
}

impl hook_state::StartupState for StartupGateProbe {
    fn set_active_middleware(&mut self, _middleware_name: &str) {}

    fn stage_startup_tools(
        &mut self,
        update: StartupToolUpdate,
    ) -> peri_agent::error::AgentResult<()> {
        assert!(self.staged.is_none(), "一次准入只允许登记一个候选");
        self.staged = Some(update);
        Ok(())
    }

    fn take_startup_tools(&mut self) -> Option<StartupToolUpdate> {
        self.staged.take()
    }
}

type GateTransport = AsyncRwTransport<RoleClient, ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

fn gate_transport(client: DuplexStream) -> GateTransport {
    let (read, write) = tokio::io::split(client);
    AsyncRwTransport::new(read, write)
}

/// 假 MCP 对端：initialize 成功；其余请求（含 `server/discover`）回
/// Method not found，驱动 Auto 生命周期回退 legacy initialize。通知无 id，不回响应。
fn spawn_gate_peer(server: DuplexStream) -> tokio::task::JoinHandle<()> {
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
                        "serverInfo": { "name": "mcp-gate-fixture", "version": "1" }
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
        task_scope_secret_file: None,
        command: Some("mcp-gate-fixture".to_string()),
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
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

fn fixture_tool(name: &str, input_schema: serde_json::Value) -> rmcp::model::Tool {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "description": "fixture",
        "inputSchema": input_schema
    }))
    .expect("fixture tool 必须能被 rmcp Tool 接收")
}

fn read_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"]
    })
}

struct GateFixture {
    pool: Arc<McpClientPool>,
    servers: Vec<tokio::task::JoinHandle<()>>,
}

impl GateFixture {
    fn new() -> Self {
        Self {
            pool: Arc::new(McpClientPool::new_pending()),
            servers: Vec::new(),
        }
    }

    fn pool(&self) -> &Arc<McpClientPool> {
        &self.pool
    }

    fn config(&self, name: &str, config: McpServerConfig) {
        self.pool.configs.write().insert(name.to_string(), config);
    }

    fn publish_loaded(&self) {
        self.pool.publish_system_manifest(SystemMcpManifest::Loaded);
    }

    /// 真实握手并提交连接；返回（句柄，已登记代际）。
    async fn connect(
        &mut self,
        name: &str,
        tools: Vec<rmcp::model::Tool>,
    ) -> (Arc<McpClientHandle>, u64) {
        let (client, server) = tokio::io::duplex(4096);
        self.servers.push(spawn_gate_peer(server));
        let service = crate::mcp::client::serve_client_auto(
            gate_transport(client),
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
            source: self.pool.configs.read().get(name).and_then(|config| config.source.clone()),
            url: None,
            skills_capable: false,
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

    /// 使某台 server 成为「本代发现完成」：清单已完整发布 + 本代成功证据。
    fn ready(&self, name: &str, generation: u64) {
        self.publish_loaded();
        self.pool
            .commit_discovery_evidence(name, DiscoveryEvidence::discovered(generation));
    }

    async fn shutdown(self) {
        self.pool.begin_shutdown();
        let _ = self.pool.shutdown().await;
        for task in self.servers {
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("假 server 必须随连接关闭退出")
                .expect("假 server 任务不得 panic");
        }
    }
}

fn bridge_names(tools: &[std::sync::Arc<dyn BaseTool>]) -> Vec<(String, bool)> {
    let mut names: Vec<(String, bool)> = tools
        .iter()
        .map(|tool| (tool.name().to_string(), tool.is_direct()))
        .collect();
    names.sort();
    names
}

/// 就绪后闸门放行并暂存**整批**静态 bridge：必需项 direct、其余 deferred，
/// 且候选只取自 deployment `tool_pool`（session projection 的伪 Connected 不参与）。
#[tokio::test]
async fn system_mcp_ready_stages_candidate_with_direct_required_tool() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    let (_, generation) = fixture
        .connect(
            "sys",
            vec![
                fixture_tool("Read", read_schema()),
                fixture_tool("Glob", read_schema()),
            ],
        )
        .await;
    fixture.ready("sys", generation);

    let projection = Arc::new(McpClientPool::new_empty());
    projection.clients.write().insert(
        "dyn".to_string(),
        make_connected_handle_with_tool("dyn", "shadow"),
    );
    let mw = McpMiddleware::new(Arc::clone(&projection)).with_tool_pool(Arc::clone(fixture.pool()));

    let mut probe = StartupGateProbe::default();
    Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect("ready 后闸门必须放行");

    let update = probe.staged.expect("System 依赖就绪必须暂存候选");
    assert_eq!(
        bridge_names(&update.tools),
        vec![
            ("Read".to_string(), true),
            ("mcp__sys__Glob".to_string(), false),
        ],
        "整批静态 bridge：仅必需项 direct，投影池工具不得进入候选"
    );
    assert_eq!(
        update.required,
        vec![StartupRequiredTool {
            server_name: "sys".to_string(),
            original_tool_name: "Read".to_string(),
            effective_tool_name: "Read".to_string(),
        }]
    );

    fixture.shutdown().await;
}

/// 必需工具缺失：闸门 fatal，不暂存候选（不发布 ready、不注入部分 direct）。
#[tokio::test]
async fn system_mcp_missing_required_tool_blocks_startup() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    let (_, generation) = fixture
        .connect("sys", vec![fixture_tool("Write", read_schema())])
        .await;
    fixture.ready("sys", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let mut probe = StartupGateProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("缺必需工具必须阻止启动");

    match error {
        peri_agent::error::AgentError::MiddlewareError {
            ref middleware,
            ref reason,
        } => {
            assert_eq!(middleware, "McpMiddleware");
            assert!(
                reason.contains("未提供必需工具") && reason.contains("Read"),
                "错误需明确工具类别: {reason}"
            );
        }
        other => panic!("必须是 fatal MiddlewareError，实际 {other:?}"),
    }
    assert!(probe.staged.is_none(), "失败不得暂存候选");

    fixture.shutdown().await;
}

/// 必需工具 schema 结构非法：闸门 fatal，不暂存候选。
#[tokio::test]
async fn system_mcp_invalid_schema_blocks_startup() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    let (_, generation) = fixture
        .connect(
            "sys",
            vec![fixture_tool(
                "Read",
                serde_json::json!({ "type": "object", "properties": 42 }),
            )],
        )
        .await;
    fixture.ready("sys", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let mut probe = StartupGateProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("非法 schema 必须阻止启动");

    assert!(
        matches!(
            &error,
            peri_agent::error::AgentError::MiddlewareError { reason, .. }
                if reason.contains("input schema 结构非法")
        ),
        "期望 InvalidSchema 投影: {error:?}"
    );
    assert!(probe.staged.is_none(), "失败不得暂存候选");

    fixture.shutdown().await;
}

/// 无 System 依赖（缺省 / 显式 false）：零动作、不等待、不产生 startup update。
#[tokio::test]
async fn system_mcp_absent_or_false_does_not_block_startup() {
    for config in [
        ordinary_config(),
        McpServerConfig {
            system_mcp: Some(false),
            ..system_config(None, None)
        },
    ] {
        let fixture = GateFixture::new();
        fixture.config("plain", config);
        fixture.publish_loaded();

        let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
        let mut probe = StartupGateProbe::default();
        Middleware::before_react_start(&mw, &mut probe)
            .await
            .expect("普通 MCP 的 pending/failed 永不阻塞启动");
        assert!(
            probe.staged.is_none(),
            "无 System 依赖不产生 startup update"
        );

        fixture.shutdown().await;
    }
}

/// 取消 → `Interrupted`（不是 fatal）；不暂存候选。
#[tokio::test]
async fn system_mcp_cancelled_startup_is_interrupted() {
    let fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    fixture.publish_loaded();
    let cancel = AgentCancellationToken::new();
    cancel.cancel();
    let mw = McpMiddleware::new(Arc::clone(fixture.pool())).with_skill_discovery(None, cancel);

    let mut probe = StartupGateProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("取消必须中断本次启动");

    assert!(
        matches!(error, peri_agent::error::AgentError::Interrupted),
        "Cancelled 必须映射 Interrupted: {error:?}"
    );
    assert!(probe.staged.is_none());

    fixture.shutdown().await;
}

/// timeout **不是**取消：映射 fatal `MiddlewareError`，不映射 `Interrupted`。
#[tokio::test]
async fn system_mcp_timeout_is_fatal() {
    let fixture = GateFixture::new();
    fixture.config(
        "sys",
        system_config(Some(vec!["Read".to_string()]), Some(1)),
    );
    fixture.publish_loaded();
    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));

    let mut probe = StartupGateProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("超时必须阻止启动");

    assert!(
        !matches!(error, peri_agent::error::AgentError::Interrupted),
        "timeout 不得映射 Interrupted"
    );
    assert!(
        matches!(
            &error,
            peri_agent::error::AgentError::MiddlewareError { reason, .. }
                if reason.contains("启动超时（1ms）")
        ),
        "期望超时固定文案: {error:?}"
    );

    fixture.shutdown().await;
}

/// 准入事实源是 deployment `tool_pool`：session projection 的连接证据不得放行。
///
/// 投影池刻意用**同名** server 且伪 `Connected`：若闸门误读投影池，得到的会是
/// `NegotiationIncomplete`（无真实协议证据）而不是 deployment 侧的 Timeout。
#[tokio::test]
async fn system_mcp_gate_uses_deployment_pool_not_session_projection() {
    let fixture = GateFixture::new();
    fixture.config(
        "sys",
        system_config(Some(vec!["Read".to_string()]), Some(1)),
    );
    fixture.publish_loaded();

    let projection = Arc::new(McpClientPool::new_empty());
    projection.clients.write().insert(
        "sys".to_string(),
        make_connected_handle_with_tool("sys", "Read"),
    );
    let mw = McpMiddleware::new(Arc::clone(&projection)).with_tool_pool(Arc::clone(fixture.pool()));

    let mut probe = StartupGateProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("投影池的伪 Connected 不得作为准入证据");

    assert!(
        matches!(
            &error,
            peri_agent::error::AgentError::MiddlewareError { reason, .. }
                if reason.contains("启动超时（1ms）")
        ),
        "必须按 deployment 侧事实判定（无连接 → 超时）: {error:?}"
    );
    assert!(probe.staged.is_none());

    fixture.shutdown().await;
}

/// 错误文案安全：控制字符折叠为空格、凭据形态遮蔽（危险形态只用非真实凭据形状）。
#[tokio::test]
async fn startup_error_text_folds_control_chars_and_redacts_credentials() {
    let mut fixture = GateFixture::new();
    fixture.config(
        "sys",
        system_config(
            Some(vec!["Re\u{7}ad?token=FAKE-SHAPE-ONLY".to_string()]),
            None,
        ),
    );
    let (_, generation) = fixture
        .connect("sys", vec![fixture_tool("Read", read_schema())])
        .await;
    fixture.ready("sys", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let mut probe = StartupGateProbe::default();
    let error = Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("必需工具缺失");

    let peri_agent::error::AgentError::MiddlewareError { reason, .. } = &error else {
        panic!("必须是 MiddlewareError: {error:?}");
    };
    assert!(
        !reason.chars().any(char::is_control),
        "控制字符必须折叠: {reason}"
    );
    assert!(
        !reason.contains("FAKE-SHAPE-ONLY"),
        "凭据形态必须遮蔽: {reason}"
    );
    assert!(
        reason.contains("未提供必需工具"),
        "错误类别仍需可见: {reason}"
    );

    fixture.shutdown().await;
}

// ─── collect_tools 的整批替换与 deferred 回退 ────────────────────────────────

/// 就绪后收集：prepared 整批**替换**初始 Vec，必需项 direct 且只注册一次；
/// resource / discover 仍原样追加。
#[tokio::test]
async fn collect_tools_replaces_initial_bridges_without_duplicate_registration() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    fixture.config("aux", ordinary_config());
    let (_, generation) = fixture
        .connect(
            "sys",
            vec![
                fixture_tool("Read", read_schema()),
                fixture_tool("Glob", read_schema()),
            ],
        )
        .await;
    fixture
        .connect("aux", vec![fixture_tool("Write", read_schema())])
        .await;
    fixture.ready("sys", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let collected = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    let names: Vec<String> = collected
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();

    assert_eq!(
        &names[names.len() - 2..],
        ["mcp_read_resource", "DiscoverMCP"],
        "resource / discover 仍原样追加: {names:?}"
    );
    let bridges = &collected[..names.len() - 2];
    assert_eq!(bridges.len(), 3, "整批静态 bridge 恰好一次: {names:?}");
    for name in ["Read", "mcp__sys__Glob", "mcp__aux__Write"] {
        assert_eq!(
            bridges.iter().filter(|tool| tool.name() == name).count(),
            1,
            "{name} 不得重复注册: {names:?}"
        );
    }
    let read = bridges
        .iter()
        .find(|tool| tool.name() == "Read")
        .expect("必需 bridge 必须在集合内");
    assert!(read.is_direct(), "必需项 direct");
    assert!(
        bridges
            .iter()
            .filter(|tool| tool.name() != "Read")
            .all(|tool| !tool.is_direct()),
        "非必需项保持 deferred"
    );

    fixture.shutdown().await;
}

/// 配置清单未发布（Pending）：`configs` 不是可信依赖事实源，不得提升 direct。
#[tokio::test]
async fn collect_tools_keeps_deferred_bridges_until_manifest_loaded() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    fixture
        .connect("sys", vec![fixture_tool("Read", read_schema())])
        .await;

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let collected = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    let read = collected
        .iter()
        .find(|tool| tool.name() == "mcp__sys__Read")
        .expect("deferred bridge 仍应存在");
    assert!(!read.is_direct(), "清单未发布不得提升 direct");
    assert_eq!(collected.len(), 3, "1 static bridge + resource + discover");

    fixture.shutdown().await;
}

/// 已 ready 但必需工具缺失：不抢先收集该 system server 的部分工具，
/// 闸门仍会在进入 Compact 前 fatal。
#[tokio::test]
async fn collect_tools_falls_back_to_deferred_when_required_tool_missing() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    let (_, generation) = fixture
        .connect("sys", vec![fixture_tool("Write", read_schema())])
        .await;
    fixture.ready("sys", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let collected = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    assert!(collected
        .iter()
        .all(|tool| tool.mcp_server_name() != Some("sys")));
    assert_eq!(collected.len(), 2, "只剩 resource + discover");

    fixture.shutdown().await;
}

#[tokio::test]
async fn collect_tools_waits_for_complete_system_batch_before_name_admission() {
    let mut fixture = GateFixture::new();
    fixture.config("alpha", system_config(Some(vec!["Read".into()]), None));
    fixture.config("zulu", system_config(Some(vec!["Read".into()]), None));
    let (_, zulu_generation) = fixture
        .connect("zulu", vec![fixture_tool("Read", read_schema())])
        .await;
    fixture.ready("zulu", zulu_generation);
    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let partial = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    assert!(partial.iter().all(|tool| tool.mcp_server_name().is_none()));

    let (_, alpha_generation) = fixture
        .connect("alpha", vec![fixture_tool("Read", read_schema())])
        .await;
    fixture.ready("alpha", alpha_generation);
    let complete = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    let system_names: Vec<_> = complete
        .iter()
        .filter_map(|tool| {
            tool.mcp_server_name()
                .map(|server| (server.to_string(), tool.name().to_string()))
        })
        .collect();
    assert_eq!(
        system_names,
        vec![
            ("alpha".into(), "Read".into()),
            ("zulu".into(), "Read".into())
        ]
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn collect_tools_waits_for_zero_tool_system_server_discovery() {
    let mut fixture = GateFixture::new();
    fixture.config("alpha", system_config(Some(vec![]), None));
    fixture.config("zulu", system_config(Some(vec!["Read".into()]), None));
    let (_, zulu_generation) = fixture
        .connect("zulu", vec![fixture_tool("Read", read_schema())])
        .await;
    fixture.ready("zulu", zulu_generation);
    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));
    let partial = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    assert!(partial.iter().all(|tool| tool.mcp_server_name().is_none()));

    let (_, alpha_generation) = fixture.connect("alpha", vec![]).await;
    fixture.ready("alpha", alpha_generation);
    let complete = <McpMiddleware as Middleware>::collect_tools(&mw, "/tmp");
    assert!(complete.iter().any(|tool| tool.name() == "Read"));
    fixture.shutdown().await;
}

/// 闸门通过不改变既有 discovery 与状态通知行为：既不消费状态变化缓冲，
/// 也不自行触发发现；`before_agent` 仍按原语义触发（幂等增量挂点保留）。
#[tokio::test]
async fn successful_gate_preserves_discovery_and_status_notifications() {
    let pool = Arc::new(McpClientPool::new_empty());
    insert_skill_handle(
        &pool,
        "srv",
        vec![Resource::new("skill://demo/SKILL.md", "d")],
    );
    insert_skill_handle(&pool, "status", vec![]);
    pool.publish_system_manifest(SystemMcpManifest::Loaded);
    let reg = Arc::new(McpSkillRegistry::new());
    let mw = McpMiddleware::new(Arc::clone(&pool))
        .with_skill_discovery(Some(Arc::clone(&reg)), AgentCancellationToken::new());

    pool.mark_initialized();
    pool.record_status_change("status", Some(&ClientStatus::Connected));
    if let Some(handle) = pool.clients.write().get_mut("status") {
        Arc::make_mut(handle).status = ClientStatus::Failed("boom".to_string());
    }
    pool.record_status_change("status", Some(&ClientStatus::Connected));

    let mut probe = StartupGateProbe::default();
    Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect("无 System 依赖的闸门必须放行");
    assert!(
        probe.staged.is_none(),
        "无 System 依赖不产生 startup update"
    );
    assert!(reg.discovery_state("srv").is_none(), "闸门自身不得触发发现");
    assert_eq!(
        pool.drain_pending_changes().len(),
        1,
        "闸门不得消费状态变化缓冲"
    );

    let mut state = AgentState::new("/tmp");
    Middleware::before_agent(&mw, &mut state).await.unwrap();
    assert!(
        matches!(
            reg.discovery_state("srv"),
            Some(ServerDiscoveryState::Started { .. })
        ),
        "既有 before_agent 发现行为保留"
    );
}

/// 失败不留下可复用的半成品：同一 middleware 连续两次准入，第二次按当次句柄
/// 重新构建（不把上次的 direct 标记套到新代，也不复用失败的候选）。
#[tokio::test]
async fn failed_gate_leaves_no_reusable_candidate() {
    let mut fixture = GateFixture::new();
    fixture.config("sys", system_config(Some(vec!["Read".to_string()]), None));
    let (_, generation) = fixture
        .connect("sys", vec![fixture_tool("Write", read_schema())])
        .await;
    fixture.ready("sys", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()));

    let mut probe = StartupGateProbe::default();
    Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect_err("首次准入缺必需工具");
    assert!(probe.staged.is_none(), "失败不得留下候选");

    // 第二代句柄补齐必需工具：证据必须重新按新代提交。
    let (_, generation) = fixture
        .connect(
            "sys",
            vec![
                fixture_tool("Read", read_schema()),
                fixture_tool("Write", read_schema()),
            ],
        )
        .await;
    fixture.ready("sys", generation);

    Middleware::before_react_start(&mw, &mut probe)
        .await
        .expect("第二代就绪后必须放行");
    let update = probe.staged.expect("成功准入必须暂存候选");
    assert_eq!(
        bridge_names(&update.tools),
        vec![
            ("Read".to_string(), true),
            ("mcp__sys__Write".to_string(), false),
        ],
        "候选按当次句柄重建，不残留失败批次"
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn remote_workspace_uses_discovered_tools_even_with_builtin_workspace_closed() {
    let mut fixture = GateFixture::new();
    let mut config = system_config(None, None);
    config.source = Some(crate::mcp::config::ConfigSource::WorkspaceRemote);
    fixture.config("workspace", config);
    let (_, generation) = fixture
        .connect("workspace", vec![fixture_tool("RemoteOnly", read_schema())])
        .await;
    fixture.ready("workspace", generation);

    let mw = McpMiddleware::new(Arc::clone(fixture.pool()))
        .with_builtin_closures(BTreeSet::from(["workspace".to_string()]));
    let snapshot = mw.await_system_ready().await.unwrap();
    assert_eq!(snapshot.bridges.len(), 1);
    assert_eq!(snapshot.bridges[0].name(), "RemoteOnly");
    assert!(snapshot.bridges[0].is_direct());
    assert!(snapshot.required_tools().unwrap().is_empty());

    let (_, generation) = fixture.connect("workspace", vec![]).await;
    fixture.ready("workspace", generation);
    let snapshot = mw.await_system_ready().await.unwrap();
    assert!(snapshot.bridges.is_empty(), "empty live tools/list is valid");
    fixture.shutdown().await;
}
