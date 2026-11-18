//! Dynamic MCP 的错误、停止竞争与资源清理（plan 10 / 12-B6）。
//!
//! 覆盖：live discovery 失败后收口未提交连接并可重试；加载中 stop 不留下句柄、
//! 任务或保留连接；重复 load/close 不增长；来源移除拒绝迟到提交。

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use super::McpServiceWrapper;
use super::{Arc, ClientStatus, McpClientPool};
use crate::mcp::client::McpClientHandle;
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

fn connected_handle(name: &str) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        call_gate: Default::default(),
        name: name.into(),
        version: None,
        cache_version: None,
        connected_at: None,
        protocol_version: None,
        peer: None,
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: Default::default(),
        source: None,
        url: None,
        skills_capable: false,
    })
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

/// 卸载先完成：迟到的提交必须被准入拒绝，不得复活已 unload 的 server。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commit_after_removal_is_rejected() {
    let fixture = Fixture::new(READY_SERVER);
    fixture.pool.remove_server("lazy").await;

    let rejected = fixture.pool.try_commit_connection(
        "lazy".into(),
        connected_handle("lazy"),
        McpServiceWrapper::Closed,
    );
    assert!(rejected.is_err(), "卸载后提交必须失败，实际已发布");
    assert!(fixture.pool.client("lazy").is_none());
    assert!(fixture.pool.services.lock().is_empty());
    assert_eq!(fixture.slot_resources(), (0, 0));
    fixture.shutdown().await;
}

/// 提交已过准入、尚未发布时并发卸载：两者必须由同一把准入锁串行化。
///
/// 旧实现里 `remove_server` 不持准入锁，会先移除配置并完成清理；提交恢复后
/// 仍发布 Connected，`discard_uncommitted_resources` 因看到 Connected 跳过收口。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removal_waits_for_an_admitted_commit_and_cleans_its_publication() {
    let fixture = Fixture::new(READY_SERVER);
    let pool = fixture.pool.clone();
    // 停点：提交已通过准入检查，但尚未写入 handle/service。
    let publication_pause = pool.pause_handle_generation();
    let commit_pool = pool.clone();
    let commit = tokio::spawn(async move {
        commit_pool
            .try_commit_connection_guarded(
                "lazy".into(),
                connected_handle("lazy"),
                McpServiceWrapper::Closed,
                || true,
            )
            .is_ok()
    });
    // 观察提交确实已持有准入锁并停在发布前，避免用 sleep 猜窗口。
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let held = pool.lifecycle_registration.try_lock().is_none();
        if held {
            break;
        }
        assert!(Instant::now() < deadline, "提交未在超时内取得准入锁");
        tokio::task::yield_now().await;
    }

    let remove_pool = pool.clone();
    let (removed_tx, removed_rx) = tokio::sync::oneshot::channel();
    let remove = tokio::spawn(async move {
        remove_pool.remove_server("lazy").await;
        let _ = removed_tx.send(());
    });
    // 修复后移除只能阻塞在准入锁上；旧实现会在提交恢复前完成卸载。
    let removed_before_commit = tokio::time::timeout(Duration::from_secs(2), removed_rx)
        .await
        .is_ok();

    drop(publication_pause);
    let committed = commit.await.unwrap();
    remove.await.unwrap();

    assert!(committed, "已过准入的提交必须完成发布");
    assert!(
        !removed_before_commit,
        "移除不得先于已准入的提交完成（两者未共用准入锁）"
    );
    assert!(
        pool.client("lazy").is_none(),
        "已卸载 server 不得残留可调用句柄"
    );
    assert!(
        pool.services.lock().is_empty(),
        "已卸载 server 不得残留 service"
    );
    assert_eq!(fixture.slot_resources(), (0, 0));
    fixture.shutdown().await;
}
