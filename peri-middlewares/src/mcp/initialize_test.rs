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
