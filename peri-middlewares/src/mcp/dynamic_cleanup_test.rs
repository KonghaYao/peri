//! Dynamic MCP 的错误、停止竞争与资源清理（plan 10 / 12-B6）。
//!
//! 覆盖：live discovery 失败后收口未提交连接并可重试；加载中 stop 不留下句柄、
//! 任务或保留连接；重复 load/close 不增长；来源移除拒绝迟到提交。

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::{Arc, ClientStatus, McpClientPool};
use crate::mcp::task_scope::McpTaskOwner;

/// 快速连接：initialize、空工具列表与资源列表。
const READY_SERVER: &str = r#"
const fs = require('node:fs');
fs.appendFileSync('starts', process.cwd() + '\n');
const readline = require('node:readline').createInterface({ input: process.stdin });
function handle(line) {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  let result;
  if (request.method === 'initialize') result = {
    protocolVersion: '2025-11-25', capabilities: { tools: {}, resources: {} },
    serverInfo: { name: 'cleanup-fixture', version: '1' },
  };
  else if (request.method === 'tools/list') result = { tools: [{
    name: 'echo',
    description: 'Echo input',
    inputSchema: { type: 'object', properties: {}, additionalProperties: true },
  }] };
  else if (request.method === 'resources/list') result = { resources: [] };
  else if (request.method === 'resources/templates/list') result = { resourceTemplates: [] };
  else if (request.method === 'ping') result = {};
  else {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id,
      error: { code: -32601, message: 'Method not found' } }) + '\n');
    return;
  }
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
}
readline.on('line', handle);
"#;

/// initialize 成功后 `tools/list` 返回协议错误：live discovery 失败。
const DISCOVERY_FAIL_SERVER: &str = r#"
const fs = require('node:fs');
fs.appendFileSync('starts', process.cwd() + '\n');
const readline = require('node:readline').createInterface({ input: process.stdin });
function handle(line) {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  if (request.method === 'initialize') {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result: {
      protocolVersion: '2025-11-25', capabilities: {},
      serverInfo: { name: 'discovery-fail-fixture', version: '1' },
    } }) + '\n');
    return;
  }
  if (request.method === 'tools/list') {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id,
      error: { code: -32603, message: 'tools unavailable' } }) + '\n');
    return;
  }
  if (request.method === 'resources/list') {
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id,
      result: { resources: [] } }) + '\n');
    return;
  }
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id,
    error: { code: -32601, message: 'Method not found' } }) + '\n');
}
readline.on('line', handle);
"#;

/// 收到 `release` 文件后才应答握手：稳定制造加载中的停止窗口。
const GATED_SERVER: &str = r#"
const fs = require('node:fs');
fs.appendFileSync('starts', process.cwd() + '\n');
const readline = require('node:readline').createInterface({ input: process.stdin });
let gated = true;
const pending = [];
function handle(line) {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  let result = {};
  if (request.method === 'initialize') result = {
    protocolVersion: '2025-11-25', capabilities: {},
    serverInfo: { name: 'gated-fixture', version: '1' },
  };
  if (request.method === 'tools/list') result = { tools: [] };
  if (request.method === 'resources/list') result = { resources: [] };
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
}
readline.on('line', line => {
  if (gated) { pending.push(line); } else { handle(line); }
});
const timer = setInterval(() => {
  if (!fs.existsSync('release')) return;
  clearInterval(timer);
  gated = false;
  while (pending.length) handle(pending.shift());
}, 20);
"#;

struct Fixture {
    _dir: tempfile::TempDir,
    cwd: PathBuf,
    pool: Arc<McpClientPool>,
    tasks: McpTaskOwner,
}

impl Fixture {
    fn new(script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::write(cwd.join("server.js"), script).unwrap();
        let (tasks, spawner) = McpTaskOwner::new();
        let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
        pool.bind_execution_cwd(&cwd).unwrap();
        let config: crate::mcp::McpServerConfig = serde_json::from_value(serde_json::json!({
            "command": "node",
            "args": ["server.js"],
            "dynamic": true,
        }))
        .unwrap();
        pool.configs.write().insert("lazy".into(), config);
        Self {
            _dir: dir,
            cwd,
            pool,
            tasks,
        }
    }

    fn starts(&self) -> usize {
        std::fs::read_to_string(self.cwd.join("starts"))
            .map(|content| content.lines().count())
            .unwrap_or(0)
    }

    fn release(&self) {
        std::fs::write(self.cwd.join("release"), "").unwrap();
    }

    async fn wait_for_starts(&self, expected: usize) {
        wait_until(|| self.starts() >= expected, "等待 stdio 进程启动超时").await;
    }

    async fn wait_for_idle(&self) {
        wait_until(|| self.tasks.active_count() == 0, "加载任务未在超时内退出").await;
        wait_until(
            || self.pool.retained_service_count() == 0,
            "加载失败后仍保留 service owner",
        )
        .await;
    }

    fn slot_resources(&self) -> (usize, usize) {
        let slot = self
            .pool
            .existing_server_slot("lazy")
            .expect("fixture slot must exist");
        #[cfg(not(target_os = "emscripten"))]
        let processes = slot.processes.lock().len();
        #[cfg(target_os = "emscripten")]
        let processes = 0;
        let services = slot.services.lock().len();
        (services, processes)
    }

    async fn shutdown(mut self) {
        self.pool.begin_shutdown();
        self.tasks.begin_shutdown();
        let _ = self.tasks.shutdown().await;
        assert!(self.pool.shutdown().await.is_complete());
    }
}

async fn wait_until(mut condition: impl FnMut() -> bool, message: &str) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{message}");
}

fn spawn_load(
    pool: &Arc<McpClientPool>,
) -> tokio::task::JoinHandle<Result<(), super::McpPoolError>> {
    let pool = pool.clone();
    tokio::spawn(async move { pool.ensure_loaded("lazy").await })
}

/// live discovery 失败：不提交 Connected，未提交连接当场收口，可重新发起 attempt。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_discovery_failure_releases_the_connection_and_can_retry() {
    let fixture = Fixture::new(DISCOVERY_FAIL_SERVER);
    let error = fixture.pool.ensure_loaded("lazy").await.unwrap_err();
    assert!(
        error.to_string().contains("tools unavailable"),
        "发现失败必须保留协议原因，实际: {error}"
    );

    let handle = fixture.pool.client("lazy").expect("失败必须发布状态");
    assert!(
        matches!(&handle.status, ClientStatus::Failed(reason) if reason.contains("工具发现失败")),
        "发现失败不得伪装成空工具 Connected，实际: {:?}",
        handle.status
    );
    assert!(handle.tools.is_empty());
    fixture.wait_for_idle().await;
    assert_eq!(fixture.slot_resources(), (0, 0), "失败连接不得留在保留表");
    assert!(fixture
        .pool
        .server_slot("lazy")
        .load_attempt
        .lock()
        .is_none());
    assert_eq!(fixture.starts(), 1);

    let retry = fixture.pool.ensure_loaded("lazy").await.unwrap_err();
    assert!(retry.to_string().contains("tools unavailable"));
    assert_eq!(fixture.starts(), 2, "失败后显式调用必须能发起新 attempt");
    fixture.wait_for_idle().await;
    assert_eq!(fixture.slot_resources(), (0, 0), "重试不得累积连接");
    fixture.shutdown().await;
}

/// 加载中 stop：等待方得到终态，迟到握手不能提交，也不留下任务或连接。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_during_load_cannot_be_revived_by_a_late_connection() {
    let fixture = Fixture::new(GATED_SERVER);
    let waiter = spawn_load(&fixture.pool);
    fixture.wait_for_starts(1).await;

    fixture
        .pool
        .close_server_connection("lazy", true)
        .await
        .unwrap();
    assert!(fixture
        .pool
        .server_slot("lazy")
        .stopped
        .load(Ordering::Acquire));
    // 释放底层握手：即使之后完成，也不得提交进已停止的 server。
    fixture.release();
    let error = waiter.await.unwrap().unwrap_err();
    assert!(
        error.to_string().contains("cancelled") || error.to_string().contains("connection"),
        "停止必须给出稳定失败，实际: {error}"
    );

    if let Some(handle) = fixture.pool.client("lazy") {
        assert!(
            !matches!(handle.status, ClientStatus::Connected),
            "迟到握手不得复活已停止的 server"
        );
        assert!(!handle.call_gate.is_open(), "停止后旧调用门必须关闭");
    }
    fixture.wait_for_idle().await;
    assert_eq!(fixture.slot_resources(), (0, 0));
    assert!(
        fixture.pool.spawn_reconnect("lazy".to_string()).is_err(),
        "停止后的晚重连请求必须被拒绝"
    );
    assert!(fixture.pool.ensure_loaded("lazy").await.is_err());
    fixture.shutdown().await;
}

/// 重复 load/close：每次关闭都收口 service/process 与任务，不随轮次增长。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_load_and_close_stays_bounded() {
    let fixture = Fixture::new(READY_SERVER);
    for round in 0..3 {
        fixture.pool.ensure_loaded("lazy").await.unwrap();
        let handle = fixture.pool.client("lazy").unwrap();
        assert!(matches!(handle.status, ClientStatus::Connected));
        assert!(handle.call_gate.is_open());
        assert_eq!(handle.tools.len(), 1);

        fixture
            .pool
            .close_server_connection("lazy", true)
            .await
            .unwrap();
        let stopped = fixture.pool.client("lazy").unwrap();
        assert!(
            !matches!(stopped.status, ClientStatus::Connected),
            "round {round}: 关闭后不得保留 Connected 句柄"
        );
        assert!(!stopped.call_gate.is_open());
        fixture.wait_for_idle().await;
        assert_eq!(fixture.slot_resources(), (0, 0), "round {round}");
        assert_eq!(
            fixture.pool.clients.read().len(),
            1,
            "round {round}: 句柄表不得增长"
        );
        assert_eq!(fixture.starts(), round + 1, "round {round}");

        // 与 control restart 同一准入事实：显式恢复后才允许下一次加载。
        fixture
            .pool
            .server_slot("lazy")
            .stopped
            .store(false, Ordering::Release);
    }
    fixture.shutdown().await;
}

/// 卸载来源同时取消在建加载：迟到握手不能提交，连接与任务计数回到基线。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_the_source_cancels_load_and_rejects_late_commit() {
    let fixture = Fixture::new(GATED_SERVER);
    let waiter = spawn_load(&fixture.pool);
    fixture.wait_for_starts(1).await;

    fixture.pool.remove_server("lazy").await;
    fixture.release();
    let error = waiter.await.unwrap().unwrap_err();
    assert!(
        error.to_string().contains("cancelled") || error.to_string().contains("connection"),
        "卸载必须终止在建加载，实际: {error}"
    );
    assert!(fixture.pool.client("lazy").is_none());
    fixture.wait_for_idle().await;
    assert_eq!(fixture.slot_resources(), (0, 0));
    assert!(
        fixture.pool.ensure_loaded("lazy").await.is_err(),
        "已卸载来源不得再加载"
    );
    fixture.shutdown().await;
}
