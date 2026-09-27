//! Tests for System MCP 启动准入（IF-M3）
//!
//! 夹具策略（testing.md / sub-plan B §6.1）：只把**外部对端**换成内存 JSON-RPC
//! 假 server，客户端侧走真实 `serve_client_auto`（真实 rmcp lifecycle、真实
//! peer_info、真实 transport 关闭语义）；发现证据由测试显式提交，等价于 B-02
//! 在 initialize / reconnect / OAuth 路径上的提交点。每个用例按
//! pool begin-close → shutdown → join 假 server 收尾，不留 orphan task。

use super::super::serve_client_auto;
use super::*;
use crate::mcp::apps::McpCapabilityProfile;
use peri_agent::error::AgentError;
use rmcp::{service::RoleClient, transport::async_rw::AsyncRwTransport};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf},
    task::JoinHandle,
};

// ─── fixtures ─────────────────────────────────────────────────────────────────

enum FakeInitialize {
    /// 回退 legacy initialize 并返回成功结果（Auto 生命周期先试 `server/discover`）。
    Legacy,
    /// initialize 回 JSON-RPC error：真实 SDK 握手失败。
    Error,
}

/// 假 MCP 对端：`server/discover` 一律失败，initialize 按参数返回。
fn spawn_fake_peer(server: DuplexStream, initialize: FakeInitialize) -> JoinHandle<()> {
    let (server_read, mut server_write) = tokio::io::split(server);
    tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: serde_json::Value = match serde_json::from_str(&line) {
                Ok(request) => request,
                Err(_) => continue,
            };
            let response = match request["method"].as_str() {
                Some("server/discover") => serde_json::json!({
                    "jsonrpc": "2.0", "id": request["id"],
                    "error": { "code": -32601, "message": "Method not found" }
                }),
                Some("initialize") => match initialize {
                    FakeInitialize::Legacy => serde_json::json!({
                        "jsonrpc": "2.0", "id": request["id"], "result": {
                            "protocolVersion": "2025-11-25",
                            "capabilities": {},
                            "serverInfo": { "name": "readiness-fixture", "version": "1" }
                        }
                    }),
                    FakeInitialize::Error => serde_json::json!({
                        "jsonrpc": "2.0", "id": request["id"],
                        "error": { "code": -32603, "message": "fixture initialize failure" }
                    }),
                },
                _ => continue,
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

type FixtureTransport =
    AsyncRwTransport<RoleClient, ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

fn fixture_transport(client: DuplexStream) -> FixtureTransport {
    let (read, write) = tokio::io::split(client);
    AsyncRwTransport::new(read, write)
}

fn system_config(required_tools: Option<Vec<String>>, timeout_ms: Option<u64>) -> McpServerConfig {
    McpServerConfig {
        command: Some("readiness-fixture".to_string()),
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

struct SystemFixture {
    pool: Arc<McpClientPool>,
    servers: Vec<JoinHandle<()>>,
}

impl SystemFixture {
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

    fn commit_evidence(&self, name: &str, evidence: DiscoveryEvidence) {
        self.pool.commit_discovery_evidence(name, evidence);
    }

    fn handle(name: &str, peer: Option<rmcp::service::Peer<RoleClient>>) -> Arc<McpClientHandle> {
        Arc::new(McpClientHandle {
            name: name.to_string(),
            version: None,
            cache_version: None,
            peer,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            skills_capable: false,
            channel_capable: false,
        })
    }

    /// 真实握手 + 提交连接；返回 (句柄, 已登记代际)。
    async fn connect(&mut self, name: &str) -> (Arc<McpClientHandle>, u64) {
        let (client, server) = tokio::io::duplex(4096);
        self.servers
            .push(spawn_fake_peer(server, FakeInitialize::Legacy));
        let service = serve_client_auto(
            fixture_transport(client),
            None,
            None,
            &McpCapabilityProfile::default(),
            Duration::from_secs(5),
        )
        .await
        .expect("fixture 握手不允许超时")
        .expect("fixture 握手不允许失败");
        let service = self.pool.retain_service(service);
        let peer = service.peer().clone();
        let handle = Self::handle(name, Some(peer));
        assert!(
            handle
                .peer
                .as_ref()
                .and_then(|peer| peer.peer_info())
                .is_some(),
            "fixture 必须完成真实 peer_info 协商"
        );
        let committed =
            self.pool
                .try_commit_connection(name.to_string(), Arc::clone(&handle), service);
        assert!(committed.is_ok(), "fixture 连接必须被 pool 接受");
        let generation = self.pool.handle_generation(&handle);
        assert_ne!(generation, 0, "fixture 连接必须有登记代际");
        (handle, generation)
    }

    /// 真实 initialize 失败：假 server 回 JSON-RPC error，随后按生产同款收口
    /// API 提交失败事实（`insert_failed` + 失败证据）。不读底层错误文案。
    async fn fail_initialize(&mut self, name: &str) {
        let (client, server) = tokio::io::duplex(4096);
        self.servers
            .push(spawn_fake_peer(server, FakeInitialize::Error));
        let outcome = serve_client_auto(
            fixture_transport(client),
            None,
            None,
            &McpCapabilityProfile::default(),
            Duration::from_secs(5),
        )
        .await;
        assert!(
            matches!(outcome, Ok(Err(_)) | Err(_)),
            "fixture initialize 必须真实失败"
        );
        McpClientPool::insert_failed(self.pool(), name, "fixture: initialize failed".to_string());
        let handle = self.pool.get_client(name).expect("失败句柄必须落表");
        let generation = self.pool.handle_generation(&handle);
        self.commit_evidence(name, DiscoveryEvidence::initialize_failed(generation));
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
        assert!(!self.pool.is_open());
    }
}

/// 断言在给定时限内**不返回** ready：未就绪状态只能停在等待。
async fn assert_pending(
    pool: &Arc<McpClientPool>,
    cancel: &AgentCancellationToken,
    started_at: tokio::time::Instant,
) {
    let outcome = tokio::time::timeout(
        Duration::from_millis(50),
        pool.await_system_connections(cancel, started_at),
    )
    .await;
    assert!(outcome.is_err(), "未就绪状态不得返回 ready: {outcome:?}");
}

/// 有界等待成功：1s 内必须返回 negotiated。
async fn await_ready(
    pool: &Arc<McpClientPool>,
    cancel: &AgentCancellationToken,
    started_at: tokio::time::Instant,
) -> Vec<NegotiatedSystemMcp> {
    tokio::time::timeout(
        Duration::from_secs(1),
        pool.await_system_connections(cancel, started_at),
    )
    .await
    .expect("ready 路径不得超时")
    .expect("ready 路径不得返回错误")
}

/// 有界等待失败：1s 内必须返回类型化错误（证明不是睡到 deadline）。
async fn await_error(
    pool: &Arc<McpClientPool>,
    cancel: &AgentCancellationToken,
    started_at: tokio::time::Instant,
) -> SystemReadinessError {
    tokio::time::timeout(
        Duration::from_secs(1),
        pool.await_system_connections(cancel, started_at),
    )
    .await
    .expect("已知失败必须立即返回，不得等待到 deadline")
    .expect_err("该场景不得返回 ready")
}

// ─── 等待与证据 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn system_ready_waits_for_initialize_and_tools_list() {
    let mut fixture = SystemFixture::new();
    fixture.config("sys", system_config(Some(vec![]), Some(5_000)));
    fixture.publish_loaded();
    let cancel = AgentCancellationToken::new();
    let started_at = tokio::time::Instant::now();

    // 阶段 1：transport / initialize 尚未完成（无句柄）→ 不返回 ready。
    assert_pending(fixture.pool(), &cancel, started_at).await;

    // 阶段 2：真实协商已成功（Connected + peer_info），但 live tools/list 尚未
    // 结束（生产路径此时不提交证据）→ 仍不返回 ready。
    let (_handle, generation) = fixture.connect("sys").await;
    assert_pending(fixture.pool(), &cancel, started_at).await;

    // 阶段 3：成功路径提交本代发现证据（必需工具为空数组也是成功结果）。
    fixture.commit_evidence("sys", DiscoveryEvidence::discovered(generation));
    let negotiated = await_ready(fixture.pool(), &cancel, started_at).await;
    assert_eq!(negotiated.len(), 1);
    assert_eq!(negotiated[0].requirement.server, "sys");
    assert_eq!(
        negotiated[0].requirement.required_tools,
        Vec::<String>::new()
    );
    assert_eq!(negotiated[0].generation, generation);
    assert!(negotiated[0].handle.tools.is_empty());

    fixture.shutdown().await;
}

#[tokio::test]
async fn system_ready_rejects_initialize_error() {
    let mut fixture = SystemFixture::new();
    fixture.config("sys", system_config(None, Some(5_000)));
    fixture.publish_loaded();
    fixture.fail_initialize("sys").await;

    let cancel = AgentCancellationToken::new();
    let error = await_error(fixture.pool(), &cancel, tokio::time::Instant::now()).await;
    assert_eq!(
        error,
        SystemReadinessError::ConnectionFailed {
            server: "sys".to_string()
        }
    );
    // 失败文案不含底层原因原文，也不含任何传输细节。
    assert!(!error.to_string().contains("fixture"));

    fixture.shutdown().await;
}

#[tokio::test]
async fn system_ready_rejects_tools_list_error_instead_of_empty() {
    let mut fixture = SystemFixture::new();
    // 必需工具显式为空数组：list 失败仍然必须是失败，不是「空清单 ready」。
    fixture.config("sys", system_config(Some(vec![]), Some(5_000)));
    fixture.publish_loaded();
    let (_handle, generation) = fixture.connect("sys").await;
    fixture.commit_evidence("sys", DiscoveryEvidence::discovery_failed(generation));

    let cancel = AgentCancellationToken::new();
    let error = await_error(fixture.pool(), &cancel, tokio::time::Instant::now()).await;
    assert_eq!(
        error,
        SystemReadinessError::ToolDiscoveryFailed {
            server: "sys".to_string()
        }
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn system_ready_rejects_stale_generation_evidence() {
    let mut fixture = SystemFixture::new();
    fixture.config("sys", system_config(None, Some(5_000)));
    fixture.publish_loaded();
    let (_handle, generation) = fixture.connect("sys").await;
    let cancel = AgentCancellationToken::new();
    let started_at = tokio::time::Instant::now();

    // 旧代 / 非本代证据（含 generation 不匹配）不得被接受。
    fixture.commit_evidence("sys", DiscoveryEvidence::discovered(generation + 41));
    assert_pending(fixture.pool(), &cancel, started_at).await;

    fixture.commit_evidence("sys", DiscoveryEvidence::discovered(generation));
    let negotiated = await_ready(fixture.pool(), &cancel, started_at).await;
    assert_eq!(negotiated[0].generation, generation);

    fixture.shutdown().await;
}

#[tokio::test]
async fn system_ready_rejects_connected_without_protocol_evidence() {
    let fixture = SystemFixture::new();
    fixture.config("sys", system_config(None, Some(5_000)));
    fixture.publish_loaded();

    // 旧式手工 Connected fixture：peer=None。即便"补齐"完整证据与登记代际，
    // 无 peer / peer_info / 已提交 service 就不是协商完成，也不能当连接中等待。
    let handle = SystemFixture::handle("sys", None);
    fixture.pool.advance_handle_generation(&handle);
    let generation = fixture.pool.handle_generation(&handle);
    assert_ne!(generation, 0);
    fixture
        .pool
        .clients
        .write()
        .insert("sys".to_string(), handle);
    fixture.commit_evidence("sys", DiscoveryEvidence::discovered(generation));

    let cancel = AgentCancellationToken::new();
    let error = await_error(fixture.pool(), &cancel, tokio::time::Instant::now()).await;
    assert_eq!(
        error,
        SystemReadinessError::NegotiationIncomplete {
            server: "sys".to_string()
        }
    );

    fixture.shutdown().await;
}

// ─── 清单与范围 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn system_ready_ignores_non_system_pending_and_failed() {
    let mut fixture = SystemFixture::new();
    fixture.config("sys", system_config(None, Some(5_000)));
    fixture.config("ordinary-pending", ordinary_config());
    fixture.config("ordinary-failed", ordinary_config());
    fixture.publish_loaded();
    McpClientPool::insert_failed(
        fixture.pool(),
        "ordinary-failed",
        "fixture ordinary failure".to_string(),
    );

    let requirements = fixture.pool().system_requirements();
    assert_eq!(requirements.len(), 1);
    assert_eq!(requirements[0].server, "sys");

    let (_handle, generation) = fixture.connect("sys").await;
    fixture.commit_evidence("sys", DiscoveryEvidence::discovered(generation));

    let cancel = AgentCancellationToken::new();
    let negotiated = await_ready(fixture.pool(), &cancel, tokio::time::Instant::now()).await;
    assert_eq!(negotiated.len(), 1);
    assert_eq!(negotiated[0].requirement.server, "sys");

    fixture.shutdown().await;
}

#[tokio::test]
async fn system_ready_does_not_treat_unloaded_manifest_as_empty() {
    let cancel = AgentCancellationToken::new();

    // Pending + 空 configs：空 map 不是「无 System MCP」的证据，不得放行。
    let pending = SystemFixture::new();
    assert_eq!(pending.pool().system_manifest(), SystemMcpManifest::Pending);
    assert_pending(pending.pool(), &cancel, tokio::time::Instant::now()).await;

    // Loaded(empty) 才通过，且立即通过（不等普通 transport）。
    pending.publish_loaded();
    let negotiated = await_ready(pending.pool(), &cancel, tokio::time::Instant::now()).await;
    assert!(negotiated.is_empty());
    pending.shutdown().await;

    // 配置加载/校验失败：明确 Err，不是空集合。
    let failed = SystemFixture::new();
    failed
        .pool()
        .publish_system_manifest(SystemMcpManifest::Failed);
    let error = await_error(failed.pool(), &cancel, tokio::time::Instant::now()).await;
    assert_eq!(error, SystemReadinessError::ConfigurationFailed);
    failed.shutdown().await;
}

#[test]
fn system_requirements_flatten_options_and_sort_by_server() {
    let pool = McpClientPool::new_pending();
    pool.configs.write().insert(
        "sys-b".to_string(),
        system_config(Some(vec!["b".to_string()]), None),
    );
    pool.configs.write().insert(
        "sys-a".to_string(),
        system_config(Some(vec![]), Some(1_500)),
    );
    pool.configs
        .write()
        .insert("sys-off".to_string(), ordinary_config());

    let requirements = pool.system_requirements();
    assert_eq!(
        requirements
            .iter()
            .map(|r| r.server.as_str())
            .collect::<Vec<_>>(),
        vec!["sys-a", "sys-b"],
        "遍历顺序必须确定，不受 HashMap 顺序影响"
    );
    assert_eq!(requirements[0].required_tools, Vec::<String>::new());
    assert_eq!(requirements[0].timeout, Duration::from_millis(1_500));
    assert_eq!(
        requirements[1].timeout,
        Duration::from_millis(McpServerConfig::DEFAULT_SYSTEM_MCP_TIMEOUT_MS)
    );
    assert_eq!(requirements[1].required_tools, vec!["b".to_string()]);
}

// ─── 失效、代际与 deadline ───────────────────────────────────────────────────

#[tokio::test]
async fn system_ready_reports_generation_change_after_proven_readiness() {
    let mut fixture = SystemFixture::new();
    fixture.config("sys-a", system_config(None, Some(3_000)));
    fixture.config("sys-b", system_config(None, Some(3_000)));
    fixture.publish_loaded();
    let (_handle, generation) = fixture.connect("sys-a").await;
    fixture.commit_evidence("sys-a", DiscoveryEvidence::discovered(generation));

    let pool = Arc::clone(fixture.pool());
    let cancel = AgentCancellationToken::new();
    let started_at = tokio::time::Instant::now();
    let waiter = {
        let cancel = cancel.clone();
        tokio::spawn(async move { pool.await_system_connections(&cancel, started_at).await })
    };
    // current-thread runtime：spawn 后让出一次，waiter 的首次评估（全同步）必然
    // 走完并 park 在 watch 上——此时 sys-a 已被证明完成、sys-b 仍 pending。
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished(), "waiter 必须停在 sys-b 上等待");

    // 重连式换代：提交新代句柄 → 失效旧证据。已证明完成过的 server 换代后不再
    // 重试（不无限等待），交回调用方重试本次输入。
    let previous = fixture
        .pool()
        .get_client("sys-a")
        .expect("旧代句柄必须存在");
    let replacement = SystemFixture::handle("sys-a", previous.peer.clone());
    let service = fixture
        .pool()
        .services
        .lock()
        .remove("sys-a")
        .expect("旧代 service 必须存在");
    let committed = fixture.pool().try_commit_connection(
        "sys-a".to_string(),
        Arc::clone(&replacement),
        service,
    );
    assert!(committed.is_ok(), "换代连接必须被 pool 接受");
    let next_generation = fixture.pool().handle_generation(&replacement);
    assert_ne!(next_generation, generation);
    fixture.pool().clear_discovery_evidence("sys-a");

    let outcome = waiter.await.expect("waiter 不得 panic");
    assert_eq!(
        outcome.expect_err("旧代 ready 不得被复用"),
        SystemReadinessError::ConnectionChanged {
            server: "sys-a".to_string()
        }
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn system_ready_rejects_closed_pool_and_cancellation() {
    // 取消：与 timeout 是不同事实。
    let cancelled = SystemFixture::new();
    cancelled.config("sys", system_config(None, Some(5_000)));
    cancelled.publish_loaded();
    let cancel = AgentCancellationToken::new();
    cancel.cancel();
    let error = await_error(cancelled.pool(), &cancel, tokio::time::Instant::now()).await;
    assert_eq!(error, SystemReadinessError::Cancelled);
    cancelled.shutdown().await;

    // 关闭中的连接池：不等 deadline，直接 PoolClosed。
    let closed = SystemFixture::new();
    closed.config("sys", system_config(None, Some(5_000)));
    closed.publish_loaded();
    closed.pool().begin_shutdown();
    let cancel = AgentCancellationToken::new();
    let error = await_error(closed.pool(), &cancel, tokio::time::Instant::now()).await;
    assert_eq!(error, SystemReadinessError::PoolClosed);
    closed.shutdown().await;
}

#[tokio::test]
async fn system_ready_timeout_is_terminal_without_fallback() {
    let mut fixture = SystemFixture::new();
    fixture.config("sys", system_config(None, Some(60)));
    fixture.publish_loaded();
    let (_handle, generation) = fixture.connect("sys").await;

    let cancel = AgentCancellationToken::new();
    let started_at = tokio::time::Instant::now();
    let error = await_error(fixture.pool(), &cancel, started_at).await;
    assert_eq!(
        error,
        SystemReadinessError::Timeout {
            server: "sys".to_string(),
            timeout_ms: 60,
        }
    );
    // 不是取消：timeout 必须映射 fatal，不能映射 Interrupted。
    assert!(matches!(
        error.into_agent_error("McpMiddleware"),
        AgentError::MiddlewareError { .. }
    ));

    // 迟到成功不得把已失败的结果翻成成功；新一次入场可以重新准入。
    fixture.commit_evidence("sys", DiscoveryEvidence::discovered(generation));
    let negotiated = await_ready(fixture.pool(), &cancel, started_at).await;
    assert_eq!(negotiated.len(), 1);

    fixture.shutdown().await;
}

#[tokio::test]
async fn system_ready_parallel_deadlines_do_not_accumulate() {
    let mut fixture = SystemFixture::new();
    fixture.config("sys-fast", system_config(None, Some(60)));
    fixture.config("sys-slow", system_config(None, Some(3_000)));
    fixture.publish_loaded();
    let (_fast, _fast_generation) = fixture.connect("sys-fast").await;
    let (_slow, _slow_generation) = fixture.connect("sys-slow").await;

    let cancel = AgentCancellationToken::new();
    let started_at = tokio::time::Instant::now();
    let error = await_error(fixture.pool(), &cancel, started_at).await;
    assert_eq!(
        error,
        SystemReadinessError::Timeout {
            server: "sys-fast".to_string(),
            timeout_ms: 60,
        }
    );
    assert!(
        started_at.elapsed() < Duration::from_secs(1),
        "deadline 必须从同一次入场时间并发计算（sys-slow 不得叠加）"
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn system_ready_reports_disabled_and_authorization_required() {
    let cancel = AgentCancellationToken::new();

    // 禁用是确定事实：不等待、不跳过。
    let disabled = SystemFixture::new();
    let mut config = system_config(None, Some(5_000));
    config.disabled = Some(true);
    disabled.config("sys", config);
    disabled.publish_loaded();
    let error = await_error(disabled.pool(), &cancel, tokio::time::Instant::now()).await;
    assert_eq!(
        error,
        SystemReadinessError::Disabled {
            server: "sys".to_string()
        }
    );
    disabled.shutdown().await;

    // 需要人工授权：AuthorizationRequired（不是「连接中」）。
    let auth = SystemFixture::new();
    auth.config("sys", system_config(None, Some(5_000)));
    auth.publish_loaded();
    McpClientPool::insert_needs_auth(
        auth.pool(),
        "sys",
        "fixture: authorization required".to_string(),
    );
    let error = await_error(auth.pool(), &cancel, tokio::time::Instant::now()).await;
    assert_eq!(
        error,
        SystemReadinessError::AuthorizationRequired {
            server: "sys".to_string()
        }
    );
    auth.shutdown().await;
}

// ─── 错误类型与文案 ───────────────────────────────────────────────────────────

#[test]
fn discovery_evidence_is_complete_only_with_nonzero_generation() {
    assert!(!DiscoveryEvidence::default().is_complete());
    assert!(!DiscoveryEvidence::initialize_failed(7).is_complete());
    assert!(!DiscoveryEvidence::discovery_failed(7).is_complete());
    assert!(!DiscoveryEvidence::discovered(0).is_complete());
    assert!(DiscoveryEvidence::discovered(7).is_complete());
}

#[test]
fn system_readiness_error_display_is_secret_safe() {
    // 危险形态：控制字符 + URL query 凭据（全部为虚构值）。
    let error = SystemReadinessError::Disabled {
        server: "sys\u{0}\nhttps://mcp.example.invalid/endpoint?token=fixture-value".to_string(),
    };
    let text = error.to_string();
    assert!(!text.contains('\n'), "文案不得包含换行: {text}");
    assert!(!text.contains('\u{0}'), "文案不得包含控制字符: {text}");
    assert!(!text.contains("fixture-value"), "文案不得包含凭据: {text}");
    assert!(text.contains("sys"));
    assert!(text.contains("服务器已禁用"));

    // RequiredTools 保留 C 的 source 链与固定模板。
    let wrapped = SystemReadinessError::RequiredTools {
        source: SystemToolError::MissingTool {
            server: "sys".to_string(),
            tool: "read".to_string(),
        },
    };
    assert!(wrapped.to_string().starts_with("System MCP 启动失败："));
    assert!(std::error::Error::source(&wrapped).is_some());
}

#[test]
fn system_readiness_error_maps_cancellation_to_interrupted_only() {
    assert!(matches!(
        SystemReadinessError::Cancelled.into_agent_error("McpMiddleware"),
        AgentError::Interrupted
    ));

    let mapped = SystemReadinessError::ConnectionFailed {
        server: "sys".to_string(),
    }
    .into_agent_error("McpMiddleware");
    match mapped {
        AgentError::MiddlewareError { middleware, reason } => {
            assert_eq!(middleware, "McpMiddleware");
            assert!(reason.contains("transport 或协议初始化失败"));
        }
        other => panic!("非取消错误必须映射 fatal: {other:?}"),
    }
}

#[test]
fn system_readiness_error_carries_the_frozen_status_set() {
    // 冻结变体全集（sub-plan B §4.4）：任何缺失都会在这里编译失败。
    let variants = [
        SystemReadinessError::ConfigurationUnavailable,
        SystemReadinessError::ConfigurationFailed,
        SystemReadinessError::PoolClosed,
        SystemReadinessError::Disabled {
            server: "s".to_string(),
        },
        SystemReadinessError::AuthorizationRequired {
            server: "s".to_string(),
        },
        SystemReadinessError::ConnectionFailed {
            server: "s".to_string(),
        },
        SystemReadinessError::NegotiationIncomplete {
            server: "s".to_string(),
        },
        SystemReadinessError::ToolDiscoveryFailed {
            server: "s".to_string(),
        },
        SystemReadinessError::ConnectionChanged {
            server: "s".to_string(),
        },
        SystemReadinessError::Timeout {
            server: "s".to_string(),
            timeout_ms: 1,
        },
        SystemReadinessError::Cancelled,
        SystemReadinessError::RequiredTools {
            source: SystemToolError::NotModelVisible {
                server: "s".to_string(),
                tool: "t".to_string(),
            },
        },
        SystemReadinessError::CatalogPublicationFailed,
    ];
    assert_eq!(variants.len(), 13);
    for variant in variants {
        assert!(!variant.to_string().is_empty());
    }
}
