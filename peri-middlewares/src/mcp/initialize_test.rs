use super::*;

const SCRIPT: &str = r#"
const fs = require('node:fs');
fs.appendFileSync('starts', process.cwd() + '\n');
const readline = require('node:readline').createInterface({ input: process.stdin });
readline.on('line', line => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  // Legacy servers must reject discovery so Auto can fall back to initialize.
  if (!['initialize', 'tools/list', 'resources/list', 'ping'].includes(request.method)) {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id,
      error: { code: -32601, message: 'Method not found' } }) + '\n');
    return;
  }
  let result = {};
  if (request.method === 'initialize') result = {
    protocolVersion: '2025-11-25', capabilities: {},
    serverInfo: { name: 'cwd-fixture', version: '1' },
  };
  if (request.method === 'tools/list') result = { tools: [] };
  if (request.method === 'resources/list') result = { resources: [] };
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
});
"#;

/// 极简 tracing Subscriber：捕获 WARN 事件的字段（沿用 `skill_discovery_test`
/// 的无 dev-dependency 做法）。本回归断言的是「启动失败必须在日志里可查」。
struct WarnCaptureSubscriber {
    warns: Arc<std::sync::Mutex<Vec<String>>>,
}

impl tracing::Subscriber for WarnCaptureSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() == tracing::Level::WARN
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(0)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if !self.0.is_empty() {
                    self.0.push(' ');
                }
                self.0.push_str(&format!("{}={value:?}", field.name()));
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.warns.lock().unwrap().push(fields.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// stdio 启动失败必须同时留下面板状态与日志：面板显示 failed，日志是排查入口
/// ——只写面板不写日志时，除面板之外没有任何可核对的事实。
#[test]
fn stdio_spawn_failure_is_recorded_and_logged() {
    let fixture = tempfile::tempdir().unwrap();
    // 执行目录已不存在（worktree 被移除）：stdio 启动立刻失败，无需等连接超时。
    let cwd = fixture.path().join("removed-workspace");
    // Inject merged config at the loading boundary so the test cannot start user servers.
    let config = serde_json::from_value(serde_json::json!({
        "mcpServers": { "broken": { "command": "node", "args": ["server.js"] } }
    }))
    .unwrap();
    let warns = Arc::new(std::sync::Mutex::new(Vec::new()));
    tracing::subscriber::with_default(
        WarnCaptureSubscriber {
            warns: warns.clone(),
        },
        || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let (mut tasks, spawner) = super::super::task_scope::McpTaskOwner::new();
                let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
                let (status, _) = tokio::sync::watch::channel(McpInitStatus::Pending);
                McpClientPool::initialize_config(
                    pool.clone(),
                    &cwd,
                    config,
                    Default::default(),
                    status,
                    None,
                    None,
                )
                .await;
                assert!(matches!(
                    pool.get_client("broken")
                        .map(|client| client.status.clone()),
                    Some(ClientStatus::Failed(reason)) if reason.contains("stdio 启动失败")
                ));
                pool.begin_shutdown();
                tasks.begin_shutdown();
                let _ = tasks.shutdown().await;
                assert!(pool.shutdown().await.is_complete());
            });
        },
    );
    let warns = warns.lock().unwrap();
    assert!(
        warns
            .iter()
            .any(|warn| warn.contains("MCP stdio 启动失败") && warn.contains("broken")),
        "启动失败必须留下告警日志（含服务器名），实际捕获: {warns:?}"
    );
}

#[tokio::test]
async fn worktree_static_server_uses_target_directory_on_initialize_and_reconnect() {
    let fixture = tempfile::tempdir().unwrap();
    for name in ["worktree a", "worktree b"] {
        let cwd = fixture.path().join(name);
        std::fs::create_dir(&cwd).unwrap();
        std::fs::write(cwd.join("server.js"), SCRIPT).unwrap();
        // Inject merged config at the loading boundary so the test cannot start user servers.
        let config = serde_json::from_value(serde_json::json!({
            "mcpServers": { "fixture": { "command": "node", "args": ["server.js"] } }
        }))
        .unwrap();
        let (mut tasks, spawner) = super::super::task_scope::McpTaskOwner::new();
        let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
        let (status, _) = tokio::sync::watch::channel(McpInitStatus::Pending);
        McpClientPool::initialize_config(
            pool.clone(),
            &cwd,
            config,
            Default::default(),
            status,
            None,
            None,
        )
        .await;
        assert!(matches!(
            pool.get_client("fixture")
                .map(|client| client.status.clone()),
            Some(ClientStatus::Connected)
        ));
        pool.reconnect("fixture", None).await.unwrap();
        pool.begin_shutdown();
        tasks.begin_shutdown();
        let _ = tasks.shutdown().await;
        assert!(pool.shutdown().await.is_complete());
        let starts = std::fs::read_to_string(cwd.join("starts")).unwrap();
        let expected = std::fs::canonicalize(cwd).unwrap();
        assert_eq!(
            starts
                .lines()
                .map(|path| std::fs::canonicalize(path).unwrap())
                .collect::<Vec<_>>(),
            vec![expected; 2]
        );
    }
}

#[test]
fn worktree_static_pool_rejects_rebinding_its_execution_directory() {
    let fixture = tempfile::tempdir().unwrap();
    let pool = McpClientPool::new_pending();
    pool.bind_execution_cwd(&fixture.path().join("a")).unwrap();
    assert!(pool.bind_execution_cwd(&fixture.path().join("b")).is_err());
    assert_eq!(pool.execution_cwd.get().unwrap(), &fixture.path().join("a"));
}

/// 配置失败必须是可见的 Failed：不发布 Ready、不标记 initialized、不注册任何
/// server（因而不会开始 transport）。只写日志不写状态时，1R 无法据状态放行/阻断。
#[tokio::test]
async fn test_system_mcp_config_error_never_publishes_ready() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    // 非法项目配置：声明 system_mcp_tools 却没有 system_mcp = true。
    std::fs::write(
        cwd.join(".mcp.json"),
        r#"{"mcpServers":{"sys":{"command":"node","system_mcp_tools":["search"]}}}"#,
    )
    .unwrap();
    let claude_home = fixture.path().join(".claude-test");
    std::fs::create_dir(&claude_home).unwrap();

    let pool = Arc::new(McpClientPool::new_pending());
    let (status_tx, status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::run_initialize(pool.clone(), &cwd, &claude_home, status_tx, None, None).await;

    let expected_rule = "system_mcp_tools requires system_mcp = true";
    match &*pool.init_status.read() {
        McpInitStatus::Failed(message) => assert!(
            message.contains(expected_rule),
            "pool 状态必须保留固定规则正文，实际: {message}"
        ),
        other => panic!("配置失败必须发布 Failed，实际: {other:?}"),
    }
    match &*status_rx.borrow() {
        McpInitStatus::Failed(message) => assert!(
            message.contains(expected_rule),
            "watch 通道必须保留固定规则正文，实际: {message}"
        ),
        other => panic!("watch 通道必须发布 Failed，实际: {other:?}"),
    }
    assert!(
        !pool.initialized.load(std::sync::atomic::Ordering::SeqCst),
        "配置失败不得标记 initialized"
    );
    assert!(
        pool.clients.read().is_empty(),
        "配置失败不得注册 server（未开始 transport）"
    );
    assert!(
        pool.configs.read().is_empty(),
        "配置失败不得把非法配置写入 pool"
    );
    // 配置清单同时收口：等待方必须立刻得到终态，不能把「加载失败」当
    // 「还没有 System 依赖」睡到 bootstrap 超时。
    assert_eq!(
        pool.system_manifest(),
        SystemMcpManifest::Failed,
        "配置加载/校验失败必须把配置清单收口为 Failed"
    );
}

/// 初始化成功但 `tools/list` 返回 RPC 错误：发现必须显式为失败。
///
/// 反面对照见 `empty_tools_list_is_success_not_failure`：空数组是成功结果，
/// 错误不是。二者若落在同一个 `Connected + tools=[]` 上，readiness 会把
/// 「发现失败」当成「发现完成且无工具」放行（契约 2）。
const BROKEN_DISCOVERY_SCRIPT: &str = r#"
const readline = require('node:readline').createInterface({ input: process.stdin });
readline.on('line', line => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  // tools/list 不在允许列表内：一律以 JSON-RPC 错误拒绝。
  if (!['initialize', 'resources/list', 'ping'].includes(request.method)) {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id,
      error: { code: -32601, message: 'Method not found' } }) + '\n');
    return;
  }
  let result = {};
  if (request.method === 'initialize') result = {
    protocolVersion: '2025-11-25', capabilities: {},
    serverInfo: { name: 'broken-discovery', version: '1' },
  };
  if (request.method === 'resources/list') result = { resources: [] };
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
});
"#;

/// 声明持久缓存版本的 fixture：每次 live `tools/list` 追加一行到计数文件
/// （argv[2]），用于区分「本次真实 round-trip」与「历史缓存命中」。
const VERSIONED_DISCOVERY_SCRIPT: &str = r#"
const fs = require('node:fs');
const counter = process.argv[2];
const readline = require('node:readline').createInterface({ input: process.stdin });
readline.on('line', line => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  if (!['initialize', 'tools/list', 'resources/list', 'ping'].includes(request.method)) {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id,
      error: { code: -32601, message: 'Method not found' } }) + '\n');
    return;
  }
  let result = {};
  if (request.method === 'initialize') result = {
    protocolVersion: '2025-11-25',
    capabilities: { extensions: { 'io.mcpp/server-cache-version': { cacheVersion: 'v1' } } },
    serverInfo: { name: 'versioned-discovery', version: '1' },
  };
  if (request.method === 'tools/list') {
    fs.appendFileSync(counter, 'tools/list\n');
    result = { tools: [{ name: 'search', description: 'fixture tool',
      inputSchema: { type: 'object', properties: {} } }] };
  }
  if (request.method === 'resources/list') result = { resources: [] };
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
});
"#;

fn discovery_lines(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

#[tokio::test]
async fn tools_list_error_never_becomes_connected_with_empty_tools() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("broken.js"), BROKEN_DISCOVERY_SCRIPT).unwrap();
    let config = serde_json::from_value(serde_json::json!({
        "mcpServers": { "broken-discovery": { "command": "node", "args": ["broken.js"] } }
    }))
    .unwrap();
    let (mut tasks, spawner) = super::super::task_scope::McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    let (status_tx, status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        &cwd,
        config,
        Default::default(),
        status_tx,
        None,
        None,
    )
    .await;

    let client = pool
        .get_client("broken-discovery")
        .expect("失败必须留下可核对的状态，而不是注册成 Connected");
    match &client.status {
        ClientStatus::Failed(reason) => assert!(
            reason.contains("工具发现失败"),
            "tools/list 失败必须在正文里可查，实际: {reason}"
        ),
        other => panic!("tools/list 失败不得提交 Connected，实际: {other:?}"),
    }
    assert!(
        client.tools.is_empty(),
        "失败不得留下工具清单（否则与空数组成功无法区分）"
    );
    // 不可伪造的发现证据：本代 initialize 成功、`tools/list` 失败，且代际绑定
    // 到当前句柄（B-01 的等待方据此返回 ToolDiscoveryFailed，而不是无限等待）。
    let evidence = pool
        .discovery_evidence("broken-discovery")
        .expect("发现尝试结束必须提交本代证据");
    assert!(
        evidence.initialize_ok,
        "initialize 已成功，证据必须如实记录"
    );
    assert!(
        !evidence.tools_list_ok,
        "tools/list 失败不得提交 tools_list_ok = true"
    );
    assert!(!evidence.is_complete(), "失败证据不构成完成证据");
    assert_eq!(
        evidence.generation,
        pool.handle_generation(&client),
        "证据必须绑定当前句柄的代际"
    );
    assert_ne!(evidence.generation, 0, "代际 0 是「未登记」哨兵，不算证据");
    assert_eq!(
        pool.system_manifest(),
        SystemMcpManifest::Loaded,
        "完整 configs 已写入，配置清单必须是 Loaded"
    );
    let lifecycle_failure = match &*pool.init_status.read() {
        McpInitStatus::Failed(message) => message.clone(),
        other => panic!("发现失败的 server 不得计入 ready，实际: {other:?}"),
    };
    assert!(
        lifecycle_failure.contains("工具发现失败"),
        "聚合状态必须保留发现失败正文，实际: {lifecycle_failure}"
    );
    match &*status_rx.borrow() {
        McpInitStatus::Failed(message) => assert!(
            message.contains("工具发现失败"),
            "watch 通道不得发布 ready，实际: {message}"
        ),
        other => panic!("watch 通道不得发布 ready，实际: {other:?}"),
    }

    pool.begin_shutdown();
    tasks.begin_shutdown();
    let _ = tasks.shutdown().await;
    assert!(pool.shutdown().await.is_complete());
}

/// System 启动闸门读到的必须是**本次发现**的结论，而不是等到 deadline。
///
/// 这条同时核对证据的代际绑定：`ToolDiscoveryFailed` 只在「本代证据记录了
/// initialize 成功 + live `tools/list` 失败」时产生；证据缺失或代际不符会退化成
/// `ConnectionFailed`，两者在用户可见文案上不同（契约 2 的失败分类）。
#[tokio::test]
async fn system_gate_concludes_tools_list_failure_without_waiting_for_timeout() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("broken.js"), BROKEN_DISCOVERY_SCRIPT).unwrap();
    let config = serde_json::from_value(serde_json::json!({
        "mcpServers": { "gate-broken": {
            "command": "node",
            "args": ["broken.js"],
            "system_mcp": true,
            "system_mcp_tools": [],
            "system_mcp_timeout": 5000,
        } }
    }))
    .unwrap();
    let (mut tasks, spawner) = super::super::task_scope::McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        &cwd,
        config,
        Default::default(),
        status_tx,
        None,
        None,
    )
    .await;

    let started_at = tokio::time::Instant::now();
    let outcome = pool
        .await_system_connections(
            &peri_agent::agent::AgentCancellationToken::new(),
            started_at,
        )
        .await
        .expect_err("发现失败不得放行启动");
    match outcome {
        crate::mcp::client::SystemReadinessError::ToolDiscoveryFailed { server } => {
            assert_eq!(server, "gate-broken")
        }
        other => panic!("必须归类为工具发现失败，实际: {other:?}"),
    }
    assert!(
        started_at.elapsed() < std::time::Duration::from_secs(2),
        "已知失败必须立即返回，不得等满 system_mcp_timeout"
    );

    pool.begin_shutdown();
    tasks.begin_shutdown();
    let _ = tasks.shutdown().await;
    assert!(pool.shutdown().await.is_complete());
}

#[tokio::test]
async fn empty_tools_list_is_success_not_failure() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    // 既有 fixture 对 tools/list 返回 `{tools: []}`：空数组是成功结果。
    std::fs::write(cwd.join("empty.js"), SCRIPT).unwrap();
    let config = serde_json::from_value(serde_json::json!({
        "mcpServers": { "empty-discovery": { "command": "node", "args": ["empty.js"] } }
    }))
    .unwrap();
    let (mut tasks, spawner) = super::super::task_scope::McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        &cwd,
        config,
        Default::default(),
        status_tx,
        None,
        None,
    )
    .await;

    let client = pool
        .get_client("empty-discovery")
        .expect("空数组是成功结果");
    assert!(
        matches!(client.status, ClientStatus::Connected),
        "空数组不得判为失败，实际: {:?}",
        client.status
    );
    assert!(client.tools.is_empty(), "空数组必须原样保留为空清单");
    assert!(
        matches!(&*pool.init_status.read(), McpInitStatus::Ready { .. }),
        "空数组成功必须发布 ready，实际: {:?}",
        *pool.init_status.read()
    );
    // 与失败分支的差别落在证据上：空数组是「完整发现」，不是「发现失败」。
    let evidence = pool
        .discovery_evidence("empty-discovery")
        .expect("成功发现必须提交本代证据");
    assert!(
        evidence.is_complete(),
        "空数组是成功结果，必须构成完整发现证据，实际: {evidence:?}"
    );
    assert_eq!(
        evidence.generation,
        pool.handle_generation(&client),
        "证据必须绑定当前句柄的代际"
    );

    pool.begin_shutdown();
    tasks.begin_shutdown();
    let _ = tasks.shutdown().await;
    assert!(pool.shutdown().await.is_complete());
}

/// System MCP 的启动发现必须是本次 live round-trip。
///
/// 持久缓存命中只证明「过去某个版本列过这些工具」，不能作为本次启动的
/// transport/能力协商健康证据；普通 MCP 保持既有 cache 策略（版本命中级跳过网络）。
/// 两轮初始化共用同一 pool 与同一隔离缓存根：普通 server 第二轮命中磁盘缓存，
/// System server 第二轮仍必须打到 fixture。
#[tokio::test]
async fn system_discovery_is_live_while_ordinary_server_uses_cache() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("versioned.js"), VERSIONED_DISCOVERY_SCRIPT).unwrap();
    let system_counter = fixture.path().join("system-tools-list.log");
    let ordinary_counter = fixture.path().join("ordinary-tools-list.log");
    let config: super::super::config::McpConfigFile = serde_json::from_value(serde_json::json!({
        "mcpServers": {
            "system-srv": {
                "command": "node",
                "args": ["versioned.js", system_counter.to_str().unwrap()],
                "system_mcp": true,
                "system_mcp_tools": [],
            },
            "ordinary-srv": {
                "command": "node",
                "args": ["versioned.js", ordinary_counter.to_str().unwrap()],
            },
        }
    }))
    .unwrap();
    let (mut tasks, spawner) = super::super::task_scope::McpTaskOwner::new();
    let mut pool = McpClientPool::new_pending_with_spawner(spawner);
    pool.resource_cache = crate::mcp::resource_cache::McpResourceCache::isolated_for_test();
    let pool = Arc::new(pool);

    for round in 1..=2 {
        let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        McpClientPool::initialize_config(
            pool.clone(),
            &cwd,
            config.clone(),
            Default::default(),
            status_tx,
            None,
            None,
        )
        .await;
        for name in ["system-srv", "ordinary-srv"] {
            let client = pool.get_client(name).unwrap_or_else(|| {
                panic!("第 {round} 轮 {name} 必须完成发现");
            });
            assert!(
                matches!(client.status, ClientStatus::Connected),
                "第 {round} 轮 {name} 必须完成发现，实际: {:?}",
                client.status
            );
            assert!(
                !client.tools.is_empty(),
                "第 {round} 轮 {name} 的发现结果必须进入 handle"
            );
            let evidence = pool
                .discovery_evidence(name)
                .unwrap_or_else(|| panic!("第 {round} 轮 {name} 必须提交本代发现证据"));
            assert!(
                evidence.is_complete(),
                "第 {round} 轮 {name} 的发现证据必须完整（含 live tools/list），实际: {evidence:?}"
            );
            assert_eq!(
                evidence.generation,
                pool.handle_generation(&client),
                "第 {round} 轮 {name} 的证据必须绑定当前句柄的代际"
            );
        }
    }

    assert_eq!(
        discovery_lines(&system_counter),
        2,
        "System MCP 每轮都必须走 live tools/list：required=[] 也要 round-trip，\
         缓存命中的历史清单不能作为本次启动证据"
    );
    assert_eq!(
        discovery_lines(&ordinary_counter),
        1,
        "普通 MCP 第二轮必须命中持久缓存（保持既有 cache 策略，不被 System 严格化波及）"
    );

    pool.begin_shutdown();
    tasks.begin_shutdown();
    let _ = tasks.shutdown().await;
    assert!(pool.shutdown().await.is_complete());
}

/// 重连必须换掉发现证据：旧代的成功证据不得被新代复用。
///
/// 场景：先成功发现（证据完整），随后 server 的 `tools/list` 变坏再重连。若证据
/// 按 server 名缓存而不换代，System 闸门会拿旧代证据放行一台已经报错的 server。
const RECONNECT_DISCOVERY_SCRIPT: &str = r#"
const fs = require('node:fs');
const marker = process.argv[2];
const readline = require('node:readline').createInterface({ input: process.stdin });
readline.on('line', line => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  if (!['initialize', 'tools/list', 'resources/list', 'ping'].includes(request.method)) {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id,
      error: { code: -32601, message: 'Method not found' } }) + '\n');
    return;
  }
  // 标记文件出现后 tools/list 一律失败，模拟「重连时 server 工具面已损坏」。
  if (request.method === 'tools/list') {
    if (fs.existsSync(marker)) {
      process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id,
        error: { code: -32603, message: 'tools unavailable' } }) + '\n');
    } else {
      process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id,
        result: { tools: [] } }) + '\n');
    }
    return;
  }
  let result = {};
  if (request.method === 'initialize') result = {
    protocolVersion: '2025-11-25', capabilities: {},
    serverInfo: { name: 'reconnect-discovery', version: '1' },
  };
  if (request.method === 'resources/list') result = { resources: [] };
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
});
"#;

#[tokio::test]
async fn reconnect_replaces_discovery_evidence_with_current_generation() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("reconnect.js"), RECONNECT_DISCOVERY_SCRIPT).unwrap();
    let marker = fixture.path().join("break-tools");
    let config = serde_json::from_value(serde_json::json!({
        "mcpServers": { "reconnect-srv": {
            "command": "node",
            "args": ["reconnect.js", marker.to_str().unwrap()],
            "system_mcp": true,
            "system_mcp_tools": [],
        } }
    }))
    .unwrap();
    let (mut tasks, spawner) = super::super::task_scope::McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        &cwd,
        config,
        Default::default(),
        status_tx,
        None,
        None,
    )
    .await;

    let first = pool.get_client("reconnect-srv").expect("首轮必须完成发现");
    let first_evidence = pool
        .discovery_evidence("reconnect-srv")
        .expect("首轮成功必须提交本代证据");
    assert!(
        first_evidence.is_complete(),
        "首轮成功必须构成完整证据，实际: {first_evidence:?}"
    );

    std::fs::write(&marker, "").unwrap();
    let reconnect = pool.reconnect("reconnect-srv", None).await;
    assert!(
        reconnect.is_err(),
        "重连时 tools/list 失败必须返回错误，实际: {reconnect:?}"
    );
    let current = pool
        .get_client("reconnect-srv")
        .expect("失败的重连必须留下可核对状态，而不是空句柄");
    assert!(
        matches!(current.status, ClientStatus::Failed(_)),
        "重连的 tools/list 失败必须显式 Failed，实际: {:?}",
        current.status
    );
    let evidence = pool
        .discovery_evidence("reconnect-srv")
        .expect("重连尝试结束必须提交本代证据");
    assert!(
        evidence.initialize_ok && !evidence.tools_list_ok,
        "重连证据必须如实记录 initialize 成功 / tools/list 失败，实际: {evidence:?}"
    );
    assert_eq!(
        evidence.generation,
        pool.handle_generation(&current),
        "证据必须绑定重连后句柄的代际"
    );
    assert_ne!(
        evidence.generation,
        pool.handle_generation(&first),
        "重连必须换代，旧代证据不得复用"
    );

    pool.begin_shutdown();
    tasks.begin_shutdown();
    let _ = tasks.shutdown().await;
    assert!(pool.shutdown().await.is_complete());
}

/// 启动依赖优先推进：普通 MCP 的慢握手不得把 System 发现排在后面。
///
/// 普通 fixture 阻塞在 `initialize`（放行文件出现前不回复），System fixture 立即
/// 响应。断言 System 证据先完成——若实现回到 HashMap 顺序串行连接，这条会因
/// fixture 的确定性阻塞而失败，而不是靠 sleep 猜时序。
const GATED_ORDINARY_SCRIPT: &str = r#"
const fs = require('node:fs');
const release = process.argv[2];
const readline = require('node:readline').createInterface({ input: process.stdin });
readline.on('line', line => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  if (!['initialize', 'tools/list', 'resources/list', 'ping'].includes(request.method)) {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id,
      error: { code: -32601, message: 'Method not found' } }) + '\n');
    return;
  }
  let result = {};
  if (request.method === 'initialize') {
    const until = Date.now() + 20000;
    while (!fs.existsSync(release) && Date.now() < until) {}
    result = { protocolVersion: '2025-11-25', capabilities: {},
      serverInfo: { name: 'gated-ordinary', version: '1' } };
  }
  if (request.method === 'tools/list') result = { tools: [] };
  if (request.method === 'resources/list') result = { resources: [] };
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
});
"#;

#[tokio::test]
async fn system_requirement_is_discovered_before_gated_ordinary_server() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("gated.js"), GATED_ORDINARY_SCRIPT).unwrap();
    let release = fixture.path().join("release-ordinary");
    let config = serde_json::from_value(serde_json::json!({
        "mcpServers": {
            "ordered-system": {
                "command": "node",
                "args": ["empty.js"],
                "system_mcp": true,
                "system_mcp_tools": [],
            },
            "gated-ordinary": {
                "command": "node",
                "args": ["gated.js", release.to_str().unwrap()],
            },
        }
    }))
    .unwrap();
    std::fs::write(cwd.join("empty.js"), SCRIPT).unwrap();
    let (mut tasks, spawner) = super::super::task_scope::McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    let init_pool = pool.clone();
    let init_cwd = cwd.clone();
    let init = tokio::spawn(async move {
        let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        McpClientPool::initialize_config(
            init_pool,
            &init_cwd,
            config,
            Default::default(),
            status_tx,
            None,
            None,
        )
        .await;
    });

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !pool
        .discovery_evidence("ordered-system")
        .is_some_and(|evidence| evidence.is_complete())
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "System 发现不得被普通 server 的慢握手挡住"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        pool.get_client("gated-ordinary").is_none(),
        "普通 server 尚未放行，不应已经写入句柄（说明它排在 System 之前）"
    );

    std::fs::write(&release, "").unwrap();
    init.await.unwrap();
    let ordinary = pool
        .get_client("gated-ordinary")
        .expect("放行后必须写入句柄");
    assert!(
        matches!(ordinary.status, ClientStatus::Connected),
        "放行后普通 server 必须完成连接，实际: {:?}",
        ordinary.status
    );

    pool.begin_shutdown();
    tasks.begin_shutdown();
    let _ = tasks.shutdown().await;
    assert!(pool.shutdown().await.is_complete());
}

// ── builtin 分支（E-03，W2）────────────────────────────────────────────────────
//
// builtin 条目**只能**由运行时标记 `source = ConfigSource::Builtin` 产生
// （`#[serde(skip)]`，配置文件无法构造），因此这些用例直接构造 typed 配置并调用
// `initialize_config` / `reconnect`，不经 `.mcp.json`。

/// builtin 默认层的条目形状：无 command / url，身份来自 `source`。
fn builtin_server_config(instance: &str) -> super::super::config::McpServerConfig {
    super::super::config::McpServerConfig {
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        protocol_version: None,
        subscriptions: None,
        system_mcp: Some(true),
        system_mcp_tools: Some(Vec::new()),
        system_mcp_timeout: None,
        source: Some(peri_acp_types::plugin::ConfigSource::Builtin {
            instance: instance.to_string(),
        }),
    }
}

fn builtin_config_file(
    servers: Vec<(&str, super::super::config::McpServerConfig)>,
) -> super::super::config::McpConfigFile {
    let mut config = super::super::config::McpConfigFile::default();
    for (name, server) in servers {
        config.mcp_servers.insert(name.to_string(), server);
    }
    config
}

/// 未注册的 builtin 实例名：typed 失败 + 本代失败证据，**不**发布 ready。
///
/// 实例解析的唯一事实源是注册表（`builtin_mcp::find` 只命中已实现实例）：名字不在
/// 注册表内时不得建立传输，也不得降级成 stdio / http（那会把「未接线」伪装成可用）。
#[tokio::test]
async fn builtin_unregistered_instance_fails_without_ready_evidence() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);

    McpClientPool::initialize_config(
        pool.clone(),
        &cwd,
        builtin_config_file(vec![("cron", builtin_server_config("cron"))]),
        Default::default(),
        status_tx,
        None,
        None,
    )
    .await;

    let handle = pool
        .get_client("cron")
        .expect("失败必须留下可核对的句柄，而不是空 entry");
    let ClientStatus::Failed(reason) = &handle.status else {
        panic!("未注册实例必须显式 Failed，实际: {:?}", handle.status);
    };
    assert!(
        reason.contains("builtin MCP 实例未注册: cron"),
        "失败原因必须保留 typed 规则文本，实际: {reason}"
    );
    assert!(
        !reason.contains(&cwd.display().to_string()),
        "错误文本不得包含路径，实际: {reason}"
    );
    let evidence = pool
        .discovery_evidence("cron")
        .expect("失败收口必须提交本代证据");
    assert!(
        !evidence.initialize_ok && !evidence.tools_list_ok,
        "启动失败不得留下任何 ready 证据，实际: {evidence:?}"
    );
    let status = pool.init_status.read().clone();
    match &status {
        McpInitStatus::Failed(message) => assert!(
            message.contains("cron"),
            "启动状态必须是 Failed 且指名实例，实际: {message}"
        ),
        other => panic!("必须发布 Failed，实际: {other:?}"),
    }
}

/// reconnect 的 builtin 分支：执行目录未绑定时按 stdio 路径的同一措辞失败。
///
/// cwd 不得 fallback 到进程 cwd——artifact 实例的相对路径解析根必须是 pool 绑定的
/// 执行目录，否则「同一 server 在不同工作区解析出不同文件」会静默发生。
#[tokio::test]
async fn reconnect_builtin_without_bound_execution_directory_is_typed_failure() {
    let pool = Arc::new(McpClientPool::new_pending());
    pool.configs
        .write()
        .insert("artifact".to_string(), builtin_server_config("artifact"));

    let result = pool.reconnect("artifact", None).await;
    let Err(crate::mcp::client::McpPoolError::ConnectionFailed { server, reason }) = result else {
        panic!("未绑定执行目录必须失败，实际: {result:?}");
    };
    assert_eq!(server, "artifact");
    assert_eq!(
        reason, "MCP execution directory is not initialized",
        "措辞必须与 stdio 路径逐字一致"
    );
    // 与 stdio 路径同构：该分支在建立传输前就提前返回（调用方已 ClearEvidence），
    // 因此不提交新证据——「没有证据」等于「本代还没有结论」，不得伪造成成功或失败。
    assert!(
        pool.discovery_evidence("artifact").is_none(),
        "cwd 未绑定必须在建立传输之前提前返回（与 stdio 同构）"
    );
    assert_eq!(pool.builtin_task_count(), 0, "未建立链路不得登记 task");
}

/// reconnect 的 builtin 分支：未注册实例名 + 已绑定目录 → typed `ConnectionFailed`，
/// 并且不留下任何 ready 证据。
#[tokio::test]
async fn reconnect_builtin_unregistered_instance_records_failure() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    pool.bind_execution_cwd(&cwd).unwrap();
    pool.configs
        .write()
        .insert("lsp".to_string(), builtin_server_config("lsp"));

    let result = pool.reconnect("lsp", None).await;
    let Err(crate::mcp::client::McpPoolError::ConnectionFailed { server, reason }) = result else {
        panic!("未注册实例必须失败，实际: {result:?}");
    };
    assert_eq!(server, "lsp");
    assert!(
        reason.contains("builtin MCP 实例未注册: lsp"),
        "实际: {reason}"
    );
    let evidence = pool
        .discovery_evidence("lsp")
        .expect("失败收口必须提交本代证据");
    assert!(
        !evidence.is_complete(),
        "失败不得构成完整证据，实际: {evidence:?}"
    );
    assert_eq!(pool.builtin_task_count(), 0, "失败路径不得登记 task");
}

/// 已实现的 builtin 实例经**生产**初始化链完整落地：transport → 握手 → live
/// `tools/list` → `Connected` 句柄，且 server task 已登记（关闭时有归属、无 orphan）。
#[tokio::test]
async fn builtin_instance_completes_initialization_and_registers_task() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);

    McpClientPool::initialize_config(
        pool.clone(),
        &cwd,
        builtin_config_file(vec![("web", builtin_server_config("web"))]),
        Default::default(),
        status_tx,
        None,
        None,
    )
    .await;

    let handle = pool
        .get_client("web")
        .expect("已实现的 builtin 实例必须完成连接");
    assert!(
        matches!(handle.status, ClientStatus::Connected),
        "builtin 实例必须经同一条处理链提交 Connected，实际: {:?}",
        handle.status
    );
    assert_eq!(
        handle
            .tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<Vec<_>>(),
        vec!["WebSearch", "WebFetch"],
        "live tools/list 必须返回真实 handler 声明的工具"
    );
    assert!(
        matches!(
            handle.source,
            Some(peri_acp_types::plugin::ConfigSource::Builtin { .. })
        ),
        "句柄必须保留 builtin 身份（面板与 discover 工具的来源标签依赖它）"
    );
    assert!(handle.url.is_none(), "builtin 实例无 URL / 无凭据");
    let evidence = pool
        .discovery_evidence("web")
        .expect("成功必须提交本代完整发现证据");
    assert!(
        evidence.is_complete(),
        "只有真实成功的 live tools/list 才构成完整证据，实际: {evidence:?}"
    );
    assert_eq!(pool.builtin_task_count(), 1, "server task 必须已登记");

    let status = pool.init_status.read().clone();
    assert!(
        matches!(status, McpInitStatus::Ready { total: 1 }),
        "初始化必须收口为 Ready，实际: {status:?}"
    );

    pool.begin_shutdown();
    let _ = pool.shutdown().await;
    assert_eq!(
        pool.builtin_task_count(),
        0,
        "pool 关闭后不得留下 orphan server task"
    );
}

/// reconnect 的 builtin **成功**路径：重建全新链路（新 task + 新代际），旧 task 收敛。
///
/// 「全新」是可观察的：代际前进、发现证据换代、task 表仍恰好一条（旧条目已被移除并
/// 收敛，不是被覆盖后泄漏）。
#[tokio::test]
async fn reconnect_builtin_replaces_link_and_advances_generation() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        &cwd,
        builtin_config_file(vec![("artifact", builtin_server_config("artifact"))]),
        Default::default(),
        status_tx,
        None,
        None,
    )
    .await;

    let first = pool.get_client("artifact").expect("首轮必须完成连接");
    let first_generation = pool.handle_generation(&first);
    let first_evidence = pool
        .discovery_evidence("artifact")
        .expect("首轮成功必须提交本代证据");
    assert!(first_evidence.is_complete());

    pool.reconnect("artifact", None)
        .await
        .expect("已实现 builtin 实例的重连必须成功");

    let second = pool.get_client("artifact").expect("重连成功后必须有句柄");
    assert!(
        matches!(second.status, ClientStatus::Connected),
        "重连后必须仍为 Connected，实际: {:?}",
        second.status
    );
    assert_ne!(
        pool.handle_generation(&second),
        first_generation,
        "重连必须换新句柄（代际前进）"
    );
    let second_evidence = pool
        .discovery_evidence("artifact")
        .expect("重连成功必须提交本代证据");
    assert!(second_evidence.is_complete());
    assert_ne!(
        second_evidence.generation, first_evidence.generation,
        "重连的本代证据不得复用旧代"
    );
    assert_eq!(
        pool.builtin_task_count(),
        1,
        "旧 server task 必须已移除并收敛，新 task 恰好一条"
    );

    pool.begin_shutdown();
    let _ = pool.shutdown().await;
    assert_eq!(pool.builtin_task_count(), 0);
}
