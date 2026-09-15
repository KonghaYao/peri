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
