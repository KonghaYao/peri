//! 首次加载 attempt 的共享与生命周期（plan 6.1–6.3 / B3）。
//!
//! 并发调用共享一次连接；失败共享同一结果而不是逐个重试；单人取消不影响
//! 其他等待者；全部取消与 close 能收口且迟到结果无法提交。

use super::{Arc, ClientStatus, McpClientPool};
use crate::mcp::task_scope::McpTaskOwner;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// 收到 `release` 文件后才应答 initialize，用于稳定制造并发叠加。
const SLOW_SERVER: &str = r#"
const fs = require('node:fs');
fs.appendFileSync('starts', process.cwd() + '\n');
const readline = require('node:readline').createInterface({ input: process.stdin });
let gated = true;
const pending = [];
function handle(line) {
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
    serverInfo: { name: 'load-attempt-fixture', version: '1' },
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

/// 只登记启动，收到 `release` 后以非零码退出：连接失败可在并发窗口内稳定复现。
const FAILING_SERVER: &str = r#"
const fs = require('node:fs');
fs.appendFileSync('starts', process.cwd() + '\n');
const timer = setInterval(() => {
  if (!fs.existsSync('release')) return;
  clearInterval(timer);
  process.exit(1);
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
        for _ in 0..500 {
            if self.starts() >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("等待 stdio 进程启动超时，实际 {} 次", self.starts());
    }

    async fn wait_for_attempt_cleared(&self) {
        for _ in 0..500 {
            if self.pool.server_slot("lazy").load_attempt.lock().is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("加载 attempt 未在超时内收尾");
    }

    async fn shutdown(mut self) {
        self.pool.begin_shutdown();
        self.tasks.begin_shutdown();
        let _ = self.tasks.shutdown().await;
        assert!(self.pool.shutdown().await.is_complete());
    }
}

fn spawn_load(
    pool: &Arc<McpClientPool>,
) -> tokio::task::JoinHandle<Result<(), super::McpPoolError>> {
    let pool = pool.clone();
    tokio::spawn(async move { pool.ensure_loaded("lazy").await })
}

/// 并发加载只建立一条连接；就绪后的显式调用直接复用，不再 initialize。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_loads_share_one_connection() {
    let fixture = Fixture::new(SLOW_SERVER);
    let waiters: Vec<_> = (0..4).map(|_| spawn_load(&fixture.pool)).collect();
    fixture.wait_for_starts(1).await;
    // 让其余调用者进入同一 attempt（而不是在失败/成功后各自重试）。
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fixture.starts(), 1, "并发加载不得重复启动 stdio 进程");

    fixture.release();
    for waiter in waiters {
        waiter.await.unwrap().unwrap();
    }
    let handle = fixture.pool.client("lazy").expect("加载成功后必须有句柄");
    assert!(matches!(handle.status, ClientStatus::Connected));
    assert!(handle.call_gate.is_open());

    fixture
        .pool
        .ensure_loaded("lazy")
        .await
        .expect("已就绪必须复用连接");
    assert_eq!(fixture.starts(), 1, "已 ready 不得重复 initialize");
    fixture.shutdown().await;
}

/// 同一 attempt 的失败由所有等待者共享；不会逐个重试建立新连接。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_failures_share_one_attempt() {
    let fixture = Fixture::new(FAILING_SERVER);
    let waiters: Vec<_> = (0..3).map(|_| spawn_load(&fixture.pool)).collect();
    fixture.wait_for_starts(1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    fixture.release();

    let mut errors = Vec::new();
    for waiter in waiters {
        errors.push(waiter.await.unwrap().unwrap_err().to_string());
    }
    assert!(
        errors.windows(2).all(|pair| pair[0] == pair[1]),
        "等待者必须收到同一个失败，实际: {errors:?}"
    );
    assert_eq!(fixture.starts(), 1, "失败不得触发逐请求重连");
    let handle = fixture.pool.client("lazy").expect("失败必须发布状态");
    assert!(matches!(handle.status, ClientStatus::Failed(_)));
    fixture.shutdown().await;
}

/// 单个等待者取消只退出自己的等待，其他等待者仍共享原 attempt。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_one_waiter_keeps_the_shared_attempt() {
    let fixture = Fixture::new(SLOW_SERVER);
    let cancelled = spawn_load(&fixture.pool);
    fixture.wait_for_starts(1).await;
    let survivor = spawn_load(&fixture.pool);
    tokio::time::sleep(Duration::from_millis(100)).await;

    cancelled.abort();
    fixture.release();
    survivor.await.unwrap().unwrap();
    assert_eq!(fixture.starts(), 1, "单人取消不得重建连接");
    assert!(matches!(
        fixture
            .pool
            .client("lazy")
            .map(|handle| handle.status.clone()),
        Some(ClientStatus::Connected)
    ));
    fixture.shutdown().await;
}

/// 全部等待者退出会取消未完成加载；之后的显式调用可以重新发起 attempt。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_all_waiters_cancels_the_attempt() {
    let fixture = Fixture::new(SLOW_SERVER);
    let cancelled = spawn_load(&fixture.pool);
    fixture.wait_for_starts(1).await;
    cancelled.abort();
    fixture.wait_for_attempt_cleared().await;
    assert!(
        fixture.pool.client("lazy").is_none(),
        "取消不得发布连接句柄"
    );

    let retry = spawn_load(&fixture.pool);
    fixture.wait_for_starts(2).await;
    fixture.release();
    retry.await.unwrap().unwrap();
    assert_eq!(fixture.starts(), 2, "下一次显式调用必须发起新 attempt");
    fixture.shutdown().await;
}

/// close 取消 attempt 并拒绝迟到结果：不会出现 Connected 句柄或开放 call gate。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn close_rejects_late_load_results() {
    let fixture = Fixture::new(SLOW_SERVER);
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

    // 释放底层连接：即使握手随后完成，也不得提交进已停止的 server。
    fixture.release();
    let error = waiter.await.unwrap().unwrap_err();
    assert!(
        error.to_string().contains("cancelled"),
        "close 后等待者必须得到取消结果，实际: {error}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    if let Some(handle) = fixture.pool.client("lazy") {
        assert!(
            !matches!(handle.status, ClientStatus::Connected),
            "迟到结果不得提交为 Connected"
        );
        assert!(
            !handle.call_gate.is_open(),
            "迟到结果不得留下开放 call gate"
        );
    }
    fixture.shutdown().await;
}
