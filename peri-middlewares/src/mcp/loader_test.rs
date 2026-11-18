//! Dynamic MCP Loader 契约（plan 5.1–5.3 / B4）。
//!
//! 覆盖：Loader 可被 ToolSearch 发现且只声明来源身份；参数无法注入地址或改写目标；
//! 来源关闭后 Loader 从目录消失且旧句柄拒绝执行；成功加载返回目录事实。

use std::path::PathBuf;
use std::time::Duration;

use peri_acp_types::mcp_control::SessionMcpCatalogSnapshot;

use super::*;
use crate::mcp::task_scope::McpTaskOwner;
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

fn context() -> ToolContext<'static> {
    ToolContext::new(&[], ".")
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
        let error = loader.invoke(injected, context()).await.unwrap_err();
        assert!(
            error.to_string().contains("takes no arguments"),
            "unexpected error for injected args: {error}"
        );
    }
    let error = loader
        .invoke(json!(["not-an-object"]), context())
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
    let error = loader.invoke(json!({}), context()).await.unwrap_err();
    assert!(
        error.to_string().contains("lazy"),
        "stale loader must fail with the source name: {error}"
    );

    fixture.pool.remove_server("lazy").await;
    assert!(fixture.loaders().is_empty());
    let error = loader.invoke(json!({}), context()).await.unwrap_err();
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
    tools: [{ name: 'echo', description: 'Echo input', inputSchema: { type: 'object', properties: {} } }],
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
        .invoke(json!({}), context())
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
            .invoke(json!({}), context())
            .await
            .expect("second load must reuse the connection"),
    )
    .unwrap();
    assert_eq!(reused["reused"], true);
    assert_eq!(reused["available_tool_count"], 1);

    tokio::time::sleep(Duration::from_millis(20)).await;
    fixture.shutdown().await;
}
