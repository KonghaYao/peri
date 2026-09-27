//! MCP 实例隔离契约测试（主 plan §6 D-03 / 验收契约 5，W5）。
//!
//! 这是**外部集成测试**（`tests/`，cargo 自动发现），因此只使用 `peri-middlewares`
//! 的 `pub` API：跳过 crate 内 readiness / system_tools seam（那些由 D-02 覆盖）。
//! fixture 沿用仓库既有 MCP stdio fixture 约定（`command: "node"` + 临时目录内脚本，
//! 见 `mcp/initialize_test.rs`），并在文件内自定义、不建共享 helper。
//!
//! # 断言范围（当前实现可观察的部分）
//!
//! - **独立 pool entry**：同一 pool 内每个 server name 一个 `clients` 条目，逐条目可见
//!   （`get_client` / `get_all_clients` / `all_server_infos` / `snapshot`）；
//! - **独立 `McpClientHandle`**：两台 server 的句柄是不同 `Arc`，各自携带自己那次
//!   `initialize` 的身份（`version` 来自各自 serverInfo）；
//! - **独立 transport wire**：两台 server 是两个真实子进程（各自独立 pid），每台只收到
//!   自己的 JSON-RPC 请求；
//! - **namespace 路由**：`mcp__{server}__{tool}` 只向所属 server 发 `tools/call`，
//!   wire 上使用的是该 server 的原始工具名；
//! - **无隐式跨 MCP 调用**：调用 A 不会在 B 的 wire 上产生任何请求，也不出现二次调用。
//!
//! # PARTIAL —— 这些断言**不能**支撑「契约 5 完成」（主 plan §8 的 PARTIAL 分级）
//!
//! - **凭据隔离：未验证（UNVERIFIED）**。`McpClientHandle` 没有 credential 字段，
//!   凭证存储（`FileCredentialStore`）没有可安全读取的 per-instance identity。
//!   本文件不断言、也无从断言两台 server 的凭据不共享；不使用真实 secret，也不比较、
//!   打印任何凭据值。
//! - **capability root 隔离：未验证（UNVERIFIED）**。`McpClientPool::capability_profile`
//!   是 pool-wide 字段且非 public，`McpConnectionKey` 亦非 public，本文件无法读取或比较
//!   它们（因此也**未**断言 capability root 不共享）。
//! - **五个目标 MCP 未迁移**。Workspace / Artifact / Web / Cron / LSP 五个生产实例尚不存在，
//!   本文件只覆盖「已落地连接的局部隔离」（两台 fixture），不代表契约 5 全文，也不代表
//!   契约 2/3/4（ready gate、direct 注入、空数组语义分别由 B-07 / D-02 负责）。

use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

use peri_acp_types::ports::McpPoolPort;
use peri_agent::tools::{BaseTool, ToolContext};
use peri_middlewares::{
    mcp::{
        build_tool_bridges, ClientStatus, McpClientHandle, McpClientPool, McpInitStatus,
        McpTaskOwner, McpToolBridge,
    },
    process_env::{self, EnvLockFile},
};
use serde_json::{json, Map, Value};

/// 每台 fixture server 的 stdio MCP 实现（node）：
/// - 每个收到的 JSON-RPC 行按原样追加到**自己**的 wire 日志（`#recv <payload>`）；
/// - `server/discover` 一律 -32601，让客户端 Auto 回退到 `initialize`
///   （与 `initialize_test.rs` 的 legacy fixture 行为一致）；
/// - 只声明并实现自己那一个工具，返回值带自己的身份，使「调用打到哪台 server」
///   在客户端返回值与 wire 日志两侧都可核对。
const FIXTURE_SERVER_JS: &str = r#"
const fs = require('node:fs');
const readline = require('node:readline');

const server = process.env.FIXTURE_SERVER;
const tool = process.env.FIXTURE_TOOL;
const version = process.env.FIXTURE_VERSION;
const logPath = process.env.FIXTURE_LOG;

const log = (line) => fs.appendFileSync(logPath, `${line}\n`);
log(`#boot ${server} pid=${process.pid}`);

const rl = readline.createInterface({ input: process.stdin });
rl.on('line', (line) => {
  log(`#recv ${line}`);
  let request;
  try {
    request = JSON.parse(line);
  } catch (error) {
    return;
  }
  if (request.id === undefined) return;
  const reply = (result) =>
    process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', id: request.id, result })}\n`);
  const refuse = (code, message) =>
    process.stdout.write(
      `${JSON.stringify({ jsonrpc: '2.0', id: request.id, error: { code, message } })}\n`,
    );
  switch (request.method) {
    case 'initialize':
      reply({ protocolVersion: '2025-11-25', capabilities: {}, serverInfo: { name: server, version } });
      break;
    case 'tools/list':
      reply({
        tools: [
          {
            name: tool,
            description: `${server} fixture tool`,
            inputSchema: { type: 'object', properties: {} },
          },
        ],
      });
      break;
    case 'tools/call':
      reply({ content: [{ type: 'text', text: `${server}:${tool}:ok` }] });
      break;
    case 'resources/list':
      reply({ resources: [] });
      break;
    case 'ping':
      reply({});
      break;
    default:
      refuse(-32601, 'Method not found');
  }
});
"#;

#[derive(Clone, Copy)]
struct Instance {
    server: &'static str,
    tool: &'static str,
    version: &'static str,
}

const INSTANCE_A: Instance = Instance {
    server: "iso-a",
    tool: "tool_a",
    version: "1.0.0-a",
};
const INSTANCE_B: Instance = Instance {
    server: "iso-b",
    tool: "tool_b",
    version: "1.0.0-b",
};

fn effective_name(instance: Instance) -> String {
    format!("mcp__{}__{}", instance.server, instance.tool)
}

/// 临时 HOME：`run_initialize` 走的是生产加载路径，会读真实的 `~/.peri/settings.json`
/// 与凭证存储；不隔离就会去启动开发者本机配置的 MCP server（可能带真实凭据）。
/// 进程级互斥沿用仓库既有 `EnvLockFile`（`Drop` 复原 `HOME` 时仍持锁）。
struct EnvIsolation {
    _lock: EnvLockFile,
    previous: Option<OsString>,
}

impl EnvIsolation {
    fn set(home: &Path) -> Self {
        let lock = process_env::lock().expect("process env lock");
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for EnvIsolation {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

struct IsolationFixture {
    _dir: tempfile::TempDir,
    _env: EnvIsolation,
    pool: Arc<McpClientPool>,
    tasks: McpTaskOwner,
    logs: [PathBuf; 2],
}

impl IsolationFixture {
    fn log_path(&self, instance: Instance) -> &Path {
        if instance.server == INSTANCE_A.server {
            &self.logs[0]
        } else {
            &self.logs[1]
        }
    }

    fn wire(&self, instance: Instance) -> String {
        std::fs::read_to_string(self.log_path(instance)).unwrap_or_default()
    }

    /// `notifications/initialized` 这类通知也会落盘，因此按 method 精确取用。
    fn requests(&self, instance: Instance) -> Vec<Value> {
        self.wire(instance)
            .lines()
            .filter_map(|line| line.strip_prefix("#recv "))
            .filter_map(|payload| serde_json::from_str::<Value>(payload).ok())
            .collect()
    }

    fn methods(&self, instance: Instance) -> BTreeSet<String> {
        self.requests(instance)
            .iter()
            .filter_map(|request| request["method"].as_str().map(str::to_string))
            .collect()
    }

    /// 该实例 wire 上收到的 `tools/call` 原始工具名（wire 上不得出现 effective name）。
    fn tool_call_names(&self, instance: Instance) -> Vec<String> {
        self.requests(instance)
            .iter()
            .filter(|request| request["method"] == "tools/call")
            .filter_map(|request| request["params"]["name"].as_str().map(str::to_string))
            .collect()
    }

    fn boot_pid(&self, instance: Instance) -> Option<String> {
        self.wire(instance)
            .lines()
            .find_map(|line| line.strip_prefix("#boot "))
            .map(str::to_string)
    }

    fn connected(&self, instance: Instance) -> Arc<McpClientHandle> {
        match self.pool.get_client(instance.server) {
            Some(handle) if matches!(handle.status, ClientStatus::Connected) => handle,
            other => panic!(
                "{} 未建立独立连接: {:?}\nwire 日志:\n{}",
                instance.server,
                other.as_ref().map(|handle| handle.status.clone()),
                self.wire(instance),
            ),
        }
    }

    async fn shutdown(&mut self) {
        self.pool.begin_shutdown();
        self.tasks.begin_shutdown();
        let _ = self.tasks.shutdown().await;
        assert!(
            self.pool.shutdown().await.is_complete(),
            "两台已落地连接都应能收尾"
        );
    }
}

/// 两台真实 stdio MCP server（各自独立子进程 / transport / wire 日志）+ 真实配置加载：
/// `run_initialize` 是唯一对 crate 外部可见的初始化入口，配置经 `{cwd}/.mcp.json` 注入，
/// plugin 加载目录指向临时 `claude_home`，`HOME` 指向临时目录以免碰到本机配置。
async fn isolation_fixture() -> IsolationFixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let claude_home = dir.path().join("claude");
    let cwd = dir.path().join("project");
    for path in [&home, &claude_home, &cwd] {
        std::fs::create_dir_all(path).unwrap();
    }
    let env = EnvIsolation::set(&home);

    let script = dir.path().join("fixture-mcp.cjs");
    std::fs::write(&script, FIXTURE_SERVER_JS).unwrap();
    let logs = [
        dir.path().join("wire-iso-a.log"),
        dir.path().join("wire-iso-b.log"),
    ];

    let mut servers = Map::new();
    for (index, instance) in [INSTANCE_A, INSTANCE_B].into_iter().enumerate() {
        servers.insert(
            instance.server.to_string(),
            json!({
                "command": "node",
                "args": [script.to_string_lossy()],
                "env": {
                    "FIXTURE_SERVER": instance.server,
                    "FIXTURE_TOOL": instance.tool,
                    "FIXTURE_VERSION": instance.version,
                    "FIXTURE_LOG": logs[index].to_string_lossy(),
                },
            }),
        );
    }
    std::fs::write(
        cwd.join(".mcp.json"),
        json!({ "mcpServers": servers }).to_string(),
    )
    .unwrap();

    let (tasks, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::run_initialize(pool.clone(), &cwd, &claude_home, status_tx, None, None).await;

    IsolationFixture {
        _dir: dir,
        _env: env,
        pool,
        tasks,
        logs,
    }
}

async fn invoke_named(bridges: &[Box<dyn BaseTool>], name: &str) -> String {
    let bridge = bridges
        .iter()
        .find(|bridge| bridge.name() == name)
        .unwrap_or_else(|| panic!("工具列表里没有 {name}"));
    bridge
        .invoke(json!({}), ToolContext::new(&[], "."))
        .await
        .unwrap_or_else(|error| panic!("{name} 调用失败: {error}"))
}

/// pool 条目 / 句柄身份 / 工具目录 / 宿主投影都按 server 逐实例分离。
#[tokio::test]
async fn distinct_instances_keep_distinct_pool_entries_and_handle_identity() {
    let mut fixture = isolation_fixture().await;

    let a = fixture.connected(INSTANCE_A);
    let b = fixture.connected(INSTANCE_B);
    assert!(
        !Arc::ptr_eq(&a, &b),
        "两台 server 必须各持一个独立句柄，而不是共享同一份 Arc"
    );
    assert_eq!(
        (a.name.as_str(), b.name.as_str()),
        (INSTANCE_A.server, INSTANCE_B.server)
    );
    // 句柄身份来自各自的 initialize 响应：串线会立刻表现为版本相同。
    assert_eq!(a.version.as_deref(), Some(INSTANCE_A.version));
    assert_eq!(b.version.as_deref(), Some(INSTANCE_B.version));
    assert!(
        a.peer.is_some() && b.peer.is_some(),
        "已连接句柄必须各自持有自己的 peer"
    );

    // 工具目录按 server 分层：每台只列出自己的工具，不出现对方的工具名。
    let names = |handle: &Arc<McpClientHandle>| -> Vec<String> {
        handle
            .tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect()
    };
    assert_eq!(names(&a), vec![INSTANCE_A.tool.to_string()]);
    assert_eq!(names(&b), vec![INSTANCE_B.tool.to_string()]);
    assert_eq!(
        fixture
            .pool
            .get_tools(INSTANCE_A.server)
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>(),
        vec![INSTANCE_A.tool.to_string()],
        "按 server 取工具不得命中文档里的另一台"
    );

    let mut connected: Vec<String> = fixture
        .pool
        .get_all_clients()
        .iter()
        .map(|handle| handle.name.clone())
        .collect();
    connected.sort();
    assert_eq!(
        connected,
        vec![INSTANCE_A.server.to_string(), INSTANCE_B.server.to_string()]
    );

    // 宿主可观察投影（面板 / `mcp/list` 命令面）逐 server 一条，不合并成一条。
    let mut infos: Vec<(String, String, usize)> = fixture
        .pool
        .all_server_infos()
        .iter()
        .map(|info| {
            (
                info.name.clone(),
                info.transport_type.clone(),
                info.tool_count,
            )
        })
        .collect();
    infos.sort();
    assert_eq!(
        infos,
        vec![
            (INSTANCE_A.server.to_string(), "stdio".to_string(), 1),
            (INSTANCE_B.server.to_string(), "stdio".to_string(), 1),
        ]
    );
    let snapshot = fixture.pool.snapshot();
    assert_eq!(snapshot["initPhase"], "ready");
    let snapshot_servers: BTreeSet<String> = snapshot["servers"]
        .as_array()
        .expect("snapshot.servers 必须是数组")
        .iter()
        .filter_map(|server| server["name"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        snapshot_servers,
        BTreeSet::from([INSTANCE_A.server.to_string(), INSTANCE_B.server.to_string()])
    );

    fixture.shutdown().await;
}

/// namespace 路由 + wire 不串 + 无隐式跨 MCP 调用。
#[tokio::test]
async fn each_instance_wire_carries_only_its_own_requests() {
    let mut fixture = isolation_fixture().await;

    // 生产 bridge 构造入口：从 pool 的已连接句柄出发生成 `mcp__{server}__{tool}`。
    let bridges = build_tool_bridges(&fixture.pool);
    let mut names: Vec<String> = bridges
        .iter()
        .map(|bridge| bridge.name().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![effective_name(INSTANCE_A), effective_name(INSTANCE_B)]
    );

    let produced_a = invoke_named(&bridges, &effective_name(INSTANCE_A)).await;
    let produced_b = invoke_named(&bridges, &effective_name(INSTANCE_B)).await;
    assert!(
        produced_a.contains("iso-a:tool_a:ok"),
        "A 的调用返回值必须带 A 的身份: {produced_a}"
    );
    assert!(
        produced_b.contains("iso-b:tool_b:ok"),
        "B 的调用返回值必须带 B 的身份: {produced_b}"
    );

    // wire 上只出现所属 server 的原始工具名（effective name 只存在于模型侧）。
    assert_eq!(
        fixture.tool_call_names(INSTANCE_A),
        vec![INSTANCE_A.tool.to_string()]
    );
    assert_eq!(
        fixture.tool_call_names(INSTANCE_B),
        vec![INSTANCE_B.tool.to_string()]
    );

    // 正向对照：同一份日志里必须能读到自己的标识，下面的「不出现对方标识」才不是空断言。
    let wire_a = fixture.wire(INSTANCE_A);
    let wire_b = fixture.wire(INSTANCE_B);
    assert!(
        wire_a.contains("iso-a") && wire_b.contains("iso-b"),
        "wire 日志必须各自记录自己的实例标识:\nA:\n{wire_a}\nB:\n{wire_b}"
    );

    // 无隐式跨 MCP 调用：A 的 wire 上不出现 B 的任何标识，反之亦然。
    assert!(
        !wire_a.contains("iso-b") && !wire_a.contains(INSTANCE_B.tool),
        "A 的 wire 上出现了 B 的标识（跨实例串线）:\n{wire_a}"
    );
    assert!(
        !wire_b.contains("iso-a") && !wire_b.contains(INSTANCE_A.tool),
        "B 的 wire 上出现了 A 的标识（跨实例串线）:\n{wire_b}"
    );

    // 两台分别是独立进程，且各自完成了自己的握手与工具清单（不是借来的目录）。
    let (pid_a, pid_b) = (fixture.boot_pid(INSTANCE_A), fixture.boot_pid(INSTANCE_B));
    assert!(
        pid_a.is_some() && pid_b.is_some(),
        "两台 fixture 都必须启动"
    );
    assert_ne!(pid_a, pid_b, "两台 server 必须是两个独立进程");
    for instance in [INSTANCE_A, INSTANCE_B] {
        let methods = fixture.methods(instance);
        assert!(
            methods.contains("initialize") && methods.contains("tools/list"),
            "{} 必须自己走完 initialize + tools/list: {methods:?}",
            instance.server
        );
    }

    fixture.shutdown().await;
}

/// 关闭一台实例不影响另一台的 transport 与句柄身份。
#[tokio::test]
async fn disabling_one_instance_leaves_the_other_transport_intact() {
    let mut fixture = isolation_fixture().await;

    let a_before = fixture.connected(INSTANCE_A);
    let b_before = fixture.connected(INSTANCE_B);
    // 在 A 被关闭前构造 A 的 bridge：模拟启动期已经持有该实例句柄的调用方。
    let stale_a: Arc<dyn BaseTool> = Arc::new(McpToolBridge::new(
        INSTANCE_A.server,
        &a_before.tools[0],
        Arc::clone(&a_before),
    ));

    fixture.pool.set_disabled(INSTANCE_A.server).await;

    let a_after = fixture
        .pool
        .get_client(INSTANCE_A.server)
        .expect("禁用只在连接层面生效，面板条目仍保留");
    assert!(matches!(a_after.status, ClientStatus::Disabled));
    assert!(a_after.peer.is_none(), "被禁用的实例不得保留 peer");

    // A 的关闭不得替换 B 的句柄：同一份 Arc、同一状态、peer 仍在。
    let b_after = fixture
        .pool
        .get_client(INSTANCE_B.server)
        .expect("B 的条目必须不受影响");
    assert!(
        Arc::ptr_eq(&b_before, &b_after),
        "B 的句柄被 A 的关闭替换了"
    );
    assert!(matches!(b_after.status, ClientStatus::Connected));
    assert!(b_after.peer.is_some());

    // 关闭的是 A 自己的 transport：A 已持有的 bridge 立即失败，且错误归属 A。
    let error = stale_a
        .invoke(json!({}), ToolContext::new(&[], "."))
        .await
        .expect_err("已关闭实例上的调用必须失败");
    assert!(
        error.to_string().contains(INSTANCE_A.server),
        "失败必须归属 {}: {error}",
        INSTANCE_A.server
    );

    // B 的 transport 仍然可用：真实 wire 上再收到一次 B 自己的 tools/call。
    let bridges = build_tool_bridges(&fixture.pool);
    let produced_b = invoke_named(&bridges, &effective_name(INSTANCE_B)).await;
    assert!(
        produced_b.contains("iso-b:tool_b:ok"),
        "B 在 A 被禁用后必须仍可调用: {produced_b}"
    );
    assert_eq!(
        fixture.tool_call_names(INSTANCE_B),
        vec![INSTANCE_B.tool.to_string()]
    );
    assert!(
        fixture.tool_call_names(INSTANCE_A).is_empty(),
        "A 已关闭，它的 wire 不该再收到调用:\n{}",
        fixture.wire(INSTANCE_A)
    );

    fixture.shutdown().await;
}
