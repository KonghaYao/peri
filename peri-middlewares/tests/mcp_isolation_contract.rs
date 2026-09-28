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
//! 最后一条用例（`instances_have_independent_transport_task_and_state`，I01 / §8 第 15 行）
//! 走**另一条路径**：不做 `PERI_MCP_BUILTIN=off`，而是按生产装配步骤注入上下文后让真实
//! builtin `cron` / `lsp` 连上，断言两者的 transport / server task / 状态对象互不共享，
//! 以及 close / reconnect / pool shutdown 的收敛面（与上面四条 stdio 用例**不同**，见其
//! 函数文档）。
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
//! - **注册表内实例已实迁，`workspace` 未迁**。`web` / `artifact` / `cron` / `lsp` 均已
//!   落地为真实 builtin 实例（`workspace` 仍是 wave 3 预留名）。本文件的
//!   `instances_have_independent_transport_task_and_state` 走**真实 builtin cron / lsp**；
//!   其余四条仍只覆盖两台 stdio fixture 的「已落地连接局部隔离」。两者都不代表契约 5
//!   全文，也不代表契约 2/3/4（ready gate、direct 注入、空数组语义分别由 B-07 / D-02 负责）。
//!
//! # builtin 路径的可达面（I01 落点核查）
//!
//! 三个「handler 级」seam 是 `pub(crate)`，外部集成目标**不可达**：
//! `mcp::builtin::dispatch::builtin_server_handler`、
//! `mcp::builtin::runtime::spawn_builtin_transport_with_handler|with_context|with_tap`、
//! `mcp::client::McpClientPool::spawn_builtin_transport`（模块 `mcp::builtin` 自身是
//! `pub(crate) mod`，`mcp::client::transport` 是私有 `mod`）。
//!
//! 这**不构成缺口**：生产装配面本来就只经公开面注入 builtin 状态，因此真实 builtin
//! cron / lsp 在本文件里可完整驱动 —— `peri_middlewares::assembly::{BuiltinInstanceContext,
//! CronInstanceInput, LspInstanceInput, create_host_lsp_pool}` +
//! `McpClientPool::{set_builtin_instance_context, run_initialize, reconnect, set_disabled,
//! remove_server, shutdown}` 全部是 `pub`。本文件**未**为测试放宽任何生产符号可见性。

use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use parking_lot::Mutex;
use peri_acp_types::{
    builtin_mcp::find as find_builtin_instance,
    lsp::LspServerConfig,
    ports::{McpPoolPort, McpPoolShutdownReport},
};
use peri_agent::tools::{BaseTool, ToolContext};
use peri_middlewares::{
    assembly::{create_host_lsp_pool, BuiltinInstanceContext, CronInstanceInput, LspInstanceInput},
    cron::{CronScheduler, CronTrigger},
    mcp::{
        build_tool_bridges, ClientStatus, McpClientHandle, McpClientPool, McpInitStatus,
        McpTaskOwner, McpToolBridge,
    },
    process_env::{self, EnvLockFile},
};
use peri_resources::lsp::{client::ServerState, pool::LspServerPool};
use rmcp::model::{CallToolRequestParams, ContentBlock};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

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

/// builtin 默认层注入的紧急闸门（A2）。`peri-middlewares` 内该常量是 `pub(crate)`，
/// 集成测试侧按字面量使用；语义 = off 时**不注入**任何 builtin 实例。
const BUILTIN_INJECTION_ENV: &str = "PERI_MCP_BUILTIN";

/// 夹具 env：临时 `HOME` + `PERI_MCP_BUILTIN` 取值由夹具指定。
///
/// `run_initialize` 走的是生产加载路径，会读真实的 `~/.peri/settings.json`
/// 与凭证存储；不隔离就会去启动开发者本机配置的 MCP server（可能带真实凭据）。
/// `PERI_MCP_BUILTIN=off` 停用 builtin 默认层注入：stdio 用例测的是两台 fixture stdio
/// server 之间的隔离，注入 `web` / `artifact` 会让 pool 变成四台（off 语义本身由本文件
/// 新增的 `builtin_injection_off_*` 用例断言，见 §5 R28）。builtin 用例（I01）反过来要求
/// **缺省注入**（变量缺失 ⇒ 注入全部已实现实例），因此取值是夹具参数而不是常量。
/// 进程级互斥沿用仓库既有 `EnvLockFile`（`Drop` 复原 env 时仍持锁）。
struct EnvIsolation {
    _lock: EnvLockFile,
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl EnvIsolation {
    /// stdio 夹具口径：`PERI_MCP_BUILTIN=off`（builtin 默认层零注入）。
    fn set(home: &Path) -> Self {
        Self::set_with_builtin_env(home, Some("off"))
    }

    /// `builtin_env = None` ⇒ **移除**该变量（缺省语义 = 注入全部已实现 builtin 实例）。
    fn set_with_builtin_env(home: &Path, builtin_env: Option<&str>) -> Self {
        let lock = process_env::lock().expect("process env lock");
        let previous = ["HOME", BUILTIN_INJECTION_ENV]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        std::env::set_var("HOME", home);
        match builtin_env {
            Some(value) => std::env::set_var(BUILTIN_INJECTION_ENV, value),
            None => std::env::remove_var(BUILTIN_INJECTION_ENV),
        }
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for EnvIsolation {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
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

/// `PERI_MCP_BUILTIN=off`（A2 的紧急闸门）在夹具侧的可观察断言：默认层**零注入**。
///
/// 这是 §5 R28 登记的、本文件唯一新增的断言（既有三条用例只复跑、断言一字不改）。
/// 观察量是三个同源的公开投影：client 目录、宿主投影（面板 / `mcp/list`）与 deferred
/// 工具目录——off 时它们都**恰为两台 fixture stdio server**，且都不含注册表里的任一
/// builtin 实例。off 的运维语义是「没有 Web/Artifact 能力」（middleware 提供面已删除，
/// 不存在回退到旧实现的路径）：本用例只断言能力面为零，不断言「退回到某个实现」。
#[tokio::test]
async fn builtin_injection_off_leaves_pool_with_exactly_the_fixture_servers() {
    let mut fixture = isolation_fixture().await;

    // 非空守卫：注册表为空会让「不含 builtin 实例」退化成永远成立的空断言。
    let builtin_instances: Vec<&str> = peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES
        .iter()
        .map(|instance| instance.name)
        .collect();
    assert!(
        !builtin_instances.is_empty(),
        "builtin 注册表为空，本用例的「零注入」失去可证伪性"
    );

    let mut client_names: Vec<String> = fixture
        .pool
        .get_all_clients()
        .iter()
        .map(|handle| handle.name.clone())
        .collect();
    client_names.sort();
    assert_eq!(
        client_names,
        vec![INSTANCE_A.server.to_string(), INSTANCE_B.server.to_string()],
        "off 时 pool 恰有两台夹具 server"
    );

    let mut info_names: Vec<String> = fixture
        .pool
        .all_server_infos()
        .iter()
        .map(|info| info.name.clone())
        .collect();
    info_names.sort();
    assert_eq!(
        info_names, client_names,
        "宿主投影（面板 / `mcp/list`）必须与 client 目录同源"
    );

    let mut bridge_names: Vec<String> = build_tool_bridges(&fixture.pool)
        .iter()
        .map(|bridge| bridge.name().to_string())
        .collect();
    bridge_names.sort();
    assert_eq!(
        bridge_names,
        vec![effective_name(INSTANCE_A), effective_name(INSTANCE_B)],
        "off 时 deferred 工具目录里不得出现任何 builtin 工具"
    );

    for instance in builtin_instances {
        assert!(
            !client_names.iter().any(|name| name == instance)
                && !info_names.iter().any(|name| name == instance),
            "off 时不得注入 builtin 实例 {instance}（零注入，不是回退到旧实现）"
        );
    }

    fixture.shutdown().await;
}

// ─── I01（主 plan §8 第 15 行 / R25）：真实 builtin cron / lsp 的隔离 ───────────────
//
// 上面四条用例走的是**两台 stdio fixture** + `PERI_MCP_BUILTIN=off`：它们证明不了真实
// builtin 实例的隔离（主 plan §0.1 IF-P3-12 / §8 第 15 行的 F6 口径）。本节的用例走
// **生产装配的完整步骤序列**（pool → `bind_execution_cwd` → 上下文注入 → `run_initialize`），
// 两个实例都是真实 `ServerHandler` + 同进程 duplex transport，且**被注入的状态对象由本
// 测试持有**（cron scheduler / host LSP pool），因此「transport / server task / 状态对象
// 是否独立」在公开面上逐条可证伪。

/// `cron` / `lsp` 实例名（与注册表 `BuiltinMcpInstance::name` 逐字一致；本文件不硬编码
/// 工具名，工具面一律从注册表派生）。
const BUILTIN_CRON: &str = "cron";
const BUILTIN_LSP: &str = "lsp";

/// 真实调用的有界等待：必须**成功**（含语言服务器首次拉起），超时即失败。
const LIVE_CALL_BOUND: Duration = Duration::from_secs(30);
/// 已收敛实例上的调用有界等待：必须**失败**，不允许既超时又不失败。
const DEAD_CALL_BOUND: Duration = Duration::from_secs(10);
/// tick **正向**等待：生产 tick 周期是 1s（`mcp::builtin::runtime::BUILTIN_TICK_INTERVAL`，
/// `pub(crate)`，读不到），取 10× 周期。
const TICK_WAIT_BOUND: Duration = Duration::from_secs(10);
/// tick **负向**窗口：3× 周期内一次触发都不出现才可判「tick task 已收敛」。
const TICK_ABSENCE_WINDOW: Duration = Duration::from_secs(3);

/// 假语言服务器（node，`Content-Length` 分帧）：
/// - 每次 spawn 向 `$ISO_LSP_SPAWNS` 追加一行 `spawned`；
/// - 对**带 `id` 的请求**向 `$ISO_LSP_REQUESTS` 追加该请求的 `method`（握手与查询都记，
///   通知如 `didOpen` / `initialized` 不记 —— 与子计划 V 的「只统计关联请求 ID 的调用」
///   口径一致）；
/// - `textDocument/documentSymbol` 回空数组，其余带 id 请求回 `null`。
///
/// 这是**语言服务器边界**上的可数事实：它把「lsp 实例的调用真的走到语言服务器」与
/// 「语言服务器进程被拉起了几次」变成文件计数，而不是 sleep 猜测。
const FIXTURE_LSP_JS: &str = r#"
const fs = require('node:fs');

const spawnLog = process.env.ISO_LSP_SPAWNS;
const methodLog = process.env.ISO_LSP_REQUESTS;
fs.appendFileSync(spawnLog, `spawned pid=${process.pid}\n`);

const reply = (id, result) => {
  const body = JSON.stringify({ jsonrpc: '2.0', id, result });
  process.stdout.write(`Content-Length: ${Buffer.byteLength(body, 'utf8')}\r\n\r\n${body}`);
};

let buffer = Buffer.alloc(0);
process.stdin.on('data', (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  for (;;) {
    const headerEnd = buffer.indexOf('\r\n\r\n');
    if (headerEnd < 0) return;
    const header = buffer.subarray(0, headerEnd).toString('latin1');
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) {
      buffer = buffer.subarray(headerEnd + 4);
      continue;
    }
    const start = headerEnd + 4;
    const length = Number(match[1]);
    if (buffer.length < start + length) return;
    const body = buffer.subarray(start, start + length).toString('utf8');
    buffer = buffer.subarray(start + length);
    let message;
    try {
      message = JSON.parse(body);
    } catch (error) {
      continue;
    }
    if (message.id === undefined) continue;
    fs.appendFileSync(methodLog, `${message.method}\n`);
    reply(message.id, message.method === 'textDocument/documentSymbol' ? [] : null);
  }
});
"#;

/// 假语言服务器的观测文件与配置构造。
struct FakeLspProbe {
    methods: PathBuf,
    spawns: PathBuf,
}

impl FakeLspProbe {
    fn new(dir: &Path) -> Self {
        Self {
            methods: dir.join("fake-lsp-methods.txt"),
            spawns: dir.join("fake-lsp-spawns.txt"),
        }
    }

    /// 把探针脚本落到临时目录后按 `node <script>` 拉起（与既有 stdio fixture 同款约定）。
    fn config(&self, dir: &Path) -> LspServerConfig {
        let script = dir.join("fixture-lsp.cjs");
        std::fs::write(&script, FIXTURE_LSP_JS).expect("写入假语言服务器脚本");
        let mut env = HashMap::new();
        env.insert(
            "ISO_LSP_SPAWNS".to_string(),
            self.spawns.to_string_lossy().into_owned(),
        );
        env.insert(
            "ISO_LSP_REQUESTS".to_string(),
            self.methods.to_string_lossy().into_owned(),
        );
        LspServerConfig {
            name: "iso-fake-lsp".to_string(),
            command: "node".to_string(),
            args: vec![script.to_string_lossy().into_owned()],
            env: Some(env),
            // `.probe` ⇒ 探针文件路由到本假服务器（`LspServerPool::server_for_file`）。
            extension_to_language: HashMap::from([(".probe".to_string(), "plaintext".to_string())]),
            initialization_options: None,
            disabled: None,
            max_restarts: None,
            startup_timeout: Some(20_000),
            source: None,
        }
    }

    /// 语言服务器进程启动次数（0 == 从未拉起）。
    fn spawns(&self) -> usize {
        std::fs::read_to_string(&self.spawns)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    /// 语言服务器边界收到的**请求** method 序列（带 id 的调用，不含通知）。
    fn methods(&self) -> Vec<String> {
        std::fs::read_to_string(&self.methods)
            .map(|text| text.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn method_count(&self, method: &str) -> usize {
        self.methods().iter().filter(|line| *line == method).count()
    }
}

/// 真实 builtin 池夹具：四实例（wave 1 的 `web` / `artifact` + wave 2 的 `cron` / `lsp`）
/// 由**缺省注入**产生，本用例只断言 `cron` / `lsp` 两个（其余实例在场不影响这些断言，
/// 反而是「关闭一个不影响其他实例」的额外见证）。
///
/// 注入的 `scheduler` / `lsp_pool` 由本夹具持有：被注入状态对象的**身份**因此可观察
/// （cron 调用是否落在同一份 scheduler、lsp 调用是否拉起同一份 pool 里的语言服务器）。
struct BuiltinIsolationFixture {
    _dir: tempfile::TempDir,
    _env: EnvIsolation,
    pool: Arc<McpClientPool>,
    tasks: McpTaskOwner,
    scheduler: Arc<Mutex<CronScheduler>>,
    triggers: mpsc::UnboundedReceiver<CronTrigger>,
    lsp_pool: Arc<LspServerPool>,
    lsp: FakeLspProbe,
    /// `.probe` 探针文件（`lsp` 实例 `documentSymbol` 的入参）。
    probe_file: PathBuf,
}

impl BuiltinIsolationFixture {
    /// 该实例的池条目（不存在即失败）。
    fn handle(&self, instance: &str) -> Arc<McpClientHandle> {
        self.pool.get_client(instance).unwrap_or_else(|| {
            let known: Vec<String> = self
                .pool
                .get_all_clients()
                .iter()
                .map(|handle| handle.name.clone())
                .collect();
            panic!("{instance} 必须已在池中，实际条目: {known:?}")
        })
    }

    /// 该实例已真实连上（未连上时后续断言全部空洞，必须先证伪这一条）。
    fn connected(&self, instance: &str) -> Arc<McpClientHandle> {
        let handle = self.handle(instance);
        assert!(
            matches!(handle.status, ClientStatus::Connected),
            "{instance} 必须是 Connected，实际: {:?}",
            handle.status
        );
        assert!(handle.peer.is_some(), "{instance} 必须持有 peer");
        handle
    }

    fn connected_count(&self) -> usize {
        self.pool
            .get_all_clients()
            .iter()
            .filter(|handle| matches!(handle.status, ClientStatus::Connected))
            .count()
    }

    /// 实例是否**仍在服务**：向它发一次必然被拒的 `tools/call`，按错误种类区分。
    ///
    /// - 仍在服务的实例由 handler 用 `invalid_params("unknown tool: ...")` 拒绝
    ///   （`McpError`）⇒ server task 仍在处理请求；
    /// - 已收敛的实例连不上 ⇒ 传输层错误（另一个变体）。
    ///
    /// 探针不依赖任何真实工具名与参数，因此对**不能触发副作用**的实例（如 `artifact`）
    /// 同样适用；两个方向都有判别力（活 ⇒ Err(McpError)，死 ⇒ 非 McpError）。
    async fn serving_probe(handle: &McpClientHandle, bound: Duration) -> Result<(), String> {
        let peer = handle
            .peer
            .clone()
            .ok_or_else(|| format!("{} 没有 peer（未连接）", handle.name))?;
        let request = CallToolRequestParams::new("__iso_liveness_probe__".to_string());
        match tokio::time::timeout(bound, peer.call_tool(request)).await {
            Err(_) => Err(format!(
                "{}::tools/call 在 {bound:?} 内没有返回",
                handle.name
            )),
            Ok(Ok(_)) => Err(format!(
                "{} 上不存在的工具名竟然调用成功（夹具假设不成立）",
                handle.name
            )),
            Ok(Err(rmcp::ServiceError::McpError(error))) => {
                assert!(
                    error.message.contains("unknown tool"),
                    "{} 必须以未知工具拒绝探针，实际: {}",
                    handle.name,
                    error.message
                );
                Ok(())
            }
            Ok(Err(error)) => Err(format!("{} 已不再服务: {error:?}", handle.name)),
        }
    }

    /// 经该句柄自己的 peer 发一次真实 `tools/call`（有界），成功时返回文本结果。
    async fn call(
        &self,
        handle: &McpClientHandle,
        tool: &str,
        arguments: Value,
        bound: Duration,
    ) -> Result<String, String> {
        let peer = handle
            .peer
            .clone()
            .ok_or_else(|| format!("{} 没有 peer（未连接）", handle.name))?;
        let request = CallToolRequestParams::new(tool.to_string())
            .with_arguments(arguments.as_object().cloned().unwrap_or_default());
        match tokio::time::timeout(bound, peer.call_tool(request)).await {
            Err(_) => Err(format!("{}::{tool} 在 {bound:?} 内没有返回", handle.name)),
            Ok(Err(error)) => Err(format!("{}::{tool} 调用失败: {error}", handle.name)),
            Ok(Ok(result)) => {
                let text = content_text(&result.content);
                if result.is_error.unwrap_or(false) {
                    Err(format!("{}::{tool} 返回错误结果: {text}", handle.name))
                } else {
                    Ok(text)
                }
            }
        }
    }

    /// `cron_list` 的真实调用（结果文本会带上该任务的 prompt）。
    async fn call_cron_list(&self, handle: &McpClientHandle) -> String {
        self.call(handle, "cron_list", json!({}), LIVE_CALL_BOUND)
            .await
            .unwrap_or_else(|error| panic!("cron 实例必须仍可调用: {error}"))
    }

    /// `lsp` 实例的真实调用（`documentSymbol` 路由到假语言服务器）。
    async fn call_lsp_document_symbol(&self, handle: &McpClientHandle) -> String {
        let arguments = json!({
            "operation": "documentSymbol",
            "file_path": self.probe_file.to_string_lossy(),
        });
        self.call(handle, "LSP", arguments, LIVE_CALL_BOUND)
            .await
            .unwrap_or_else(|error| panic!("lsp 实例必须仍可调用: {error}"))
    }

    /// 注入 scheduler 里的任务数（唯一真值来自**测试持有**的那份 scheduler）。
    fn cron_task_count(&self) -> usize {
        self.scheduler.lock().list_tasks().len()
    }

    fn cron_task_id(&self) -> String {
        let scheduler = self.scheduler.lock();
        scheduler
            .list_tasks()
            .first()
            .map(|task| task.id.clone())
            .unwrap_or_else(|| panic!("注入 scheduler 里必须已有任务"))
    }

    /// 正向 tick 观测：排空历史触发 → 把任务强制到点 → 等一次触发。
    ///
    /// 触发只会由**池侧本代 tick task**（每 1s 一次 `scheduler.lock().tick()`）产生；
    /// 本测试自己从不调 `tick()`，因此收到触发 == 该实例的 tick task 正在跑。
    async fn await_cron_tick(&mut self, label: &str) -> CronTrigger {
        while self.triggers.try_recv().is_ok() {}
        let task_id = self.cron_task_id();
        assert!(
            self.scheduler.lock().force_next_fire_to_past(&task_id),
            "[{label}] 强制到点必须命中已注册任务"
        );
        match tokio::time::timeout(TICK_WAIT_BOUND, self.triggers.recv()).await {
            Ok(Some(trigger)) => trigger,
            Ok(None) => panic!("[{label}] cron 触发通道被关闭"),
            Err(_) => panic!(
                "[{label}] {TICK_WAIT_BOUND:?} 内没有触发：cron 实例的 tick task 不在跑（或已收敛）"
            ),
        }
    }

    /// 负向 tick 观测：排空历史触发 → 强制到点 → `TICK_ABSENCE_WINDOW` 内不得有触发。
    async fn assert_no_cron_tick(&mut self, label: &str) {
        while self.triggers.try_recv().is_ok() {}
        let task_id = self.cron_task_id();
        assert!(
            self.scheduler.lock().force_next_fire_to_past(&task_id),
            "[{label}] 强制到点必须命中已注册任务"
        );
        match tokio::time::timeout(TICK_ABSENCE_WINDOW, self.triggers.recv()).await {
            Err(_) => {}
            Ok(Some(trigger)) => panic!(
                "[{label}] 已收敛的实例仍在被 tick 驱动（task={}）",
                trigger.task_id
            ),
            Ok(None) => {}
        }
    }

    /// 观测 tick 周期的**上界**：连续两次「强制到点 → 等触发」之间的间隔。
    ///
    /// 生产 tick 周期是 `pub(crate)` 常量（集成目标读不到）。负向断言「窗口内没有触发」
    /// 只有在窗口 ≥ 一个周期时才有判别力，因此这里把该前提从假设变成观测：两次相邻
    /// 触发都只能落在 tick 边界上 ⇒ 间隔 ≤ 一个周期。调用方据此断言
    /// `间隔 < TICK_ABSENCE_WINDOW`，负向窗口的有效性就不再依赖外部常量。
    async fn measure_tick_period_upper_bound(&mut self, label: &str) -> Duration {
        self.await_cron_tick(label).await;
        let started = std::time::Instant::now();
        self.await_cron_tick(label).await;
        started.elapsed()
    }
}

/// 注册表声明的原始工具名（断言一律从注册表派生，不硬编码工具名）。
fn declared_original_tools(instance: &str) -> Vec<String> {
    let mut names: Vec<String> = find_builtin_instance(instance)
        .unwrap_or_else(|| panic!("注册表必须含实例 {instance}"))
        .tools
        .iter()
        .map(|tool| tool.original_name.to_string())
        .collect();
    names.sort();
    names
}

/// 句柄上 `tools/list` 的工具名（排序后比较，顺序由 crate 内用例负责）。
fn handle_tool_names(handle: &McpClientHandle) -> Vec<String> {
    let mut names: Vec<String> = handle
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();
    names.sort();
    names
}

/// `CallToolResponse` 的文本投影（内置 handler 的成功结果都是单条文本）。
fn content_text(contents: &[ContentBlock]) -> String {
    contents
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 生产装配步骤序列 + 缺省 builtin 注入 + 测试持有的状态对象。
async fn builtin_isolation_fixture() -> BuiltinIsolationFixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let claude_home = dir.path().join("claude");
    let cwd = dir.path().join("project");
    for path in [&home, &claude_home, &cwd] {
        std::fs::create_dir_all(path).unwrap();
    }
    // 缺省注入（`PERI_MCP_BUILTIN` 缺失 ⇒ 全部已实现实例）；HOME 重定向到临时目录，
    // 因此读不到开发者本机的 `~/.claude.json` / `~/.peri/settings.json`。
    let env = EnvIsolation::set_with_builtin_env(&home, None);
    // 不写 `{cwd}/.mcp.json`：本夹具的 server 集合完全由 builtin 默认层注入产生。
    assert!(
        !cwd.join(".mcp.json").exists(),
        "本夹具不得有项目级 MCP 配置"
    );

    let probe_file = cwd.join("probe.probe");
    std::fs::write(&probe_file, "// iso probe\n").unwrap();
    let lsp = FakeLspProbe::new(dir.path());

    let (cron_trigger_tx, triggers) = mpsc::unbounded_channel();
    let scheduler = Arc::new(Mutex::new(CronScheduler::new(cron_trigger_tx)));
    let lsp_pool = create_host_lsp_pool(&cwd.to_string_lossy(), &[lsp.config(dir.path())]);
    let context = BuiltinInstanceContext::new(cwd.to_string_lossy().into_owned())
        .with_cron(CronInstanceInput {
            scheduler: Arc::clone(&scheduler),
            // `tick_enabled = true`（TUI 投影）：池的唯一 spawn 点会为本代 cron 挂 tick，
            // 使「实例的 server task / tick task 是否独立」成为可观察事实。
            tick_enabled: true,
        })
        .with_lsp(LspInstanceInput {
            pool: Arc::clone(&lsp_pool),
        });

    let (tasks, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    // 与生产装配同一步骤（`host/assemble.rs::pending_mcp_pool`）：先绑定 host cwd，
    // 再注入上下文（A33：注入必须早于 `run_initialize`）。
    pool.bind_execution_cwd(&cwd)
        .expect("夹具第一次绑定 execution cwd");
    pool.set_builtin_instance_context(Arc::new(context))
        .expect("首次注入必须成功（夹具不会二次注入）");

    let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::run_initialize(pool.clone(), &cwd, &claude_home, status_tx, None, None).await;

    BuiltinIsolationFixture {
        _dir: dir,
        _env: env,
        pool,
        tasks,
        scheduler,
        triggers,
        lsp_pool,
        lsp,
        probe_file,
    }
}

/// **真实 builtin `cron` / `lsp` 的 transport / server task / 状态对象互不共享**，
/// 且实例级 close 只收敛被关的那一个、pool 级 shutdown 才同时收敛两者。
///
/// 与上面四条 stdio 用例的差别（主 plan §8 第 15 行的 F6 口径）：那些用例
/// `PERI_MCP_BUILTIN=off` + 两台独立 stdio 子进程，**不能**作为 builtin 隔离证据；
/// 本用例的结论只来自真实 builtin 实例（同进程 duplex + 真实 `ServerHandler`），
/// 观察面全部是 `peri-middlewares` 的 `pub` API。
///
/// 观察面与判据（每条都可证伪）：
///
/// 1. **独立 transport / 句柄 / 工具面**：两个句柄是不同 `Arc`；工具面各自等于注册表
///    为该实例声明的集合（且两集合不相交）；两实例都上报 `transport = "builtin"`；调用经
///    各自的 peer 落回各自的 handler（cron 调用只改注入 scheduler，lsp 调用只拉语言服务器）。
/// 2. **独立 server task / tick task**：cron 代的 tick 触发只可能来自池侧本代 tick task
///    （测试从不自己 tick）；lsp 侧的可数事实是语言服务器进程与请求记账。
/// 3. **close 一个不动另一个**：`set_disabled` / `remove_server` 后，被关实例的旧 peer
///    有界失败、cron 的 tick 收敛；对侧句柄仍是**同一份 `Arc`**、仍 `Connected`、真实调用
///    仍成功。
/// 4. **reconnect 代数不串台**：被重连实例换新句柄（旧代 peer 失效），对侧句柄一字不动；
///    注入的状态对象跨代保持（cron 任务数不变、语言服务器进程数不变）。
/// 5. **pool 级 shutdown 才同时收敛**：两个实例同时转 `Disconnected` + `peer = None`，
///    旧 peer 全部有界失败、tick 收敛，且 shutdown 报告收口为 `Complete`（计数与关闭前
///    已连接数一致）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instances_have_independent_transport_task_and_state() {
    let mut fixture = builtin_isolation_fixture().await;

    // ── 步骤 0：装配前提（两实例真连上，否则下面的断言面是空的）──────────────────
    assert_eq!(
        fixture.pool.snapshot()["initPhase"],
        json!("ready"),
        "builtin 池必须收口为 ready（失败 ⇒ cron / lsp 未连上，断言面空洞）"
    );
    let infos = fixture.pool.all_server_infos();
    for instance in [BUILTIN_CRON, BUILTIN_LSP] {
        let info = infos
            .iter()
            .find(|info| info.name == instance)
            .unwrap_or_else(|| panic!("宿主投影必须含 {instance}"));
        assert_eq!(
            info.transport_type, "builtin",
            "{instance} 必须是 builtin 分类（同进程 transport），实际: {}",
            info.transport_type
        );
    }
    let cron = fixture.connected(BUILTIN_CRON);
    let lsp = fixture.connected(BUILTIN_LSP);
    // 正向 tick 观测计数（证据行用）。
    let mut tick_fires = 0usize;

    // ── 步骤 1：transport / 句柄 / 工具面逐实例独立 ──────────────────────────────
    assert!(
        !Arc::ptr_eq(&cron, &lsp),
        "cron 与 lsp 必须各持一个句柄，而不是共享同一份 Arc"
    );
    assert_eq!(
        (cron.name.as_str(), lsp.name.as_str()),
        (BUILTIN_CRON, BUILTIN_LSP)
    );
    for (handle, instance) in [(&cron, BUILTIN_CRON), (&lsp, BUILTIN_LSP)] {
        assert_eq!(
            handle_tool_names(handle),
            declared_original_tools(instance),
            "{instance} 的工具面必须来自自己那次握手，且等于注册表声明"
        );
    }
    let (cron_tools, lsp_tools) = (handle_tool_names(&cron), handle_tool_names(&lsp));
    assert!(
        cron_tools.iter().all(|name| !lsp_tools.contains(name)),
        "两实例的工具面不得相交（串台会把对侧工具暴露给本实例）: cron={cron_tools:?} lsp={lsp_tools:?}"
    );
    assert!(
        fixture.lsp.spawns() == 0,
        "连接阶段不得拉起语言服务器（`LspServerPool` 惰性、工具面按配置快照）"
    );

    // cron 侧真实调用：结果必须落回**测试注入**的那一份 scheduler。
    let registered = fixture
        .call(
            &cron,
            "cron_register",
            json!({ "expression": "0 3 * * *", "prompt": "iso-cron-probe" }),
            LIVE_CALL_BOUND,
        )
        .await
        .unwrap_or_else(|error| panic!("cron 实例必须可调用: {error}"));
    assert!(
        registered.contains("iso-cron-probe"),
        "cron_register 的返回必须来自本实例的工具面: {registered}"
    );
    assert_eq!(
        fixture.cron_task_count(),
        1,
        "任务必须落在被注入的那**一份** scheduler 上（0 = 不是同一份；>1 = 有两份状态）"
    );
    assert_eq!(
        fixture.lsp.methods(),
        Vec::<String>::new(),
        "cron 调用不得在语言服务器边界留下任何请求（跨实例串台）"
    );
    assert_eq!(fixture.lsp.spawns(), 0, "cron 调用不得拉起语言服务器");

    // cron 代的 tick task 正在跑（触发只可能来自池侧 tick）。
    let trigger = fixture
        .await_cron_tick("连接后 cron tick 必须由池侧驱动")
        .await;
    tick_fires += 1;
    assert_eq!(
        trigger.prompt, "iso-cron-probe",
        "触发必须属于本实例注册的任务"
    );
    // 负向断言的窗口必须 ≥ 一个 tick 周期：用**观测到的周期上界**自证，不依赖外部常量。
    let tick_period_upper = fixture
        .measure_tick_period_upper_bound("周期上界观测：相邻两次触发")
        .await;
    tick_fires += 2;
    assert!(
        tick_period_upper < TICK_ABSENCE_WINDOW,
        "负向窗口 {TICK_ABSENCE_WINDOW:?} 必须覆盖一个 tick 周期，实际观测上界 {tick_period_upper:?}"
    );

    // lsp 侧真实调用：必须走到**注入 pool 里的**假语言服务器。
    let symbols = fixture.call_lsp_document_symbol(&lsp).await;
    assert!(
        symbols.contains("No symbols found"),
        "documentSymbol 必须返回伪造服务器的空符号结果: {symbols}"
    );
    assert_eq!(
        fixture.lsp.spawns(),
        1,
        "首次 lsp 调用恰好拉起一个语言服务器"
    );
    assert_eq!(
        fixture.lsp.method_count("textDocument/documentSymbol"),
        1,
        "语言服务器边界必须恰好收到一次查询: {:?}",
        fixture.lsp.methods()
    );
    let lsp_states: Vec<_> = fixture
        .lsp_pool
        .server_info()
        .iter()
        .map(|info| (info.name.clone(), info.state.clone()))
        .collect();
    // 非空守卫：空 vec 会让下面「不得是 Stopped」的断言空洞。
    assert_eq!(
        lsp_states.len(),
        1,
        "注入 pool 恰有一个语言服务器，实际: {lsp_states:?}"
    );
    assert_eq!(lsp_states[0].0, "iso-fake-lsp");
    assert!(
        !matches!(lsp_states[0].1, ServerState::Stopped),
        "handler 必须持有**同一份**注入 pool（否则测试持有的 pool 看不到语言服务器被拉起）: {lsp_states:?}"
    );
    assert_eq!(
        fixture.cron_task_count(),
        1,
        "lsp 调用不得改动 cron 侧状态（任务集合）"
    );
    assert!(
        fixture
            .call_cron_list(&cron)
            .await
            .contains("iso-cron-probe"),
        "lsp 调用后 cron 侧任务必须仍在（同一份 scheduler）"
    );

    // ── 步骤 2：close `lsp` —— cron 不受影响 ────────────────────────────────────
    let cron_before = fixture.connected(BUILTIN_CRON);
    let stale_lsp = fixture.connected(BUILTIN_LSP);
    fixture.pool.set_disabled(BUILTIN_LSP).await;
    let lsp_disabled = fixture.handle(BUILTIN_LSP);
    assert!(
        matches!(lsp_disabled.status, ClientStatus::Disabled),
        "set_disabled 后必须是 Disabled，实际: {:?}",
        lsp_disabled.status
    );
    assert!(lsp_disabled.peer.is_none(), "被关闭实例不得保留 peer");
    // 该实例的 server task 已收敛：它自己的旧 peer 有界失败。
    assert!(
        fixture
            .call(
                &stale_lsp,
                "LSP",
                json!({ "operation": "diagnostics" }),
                DEAD_CALL_BOUND
            )
            .await
            .is_err(),
        "已关闭的 lsp 实例上，旧 peer 的调用必须失败"
    );
    // 对侧一字不动：同一份 Arc、仍 Connected、真实调用仍成功、tick 仍在跑。
    let cron_after = fixture.connected(BUILTIN_CRON);
    assert!(
        Arc::ptr_eq(&cron_before, &cron_after),
        "lsp 的关闭替换了 cron 的句柄"
    );
    assert!(
        fixture
            .call_cron_list(&cron_after)
            .await
            .contains("iso-cron-probe"),
        "lsp 关闭后 cron 必须仍可真实调用"
    );
    fixture
        .await_cron_tick("lsp 关闭后 cron 的 tick task 必须仍在跑")
        .await;
    tick_fires += 1;

    // ── 步骤 3：reconnect `lsp` —— 代数只前进被重连的实例 ───────────────────────
    let spawns_before = fixture.lsp.spawns();
    let symbols_before = fixture.lsp.method_count("textDocument/documentSymbol");
    let cron_before_reconnect = fixture.connected(BUILTIN_CRON);
    fixture
        .pool
        .reconnect(BUILTIN_LSP, None)
        .await
        .expect("lsp 重连必须成功");
    let lsp_new = fixture.connected(BUILTIN_LSP);
    assert!(
        !Arc::ptr_eq(&lsp_new, &stale_lsp),
        "重连必须换新代句柄（旧代 `Arc` 不得被复用）"
    );
    assert_eq!(
        handle_tool_names(&lsp_new),
        declared_original_tools(BUILTIN_LSP),
        "新代实例的工具面必须重新来自自己那次握手"
    );
    assert!(
        fixture
            .call(
                &stale_lsp,
                "LSP",
                json!({ "operation": "diagnostics" }),
                DEAD_CALL_BOUND
            )
            .await
            .is_err(),
        "旧代 lsp peer 在重连后必须失效"
    );
    // 对侧对象集合不串台：cron 句柄仍是同一份 Arc，任务计数不变。
    let cron_after_reconnect = fixture.connected(BUILTIN_CRON);
    assert!(
        Arc::ptr_eq(&cron_before_reconnect, &cron_after_reconnect),
        "lsp 的重连替换了 cron 的句柄（代数串台）"
    );
    assert_eq!(fixture.cron_task_count(), 1, "重连不得复制 cron 侧状态");
    // 注入的状态对象跨代保持：语言服务器**进程**不因 MCP 实例重连而重建。
    let symbols_after = fixture.call_lsp_document_symbol(&lsp_new).await;
    assert!(symbols_after.contains("No symbols found"));
    assert_eq!(
        fixture.lsp.spawns(),
        spawns_before,
        "重连不得重建 host LSP pool / 重启语言服务器（状态对象是注入的同一份）"
    );
    assert_eq!(
        fixture.lsp.method_count("textDocument/documentSymbol"),
        symbols_before + 1,
        "新代 lsp 调用必须落在同一个语言服务器进程上"
    );
    fixture
        .await_cron_tick("lsp 重连后 cron 的 tick task 必须仍在跑")
        .await;
    tick_fires += 1;

    // ── 步骤 4：close `cron` —— lsp 不受影响，且 cron 的 task 已收敛 ─────────────
    let lsp_before = fixture.connected(BUILTIN_LSP);
    let stale_cron = fixture.connected(BUILTIN_CRON);
    let symbols_before = fixture.lsp.method_count("textDocument/documentSymbol");
    fixture.pool.set_disabled(BUILTIN_CRON).await;
    let cron_disabled = fixture.handle(BUILTIN_CRON);
    assert!(matches!(cron_disabled.status, ClientStatus::Disabled));
    assert!(cron_disabled.peer.is_none());
    assert!(
        fixture
            .call(&stale_cron, "cron_list", json!({}), DEAD_CALL_BOUND)
            .await
            .is_err(),
        "已关闭的 cron 实例上，旧 peer 的调用必须失败"
    );
    // tick task 必须随实例一起收敛（否则它仍在驱动已关闭实例的状态对象）。
    fixture
        .assert_no_cron_tick("cron 关闭后 tick task 必须已收敛")
        .await;
    // 对侧不受影响：同一份 Arc、真实调用成功、语言服务器记账继续前进。
    let lsp_after = fixture.connected(BUILTIN_LSP);
    assert!(
        Arc::ptr_eq(&lsp_before, &lsp_after),
        "cron 的关闭替换了 lsp 的句柄"
    );
    let symbols_after = fixture.call_lsp_document_symbol(&lsp_after).await;
    assert!(symbols_after.contains("No symbols found"));
    assert_eq!(
        fixture.lsp.method_count("textDocument/documentSymbol"),
        symbols_before + 1,
        "cron 关闭后 lsp 调用必须仍然打到语言服务器"
    );

    // ── 步骤 5：reconnect `cron` —— 状态对象跨代保持、tick 重新挂上 ──────────────
    fixture
        .pool
        .reconnect(BUILTIN_CRON, None)
        .await
        .expect("cron 重连必须成功");
    let cron_new = fixture.connected(BUILTIN_CRON);
    assert!(!Arc::ptr_eq(&cron_new, &stale_cron), "重连必须换新代句柄");
    assert_eq!(
        fixture.cron_task_count(),
        1,
        "重连不得复制或丢失 cron 侧状态（同一份注入 scheduler）"
    );
    assert!(
        fixture
            .call_cron_list(&cron_new)
            .await
            .contains("iso-cron-probe"),
        "新代 cron 必须读到同一份状态对象里的任务"
    );
    fixture
        .await_cron_tick("cron 重连后新代的 tick task 必须重新挂上")
        .await;
    tick_fires += 1;
    let lsp_untouched = fixture.connected(BUILTIN_LSP);
    assert!(
        Arc::ptr_eq(&lsp_before, &lsp_untouched),
        "cron 的重连替换了 lsp 的句柄（代数串台）"
    );

    // ── 步骤 6：终态关闭面（`remove_server`，连带删配置）也只在被关实例上收敛 ──────
    //
    // 关闭面有两个公开入口：`set_disabled`（步骤 2/4，保留配置与面板条目）与
    // `remove_server`（终态关闭，连带删配置）。两者共用同一份物理收敛
    // （`close_builtin_task`），但**入口语义不同**，因此补一条真实 builtin 实例的
    // `remove_server` 证据。被关的必须是**在场的第三个** builtin 实例：cron / lsp 要
    // 留到步骤 7 才能断言「pool 级 shutdown 才同时收敛两者」。
    let artifact = "artifact";
    let artifact_before = fixture.connected(artifact);
    let cron_alive = fixture.connected(BUILTIN_CRON);
    let lsp_alive = fixture.connected(BUILTIN_LSP);
    // 正向对照：被关**之前**它确实在服务（否则下面的失败无法归因于关闭）。
    assert!(
        BuiltinIsolationFixture::serving_probe(&artifact_before, DEAD_CALL_BOUND)
            .await
            .is_ok(),
        "{artifact} 在 remove_server 之前必须在服务"
    );
    fixture.pool.remove_server(artifact).await;
    assert!(
        fixture.pool.get_client(artifact).is_none(),
        "remove_server 必须移除该实例的池条目（连带配置）"
    );
    assert!(
        BuiltinIsolationFixture::serving_probe(&artifact_before, DEAD_CALL_BOUND)
            .await
            .is_err(),
        "remove_server 后旧 peer 必须失效（server task 已收敛）"
    );
    for (instance, before) in [(BUILTIN_CRON, &cron_alive), (BUILTIN_LSP, &lsp_alive)] {
        let after = fixture.connected(instance);
        assert!(
            Arc::ptr_eq(before, &after),
            "{instance} 的句柄被 {artifact} 的 remove_server 替换了"
        );
    }
    assert!(
        fixture
            .call_cron_list(&cron_alive)
            .await
            .contains("iso-cron-probe"),
        "remove_server 第三方实例后 cron 必须仍可真实调用"
    );
    let symbols_before = fixture.lsp.method_count("textDocument/documentSymbol");
    let symbols = fixture.call_lsp_document_symbol(&lsp_alive).await;
    assert!(symbols.contains("No symbols found"));
    assert_eq!(
        fixture.lsp.method_count("textDocument/documentSymbol"),
        symbols_before + 1,
        "remove_server 第三方实例后 lsp 调用必须仍然打到语言服务器"
    );

    // ── 步骤 7：pool 级 shutdown 才**同时**收敛两者 ─────────────────────────────
    let services_before = fixture.connected_count();
    let cron_before_shutdown = fixture.connected(BUILTIN_CRON);
    let lsp_before_shutdown = fixture.connected(BUILTIN_LSP);
    fixture.tasks.begin_shutdown();
    let _ = fixture.tasks.shutdown().await;
    let report = fixture.pool.shutdown().await;
    match report {
        McpPoolShutdownReport::Complete {
            settled_services,
            failed_services,
        } => {
            assert_eq!(failed_services, 0, "shutdown 不得留下失败的服务");
            assert_eq!(
                settled_services, services_before,
                "pool 级 shutdown 必须收敛**全部**已连接实例（含 cron / lsp）"
            );
        },
        McpPoolShutdownReport::Incomplete {
            settled_services,
            unfinished_services,
            failed_services,
        } => panic!(
            "pool 级 shutdown 未收口: settled={settled_services} unfinished={unfinished_services} failed={failed_services}"
        ),
    }
    for instance in [BUILTIN_CRON, BUILTIN_LSP] {
        let handle = fixture.handle(instance);
        assert!(
            matches!(handle.status, ClientStatus::Disconnected),
            "{instance} 必须在 pool shutdown 后转 Disconnected，实际: {:?}",
            handle.status
        );
        assert!(handle.peer.is_none(), "{instance} 不得保留 peer");
    }
    for (instance, handle, tool, arguments) in [
        (BUILTIN_CRON, &cron_before_shutdown, "cron_list", json!({})),
        (
            BUILTIN_LSP,
            &lsp_before_shutdown,
            "LSP",
            json!({ "operation": "diagnostics" }),
        ),
    ] {
        assert!(
            fixture
                .call(handle, tool, arguments, DEAD_CALL_BOUND)
                .await
                .is_err(),
            "{instance} 的 server task 必须在 pool shutdown 后结束（旧 peer 调用必须失败）"
        );
    }
    fixture
        .assert_no_cron_tick("pool shutdown 后 cron 的 tick task 必须已收敛")
        .await;

    // 现场证据行（`--nocapture` 可见；数值供验收台账记录）。
    println!(
        "[I01 builtin isolation] handles: cron|lsp distinct Arc | transport=builtin | \
         lsp_spawns={} documentSymbol={} | cron_tasks={} tick_fires={} tick_period<={:?} \
         tick_absent=2 | close/reconnect=one-sided | shutdown=Complete(settled={services_before}, failed=0)",
        fixture.lsp.spawns(),
        fixture.lsp.method_count("textDocument/documentSymbol"),
        fixture.cron_task_count(),
        tick_fires,
        tick_period_upper,
    );

    // 收尾：语言服务器进程随 host pool 关闭（有界，不做断言——LSP pool 的收敛面
    // 由 §8 第 13 行 / R19 的 host shutdown 用例负责）。
    let _ = tokio::time::timeout(Duration::from_secs(10), fixture.lsp_pool.shutdown()).await;
}
