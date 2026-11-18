//! Dynamic MCP Loader 契约（plan 5.1–5.3 / B4）。
//!
//! 覆盖：Loader 可被 ToolSearch 发现且只声明来源身份；参数无法注入地址或改写目标；
//! 来源关闭后 Loader 从目录消失且旧句柄拒绝执行；成功加载返回目录事实。

use std::path::PathBuf;
use std::time::Duration;

use peri_acp_types::mcp_control::{
    SessionMcpCatalogSnapshot, SessionToolCatalogPreparation, SessionToolCatalogPreparationPort,
    SessionToolCatalogPrepareError,
};
use peri_agent::session::tool_catalog::SessionToolCatalog;

use super::*;
use crate::mcp::task_scope::McpTaskOwner;
use crate::tool_search::ExecuteExtraTool;
use crate::tool_search::ToolSearchIndex;

struct Fixture {
    _dir: tempfile::TempDir,
    cwd: PathBuf,
    pool: Arc<McpClientPool>,
    tasks: McpTaskOwner,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let (tasks, spawner) = McpTaskOwner::new();
        let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
        pool.bind_execution_cwd(&cwd).unwrap();
        Self {
            _dir: dir,
            cwd,
            pool,
            tasks,
        }
    }

    fn snapshot(&self) -> Arc<SessionMcpCatalogSnapshot> {
        crate::mcp::control::catalog_source(self.pool.clone(), "session-1").snapshot()
    }

    fn loaders(&self) -> Vec<Arc<dyn BaseTool>> {
        self.snapshot()
            .tools
            .iter()
            .filter(|tool| tool.name().starts_with(LOADER_PREFIX))
            .cloned()
            .collect()
    }

    fn single_loader(&self) -> Arc<dyn BaseTool> {
        let mut loaders = self.loaders();
        assert_eq!(loaders.len(), 1, "expected exactly one dynamic loader");
        loaders.remove(0)
    }

    /// 与生产装配同一来源：PoolCatalog 投影 + 会话工具目录。
    fn session_catalog(&self) -> Arc<SessionToolCatalog> {
        Arc::new(SessionToolCatalog::new(
            std::collections::BTreeMap::new(),
            Some(crate::mcp::control::catalog_source(
                self.pool.clone(),
                "session-1",
            )),
        ))
    }

    fn context_with(catalog: &Arc<SessionToolCatalog>) -> ToolContext<'static> {
        let concrete = Arc::clone(catalog);
        let port: Arc<dyn SessionToolCatalogPreparationPort> = concrete;
        ToolContext::new(&[], ".").with_session_tool_catalog(port)
    }

    fn context(&self) -> ToolContext<'static> {
        Self::context_with(&self.session_catalog())
    }

    async fn shutdown(mut self) {
        self.pool.begin_shutdown();
        self.tasks.begin_shutdown();
        let _ = self.tasks.shutdown().await;
        assert!(self.pool.shutdown().await.is_complete());
    }
}

fn dynamic_stdio_config() -> McpServerConfig {
    serde_json::from_value(json!({
        "command": "node",
        "args": ["server.js"],
        "dynamic": true,
    }))
    .expect("valid dynamic stdio config")
}

fn secret_bearing_config() -> McpServerConfig {
    serde_json::from_value(json!({
        "url": "https://user:topsecret@internal.example.test/mcp?access_token=topsecret",
        "headers": { "Authorization": "Bearer topsecret" },
        "env": { "MCP_SECRET": "topsecret" },
        "dynamic": true,
    }))
    .expect("valid dynamic config")
}

/// Loader 是 deferred 宿主工具：可被索引发现，且身份只声明受控来源，没有 wire 工具名。
#[test]
fn dynamic_loader_is_discoverable_and_has_no_wire_tool_identity() {
    let fixture = Fixture::new();
    let mut lazy = dynamic_stdio_config();
    lazy.source = Some(ConfigSource::Plugin);
    fixture.pool.configs.write().insert("research".into(), lazy);
    let mut eager = dynamic_stdio_config();
    eager.dynamic = false;
    fixture.pool.configs.write().insert("eager".into(), eager);
    let mut disabled = dynamic_stdio_config();
    disabled.disabled = Some(true);
    fixture
        .pool
        .configs
        .write()
        .insert("disabled-lazy".into(), disabled);

    let snapshots = fixture.snapshot();
    let loaders: Vec<_> = snapshots
        .tools
        .iter()
        .filter(|tool| tool.name().starts_with(LOADER_PREFIX))
        .collect();
    assert_eq!(
        loaders.len(),
        1,
        "only enabled dynamic configs get a loader"
    );
    let loader = loaders[0].clone();

    assert!(loader.name().contains("research"));
    assert!(loader.description().contains("research"));
    assert!(
        !loader.is_direct(),
        "loader must be discovered via ToolSearch"
    );
    assert!(loader.visible_to_model());
    assert_eq!(loader.mcp_server_name(), Some("research"));
    // 没有 wire 工具名：Loader 不能冒充 MCP App 业务工具或远端 tools/call 目标。
    assert_eq!(loader.mcp_tool_name(), None);
    assert_eq!(loader.builtin_mcp_instance(), None);

    let parameters = loader.parameters();
    assert_eq!(parameters["type"], "object");
    assert_eq!(parameters["properties"], json!({}));
    assert_eq!(parameters["additionalProperties"], false);

    let index = ToolSearchIndex::new();
    index.build(
        snapshots
            .tools
            .iter()
            .filter(|tool| !tool.is_direct() && tool.visible_to_model())
            .cloned()
            .collect(),
    );
    let hits = index.search("research", 5);
    assert!(
        hits.iter().any(|hit| hit.name == loader.name()),
        "ToolSearch must discover the loader by its display name: {hits:?}"
    );
    let selected = index.search(&format!("select:{}", loader.name()), 5);
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].name, loader.name());
}

/// 调用参数不能注入 URL、command、server_name、config 或 force 来改写目标。
#[tokio::test]
async fn loader_rejects_target_injection_without_touching_the_source() {
    let fixture = Fixture::new();
    fixture
        .pool
        .configs
        .write()
        .insert("lazy".into(), secret_bearing_config());
    let loader = fixture.single_loader();
    let name = loader.name().to_owned();
    let description = loader.description().to_owned();
    for secret in ["topsecret", "internal.example.test"] {
        assert!(
            !name.contains(secret),
            "credentials must not enter loader name"
        );
        assert!(
            !description.contains(secret),
            "credentials must not enter loader search text"
        );
    }

    for injected in [
        json!({"url": "https://evil.example/mcp"}),
        json!({"command": "sh"}),
        json!({"server_name": "other"}),
        json!({"config": {"url": "https://evil.example/mcp"}}),
        json!({"force": true}),
    ] {
        let error = loader
            .invoke(injected, fixture.context())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("takes no arguments"),
            "unexpected error for injected args: {error}"
        );
    }
    let error = loader
        .invoke(json!(["not-an-object"]), fixture.context())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("takes no arguments"));
    assert!(
        fixture.pool.client("lazy").is_none(),
        "rejected arguments must not start a connection"
    );
    assert!(
        fixture
            .pool
            .existing_server_slot("lazy")
            .is_none_or(|slot| slot.load_attempt.lock().is_none()),
        "rejected arguments must not create a load attempt"
    );
}

/// 关闭来源后 Loader 从目录消失，移除配置后旧句柄也不能再加载。
#[tokio::test]
async fn closing_the_source_disables_the_loader() {
    let fixture = Fixture::new();
    fixture
        .pool
        .configs
        .write()
        .insert("lazy".into(), dynamic_stdio_config());
    let loader = fixture.single_loader();

    fixture
        .pool
        .close_server_connection("lazy", true)
        .await
        .unwrap();
    assert!(
        fixture.loaders().is_empty(),
        "stopped source must not keep a loader in the catalog"
    );
    let error = loader
        .invoke(json!({}), fixture.context())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("lazy"),
        "stale loader must fail with the source name: {error}"
    );

    fixture.pool.remove_server("lazy").await;
    assert!(fixture.loaders().is_empty());
    let error = loader
        .invoke(json!({}), fixture.context())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("lazy"));
    assert!(fixture.pool.client("lazy").is_none());
}

/// 名称由规范键 + 来源摘要构成：稳定、合法、限长，且随作用域/插件身份变化。
#[test]
fn loader_names_are_stable_bounded_and_source_bound() {
    let first = Fixture::new();
    let mut plugin = secret_bearing_config();
    plugin.source = Some(ConfigSource::Plugin);
    first
        .pool
        .configs
        .write()
        .insert("plugin:pa:srvA".into(), plugin.clone());
    first
        .pool
        .plugin_sources
        .write()
        .insert("plugin:pa:srvA".into(), "pa@alpha".into());
    let name = first.single_loader().name().to_owned();
    assert!(name.starts_with("LoadMCP__"));
    assert!(
        name.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'),
        "loader name must stay API-safe: {name}"
    );
    assert!(name.len() <= 64, "loader name must stay bounded: {name}");
    assert!(name.contains("plugin_pa_srva"));
    assert!(!name.contains("topsecret"));
    assert_eq!(
        name,
        first.single_loader().name(),
        "same source identity must produce a stable loader name"
    );

    let other_plugin = Fixture::new();
    other_plugin
        .pool
        .configs
        .write()
        .insert("plugin:pa:srvA".into(), plugin);
    other_plugin
        .pool
        .plugin_sources
        .write()
        .insert("plugin:pa:srvA".into(), "pb@beta".into());
    assert_ne!(
        name,
        other_plugin.single_loader().name(),
        "plugin identity must participate in the binding"
    );

    let other_scope = Fixture::new();
    let mut global = secret_bearing_config();
    global.source = Some(ConfigSource::Global(PathBuf::from("/tmp/peri")));
    other_scope
        .pool
        .configs
        .write()
        .insert("plugin:pa:srvA".into(), global);
    assert_ne!(
        name,
        other_scope.single_loader().name(),
        "config scope must participate in the binding"
    );
}

/// 大小写折叠后同名不冲突：摘要区分仅大小写不同的 server key。
#[test]
fn case_folded_server_keys_get_distinct_loader_names() {
    let fixture = Fixture::new();
    let scope = Some(ConfigSource::Project(PathBuf::from("/tmp/peri")));
    for key in ["Research", "research"] {
        let mut config = dynamic_stdio_config();
        config.source = scope.clone();
        fixture.pool.configs.write().insert(key.into(), config);
    }
    let names: Vec<String> = fixture
        .loaders()
        .into_iter()
        .map(|loader| loader.name().to_ascii_lowercase())
        .collect();
    assert_eq!(names.len(), 2);
    assert_ne!(names[0], names[1]);
}

#[cfg(not(target_os = "emscripten"))]
const ECHO_SERVER: &str = r#"
const readline = require('node:readline').createInterface({ input: process.stdin });
function handle(line) {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  let result;
  if (request.method === 'initialize') result = {
    protocolVersion: '2025-11-25',
    capabilities: { tools: {}, resources: {} },
    serverInfo: { name: 'loader-fixture', version: '1' },
  };
  else if (request.method === 'tools/list') result = {
    tools: [{
      name: 'echo',
      description: 'Echo input',
      inputSchema: {
        type: 'object',
        properties: {
          message: { type: 'string', description: 'Text to echo back' },
        },
        required: ['message'],
      },
    }],
  };
  else if (request.method === 'resources/list') result = { resources: [] };
  else if (request.method === 'resources/templates/list') result = { resourceTemplates: [] };
  else if (request.method === 'ping') result = {};
  else {
    process.stdout.write(JSON.stringify({
      jsonrpc: '2.0', id: request.id,
      error: { code: -32601, message: 'Method not found' },
    }) + '\n');
    return;
  }
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
}
readline.on('line', handle);
"#;

/// 成功加载绑定配置，返回目录事实；再次调用复用同一连接。
#[cfg(not(target_os = "emscripten"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loader_loads_bound_config_and_reports_ready_catalog() {
    let fixture = Fixture::new();
    std::fs::write(fixture.cwd.join("server.js"), ECHO_SERVER).unwrap();
    fixture
        .pool
        .configs
        .write()
        .insert("lazy".into(), dynamic_stdio_config());
    let loader = fixture.single_loader();

    let output = loader
        .invoke(json!({}), fixture.context())
        .await
        .expect("first load must succeed");
    let result: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(result["status"], "ready");
    assert_eq!(result["server"], "lazy");
    assert_eq!(result["reused"], false);
    assert_eq!(result["available_tool_count"], 1);
    assert_eq!(
        result["catalog_generation"].as_u64().unwrap(),
        fixture.pool.catalog_generation.load(Ordering::Acquire)
    );

    let after_load = fixture.snapshot();
    assert!(
        after_load
            .tools
            .iter()
            .any(|tool| tool.mcp_server_name() == Some("lazy")
                && tool.mcp_tool_name() == Some("echo")),
        "loaded server tools must join the same catalog"
    );

    let reused: Value = serde_json::from_str(
        &loader
            .invoke(json!({}), fixture.context())
            .await
            .expect("second load must reuse the connection"),
    )
    .unwrap();
    assert_eq!(reused["reused"], true);
    assert_eq!(reused["available_tool_count"], 1);

    tokio::time::sleep(Duration::from_millis(20)).await;
    fixture.shutdown().await;
}

/// B5：加载提交后准备当前会话目录——下一轮可搜完整 schema，当前 Act 视图不变，
/// 失效目标由既有 call gate 拒绝（plan 7 / 8 / 12-B5）。
#[cfg(not(target_os = "emscripten"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn load_prepares_the_next_reason_without_replacing_the_current_act() {
    let fixture = Fixture::new();
    std::fs::write(fixture.cwd.join("server.js"), ECHO_SERVER).unwrap();
    fixture
        .pool
        .configs
        .write()
        .insert("lazy".into(), dynamic_stdio_config());
    let catalog = fixture.session_catalog();
    let act_view = catalog.snapshot();
    assert!(
        !act_view.tools.contains_key("mcp__lazy__echo"),
        "fixture must start before the server is loaded"
    );

    let loader = fixture.single_loader();
    let output = loader
        .invoke(json!({}), Fixture::context_with(&catalog))
        .await
        .expect("load must verify the next session catalog");
    let result: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(result["status"], "ready");
    assert_eq!(
        result["session_tool_count"], 1,
        "the loaded server must contribute its business tool"
    );
    assert!(
        result["session_catalog_generation"].as_u64().unwrap()
            >= result["catalog_generation"].as_u64().unwrap(),
        "prepared generation must include the committed connection"
    );

    // 当前 Act 持有的快照还是同一份对象，加载不会中途替换执行目录。
    assert!(Arc::ptr_eq(&act_view, &catalog.snapshot()));
    assert!(!act_view.tools.contains_key("mcp__lazy__echo"));
    let tool_invoke =
        ExecuteExtraTool::new(Arc::new(parking_lot::RwLock::new(act_view.tool_map())));
    let not_in_act = tool_invoke
        .invoke(
            json!({"name": "mcp__lazy__echo", "arguments": {"message": "hi"}}),
            Fixture::context_with(&catalog),
        )
        .await
        .unwrap_err();
    assert!(
        not_in_act.to_string().contains("Tool not found"),
        "the current Act must not resolve a tool that only the next Reason sees: {not_in_act}"
    );

    // 下一轮 Reason 刷新后，业务工具与完整 schema 进入搜索索引。
    let next = catalog
        .refresh()
        .expect("the next Reason must publish the committed connection");
    assert!(next.generation > act_view.generation);
    let echo = next
        .tools
        .get("mcp__lazy__echo")
        .expect("loaded business tool must be admitted");
    assert_eq!(echo.tool.mcp_server_name(), Some("lazy"));
    assert_eq!(echo.tool.mcp_tool_name(), Some("echo"));
    let index = ToolSearchIndex::new();
    index.build(
        next.tools
            .values()
            .filter(|entry| !entry.tool.is_direct() && entry.tool.visible_to_model())
            .map(|entry| Arc::clone(&entry.tool))
            .collect(),
    );
    let hits = index.search("echo", 5);
    let hit = hits
        .iter()
        .find(|hit| hit.name == "mcp__lazy__echo")
        .unwrap_or_else(|| panic!("ToolSearch must find the loaded tool: {hits:?}"));
    assert_eq!(hit.parameters["properties"]["message"]["type"], "string");
    assert_eq!(hit.parameters["required"], json!(["message"]));

    // 旧快照里的目标在来源停止后不能再执行（call gate 拒绝失效调用）。
    let pinned_echo = Arc::clone(&echo.tool);
    fixture
        .pool
        .close_server_connection("lazy", true)
        .await
        .expect("closing the loaded source must settle");
    let mut ctx = Fixture::context_with(&catalog);
    ctx.session_id = Some("session-1".into());
    let stale = pinned_echo
        .invoke(json!({"message": "hi"}), ctx)
        .await
        .unwrap_err();
    assert!(
        stale.to_string().contains("draining or closed"),
        "call gate must reject a target from a stopped source: {stale}"
    );

    fixture.shutdown().await;
}

/// 服务器有工具但本会话全部过滤：仍可 ready，但必须明确没有可调用工具。
#[cfg(not(target_os = "emscripten"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn load_reports_zero_session_tools_when_the_session_filters_them_all() {
    let fixture = Fixture::new();
    std::fs::write(fixture.cwd.join("server.js"), ECHO_SERVER).unwrap();
    fixture
        .pool
        .configs
        .write()
        .insert("lazy".into(), dynamic_stdio_config());
    // 只保留没有 wire 工具名的候选（Loader），过滤掉远端业务工具。
    let catalog = Arc::new(SessionToolCatalog::with_filter(
        std::collections::BTreeMap::new(),
        Some(crate::mcp::control::catalog_source(
            fixture.pool.clone(),
            "session-1",
        )),
        Arc::new(|tool: &dyn BaseTool| tool.mcp_tool_name().is_none()),
    ));
    let loader = fixture.single_loader();

    let output = loader
        .invoke(json!({}), Fixture::context_with(&catalog))
        .await
        .expect("a fully filtered session still connects");
    let result: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(result["status"], "ready");
    assert_eq!(result["available_tool_count"], 1);
    assert_eq!(result["session_tool_count"], 0);
    let note = result["note"]
        .as_str()
        .expect("filtered-but-present tools require an explicit note");
    assert!(
        note.contains("none are callable"),
        "unexpected note: {note}"
    );
    assert!(
        !catalog
            .refresh()
            .expect("filtered refresh must stay constructible")
            .tools
            .contains_key("mcp__lazy__echo"),
        "the session policy must keep filtering the business tool"
    );

    fixture.shutdown().await;
}
/// 连接与发现成功但当前会话目录构造失败：返回目录阶段错误，连接仍归 pool 所有。
#[cfg(not(target_os = "emscripten"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn load_fails_at_the_catalog_stage_without_closing_the_connection() {
    struct FailingCatalog;

    impl SessionToolCatalogPreparationPort for FailingCatalog {
        fn prepare_next(
            &self,
        ) -> Result<SessionToolCatalogPreparation, SessionToolCatalogPrepareError> {
            Err(SessionToolCatalogPrepareError::new(
                "candidate alias conflict",
            ))
        }
    }

    let fixture = Fixture::new();
    std::fs::write(fixture.cwd.join("server.js"), ECHO_SERVER).unwrap();
    fixture
        .pool
        .configs
        .write()
        .insert("lazy".into(), dynamic_stdio_config());
    let loader = fixture.single_loader();
    let port: Arc<dyn SessionToolCatalogPreparationPort> = Arc::new(FailingCatalog);

    let error = loader
        .invoke(
            json!({}),
            ToolContext::new(&[], ".").with_session_tool_catalog(port),
        )
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("session catalog could not be prepared"),
        "loader must report the catalog stage: {message}"
    );
    assert!(
        message.contains("candidate alias conflict"),
        "loader must preserve the original catalog error: {message}"
    );
    assert!(
        fixture.pool.client("lazy").is_some(),
        "a session-specific catalog failure must not close the pooled connection"
    );

    fixture.shutdown().await;
}

/// 需要 OAuth 的 dynamic server：加载报告失败而不是 ready，且不留下可调用连接
/// 或 OAuth 流程（plan 10 / 12-B6 授权面）。
#[cfg(not(target_os = "emscripten"))]
#[tokio::test]
async fn load_requiring_authorization_is_not_ready_and_starts_no_flow() {
    let fixture = Fixture::new();
    let config: McpServerConfig = serde_json::from_value(json!({
        "url": "https://mcp.example.test/rpc",
        "oauth": {},
        "dynamic": true,
    }))
    .expect("valid oauth dynamic config");
    fixture.pool.configs.write().insert("lazy".into(), config);
    let loader = fixture.single_loader();

    let error = loader
        .invoke(json!({}), fixture.context())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("OAuth authorization required"),
        "loader must surface the authorization requirement: {error}"
    );
    let handle = fixture
        .pool
        .client("lazy")
        .expect("authorization requirement must be queryable");
    assert!(matches!(handle.status, ClientStatus::Failed(_)));
    assert!(matches!(
        handle.oauth_status,
        crate::mcp::OAuthStatus::NeedsAuthorization
    ));
    assert!(handle.tools.is_empty());
    assert!(
        fixture.pool.active_oauth_flow("lazy").is_none(),
        "loader must not start an interactive OAuth flow"
    );
    assert_eq!(fixture.pool.retained_service_count(), 0);

    fixture.shutdown().await;
}
