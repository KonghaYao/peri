use super::*;

// ── 用例 11：R29 / F02（§7.3；子计划 V §5「外层超时取消」）──────────────────────
//
// 断言面 = **桥层 120s 外加上界**（`McpToolBridge::TOOL_CALL_TIMEOUT`）在真实 builtin
// `lsp` 实例 + 真实 language server 子进程上的到期行为，以及到期后**显式 close** 的收敛。
//
// 证据边界（诚实声明）：
// - 调用面：`mcp__lsp__LSP` 的 bridge 由生产 public builder
//   （`peri_middlewares::mcp::build_tool_bridges`）从**本夹具真实 McpClientPool** 构造，
//   走的 `McpToolBridge::invoke` 是生产实现（`:265` 的 `tokio::time::timeout(120s, ..)`）。
//   模型面经 `ExecuteExtraTool` 的解析 / 绑定路径属 H02 与 §8 的覆盖面，本用例不重复取。
// - 延迟夹具：perl 假 language server 应答 `initialize` 后**不再读 stdin**；随后 `LSP`
//   工具的 `didOpen` 帧（2 MiB > 管道容量）只能填满管道缓冲，写入完成永久等待 ⇒ 该调用
//   **没有内层期限**（`LspTool::timeout() == None`；`did_open` 的 `completion.wait()`
//   本身无超时），唯一上界就是桥的 120s。这正是 R29 登记的不对称。
// - 时钟：到期段用 `tokio::time::pause()`（只作用于本用例 runtime），120s 在**虚拟时钟**
//   上到期，不真等 120 秒；夹具自身的装配 / 握手（真实子进程 I/O）在暂停时钟**之前**完成。
//   收敛段 `resume()` 回真实时钟：虚拟时钟的 auto-advance 会把真实 I/O 轮询折算成虚拟
//   时间，那样的耗时数字不能当证据（本用例因此对收敛段只断言「有界真实等待内 Complete」，
//   并打印真实耗时）。
// - 「取消」的两种读法必须分开：到期只 drop **客户端等待**（`client_await_dropped=true`），
//   **不**取消 server 侧执行（`server_side_cancelled=false`，子计划 V 的硬约束：
//   「不得把 client timeout 当作 server 自动取消成功」）。收敛由**显式 close**
//   （生产 `shutdown_host`）取得：LSP 优雅关停走完自己的 5s/1s 期限后 kill 子进程，
//   被弃置的在飞写入才被判失败（`close_elapsed` 是这段真实耗时）。

/// 延迟假 LSP 的 perl 脚本：写自证 PID → 只读一帧（`initialize`）并应答 → **不再读 stdin**。
const DELAYED_LSP_SCRIPT: &str = r#"open my $p, '>', $ENV{PERI_LSP_STALL_PID} or exit 1;
print $p "$$";
close $p;
binmode STDIN;
select STDOUT;
$| = 1;
my $h = '';
while (1) {
    my $l = <STDIN>;
    exit 1 unless defined $l;
    last if $l =~ /^\r?\n$/;
    $h .= $l;
}
my ($len) = $h =~ /Content-Length:\s*(\d+)/i;
if (defined $len) {
    my $b = '';
    while (length($b) < $len) {
        my $n = read(STDIN, my $part, $len - length($b));
        exit 1 unless $n;
        $b .= $part;
    }
    if ($b =~ /"id"\s*:\s*(\d+)/) {
        my $r = '{"jsonrpc":"2.0","id":' . $1 . ',"result":null}';
        print "Content-Length: " . length($r) . "\r\n\r\n" . $r;
    }
}
while (1) { sleep 3600; }"#;

/// didOpen 正文长度：必须远大于管道容量（macOS / Linux 均为 64 KiB 量级），
/// 否则写入会完成、挂起点消失。
const DELAYED_LSP_PAYLOAD: usize = 2 * 1024 * 1024;

/// 写入 F02 延迟夹具的全局 LSP 配置（与 `FixtureDirs::write_lsp_settings` 同隔离口径：
/// 必须落在夹具临时 HOME 内；池仍是惰性构造，子进程在用例显式预热时才拉起）。
fn write_delayed_lsp_settings(dirs: &FixtureDirs, server: &str, pid_file: &Path) {
    let settings_path = crate::provider::config_path();
    assert!(
        settings_path.starts_with(&dirs.home),
        "LSP 配置必须落在夹具临时 HOME 内（否则写的是开发者本机配置）: {}",
        settings_path.display()
    );
    std::fs::create_dir_all(settings_path.parent().expect("settings.json 必有父目录"))
        .expect("创建临时 HOME 下的 ~/.peri");
    let mut servers = serde_json::Map::new();
    servers.insert(
        server.to_string(),
        json!({
            "command": "perl",
            "args": ["-e", DELAYED_LSP_SCRIPT],
            "env": { "PERI_LSP_STALL_PID": pid_file.to_string_lossy() },
            "extensionToLanguage": { ".w2stall": "plaintext" },
        }),
    );
    std::fs::write(
        &settings_path,
        json!({ "config": { "lspServers": servers } }).to_string(),
    )
    .expect("写入 LSP settings.json");
}

/// 夹具子进程自证的 PID（脚本启动时写入；读取失败表示子进程没起来）。
fn read_stall_pid(pid_file: &Path) -> u32 {
    std::fs::read_to_string(pid_file)
        .unwrap_or_else(|error| panic!("延迟夹具必须自证 PID（{}）: {error}", pid_file.display()))
        .trim()
        .parse()
        .expect("PID 必须是整数")
}

impl AssembledHostFixture {
    /// F02 收尾：调用**生产** `shutdown_host`（与
    /// `multi_cwd_degradation_and_host_shutdown` 收尾同一函数、同一参数形状）。
    ///
    /// 夹具没有 session，`sessions` 传空表：本用例的断言面是 host pool / MCP task /
    /// host LSP pool 的收敛，与 session 生命周期无关。
    async fn shutdown_host_production(
        &mut self,
    ) -> crate::host::task_scope::HostTerminalShutdownReport {
        let mut task_owner = self.cfg.host_task_owner.take().expect("宿主 task owner");
        let shared: SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let prompt_locks: PromptLocks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let connection = Arc::new(tokio::sync::Mutex::new(ConnectionContext::new(false)));
        let connection_cancellation = tokio_util::sync::CancellationToken::new();
        let mut cont_tx = None;
        let mut closing_sessions = std::collections::BTreeMap::new();
        crate::host::shutdown::shutdown_host(
            &mut task_owner,
            self._owner.as_mut(),
            &self.cfg,
            &shared,
            &prompt_locks,
            &mut cont_tx,
            &connection,
            &connection_cancellation,
            &mut closing_sessions,
        )
        .await
    }
}

/// R29 / F02（§3 第 5 行、§7.3）：`mcp__lsp__LSP` 经桥层外层 120s 上界到期后返回 typed
/// 超时错误，且**到期不等于取消传播**；显式 close 后收敛（子进程回收 + host shutdown Complete）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn lsp_bridge_timeout_cancels_and_converges() {
    const LSP_TOOL: &str = "mcp__lsp__LSP";
    let dirs = FixtureDirs::new();
    let pid_file = dirs.tmp.path().join("delayed-lsp.pid");
    write_delayed_lsp_settings(&dirs, "w2_delayed", &pid_file);
    let stalled_file = dirs.workspace.join("r29-delayed.w2stall");
    std::fs::write(
        &stalled_file,
        "// r29 delayed fixture line\n".repeat(DELAYED_LSP_PAYLOAD / 28),
    )
    .expect("写入延迟夹具正文");
    let stalled_path = stalled_file.to_string_lossy().into_owned();

    let mut fixture = AssembledHostFixture::start_with_settings_written(dirs, false).await;
    fixture.assert_all_ready();
    // host LSP pool（生产装配产出的唯一一份，与 builtin `lsp` 实例同一 `Arc`）。
    let host_pool = Arc::clone(
        &fixture
            .cfg
            .lsp_pool
            .clone()
            .expect("装配必须构造 host LSP pool"),
    )
    .downcast_arc::<LspServerPool>()
    .unwrap_or_else(|_| panic!("host pool 必须是 LspServerPool"));
    // 预热（**真实时钟**）：让 language server 子进程真的起来并完成握手，使被测窗口里
    // 只剩「didOpen 写入挂起」这一件事——否则真实子进程的启动 I/O 会与暂停时钟互相干扰。
    host_pool
        .ensure_initialized()
        .await
        .expect("延迟夹具的 language server 必须能起来（否则断言面空洞）");
    let stall_pid = read_stall_pid(&pid_file);
    assert!(
        process_alive(stall_pid),
        "预热后延迟夹具子进程（pid={stall_pid}）必须存活"
    );

    // 模型面 bridge：生产 public builder 从**本夹具真实 pool** 构造。
    let tools = peri_middlewares::mcp::build_tool_bridges(&fixture.pool);
    let bridge = tools
        .iter()
        .find(|tool| tool.name() == LSP_TOOL)
        .unwrap_or_else(|| {
            panic!(
                "夹具 pool 必须提供 {LSP_TOOL}（否则断言面空洞）；实际 = {:?}",
                tools.iter().map(|tool| tool.name()).collect::<Vec<_>>()
            )
        });

    // ── ① 桥层 120s 上界：虚拟时钟到期，不真等 120 秒 ─────────────────────────
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let outcome = bridge
        .invoke(
            json!({ "operation": "documentSymbol", "file_path": stalled_path }),
            peri_agent::tools::ToolContext::new(&[], &fixture.dirs.workspace_str()),
        )
        .await;
    let elapsed = started.elapsed();
    let error = outcome.expect_err("延迟夹具不返回 ⇒ 桥层到期必须报错，不得静默成功");
    let text = error.to_string();
    let typed = error
        .downcast_ref::<peri_middlewares::mcp::ToolCallError>()
        .unwrap_or_else(|| panic!("必须浮出 typed ToolCallError，实际: {error:?}"));
    assert!(
        matches!(
            typed,
            peri_middlewares::mcp::ToolCallError::Timeout {
                server,
                tool,
                timeout_secs,
            } if server == "lsp" && tool == "LSP" && *timeout_secs == 120
        ),
        "必须是 Timeout{{server: lsp, tool: LSP, timeout_secs: 120}}，实际: {typed:?}"
    );
    assert_eq!(
        text, "MCP 服务器 \"lsp\" 工具 \"LSP\" 调用超时 (120s)",
        "超时文案必须与冻结模板逐字相等"
    );
    assert!(
        !text.contains(&stalled_path) && !text.contains("r29-delayed.w2stall"),
        "超时文案不得回显入参（文件路径）: {text}"
    );
    assert!(
        elapsed >= std::time::Duration::from_secs(120)
            && elapsed < std::time::Duration::from_secs(121),
        "虚拟耗时必须落在桥 deadline 上（{elapsed:?} 应为 120s）"
    );

    // ── ② 到期 ≠ 取消传播：底层调用仍在飞 ────────────────────────────────────
    let states = host_pool.server_info();
    let state = states
        .first()
        .map(|info| info.state.clone())
        .expect("host pool 必须登记延迟夹具 server");
    assert!(
        matches!(state, peri_resources::lsp::client::ServerState::Running),
        "到期时 LSP 连接必须仍在（超时只 drop 客户端等待，不得重置 LSP 层）: {state:?}"
    );
    assert!(
        process_alive(stall_pid),
        "到期时 language server 子进程（pid={stall_pid}）必须仍存活 ⇒ 底层调用在飞"
    );

    // ── ③ 显式 close 收敛：生产 shutdown_host ─────────────────────────────────
    // 时钟**恢复真实**：收敛阶段只做有界真实等待（LSP 优雅关停自身的 5s/1s 期限），
    // 虚拟时钟下的 auto-advance 会把「真实 I/O 轮询」也算成虚拟时间，不作断言依据。
    tokio::time::resume();
    let close_started = std::time::Instant::now();
    let report = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        fixture.shutdown_host_production(),
    )
    .await
    .expect("host shutdown 必须在有界真实等待内返回，不得挂起");
    let close_elapsed = close_started.elapsed();
    assert!(
        matches!(
            report,
            crate::host::task_scope::HostTerminalShutdownReport::Complete { .. }
        ),
        "host shutdown 必须收敛（MCP task / host pool / host LSP pool 全部结算）: {report:?}"
    );
    assert!(
        !process_alive(stall_pid),
        "收敛后 language server 子进程（pid={stall_pid}）必须被回收（无孤儿进程）"
    );
    let closed_state = host_pool.server_info();
    assert!(
        closed_state.iter().all(|info| !matches!(
            info.state,
            peri_resources::lsp::client::ServerState::Starting
                | peri_resources::lsp::client::ServerState::Running
        )),
        "收敛后 host LSP pool 必须处于终态（不得残留 Starting/Running）: {closed_state:?}"
    );
    println!(
        "[W2 timeout] elapsed={elapsed:?} timeout=true timeout_secs=120 text={text:?} client_await_dropped=true server_side_cancelled=false in_flight_at_deadline=true(pid={stall_pid} alive + lsp_state={state:?}) close_elapsed={close_elapsed:?} host_shutdown={report:?} child_reaped=true converged=true"
    );
}
