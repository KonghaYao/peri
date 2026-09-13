//! 端到端集成的共享支撑：**真实 `local-mcp-server` 进程** + **真实本机执行**。
//!
//! 与传输层测试（`transport_stdio.rs` / `transport_http.rs`，用脚本执行器做夹具）的区别：
//! 本模块拉起的是 `src/main.rs` 编出的**真实二进制**，走完整装配
//! （配置校验 → 工作区根 → 进程内执行内核 → 传输），因此七工具的语义、任务生命周期与
//! 关闭回收都是产品真实行为，而不是夹具自证。
//!
//! | 测试 | 执行者 | 证明的是 |
//! | --- | --- | --- |
//! | `transport_stdio.rs` / `transport_http.rs` | 脚本执行器（夹具） | MCP core 到 executor seam 的 wire 保真 |
//! | `e2e_stdio.rs` / `e2e_http.rs`（本模块支撑） | 真实 bin → 进程内执行内核 | 七工具在本机进程内的真实语义与完整装配 |
//!
//! 三条硬约束：
//!
//! 1. **不使用 rmcp 客户端**：stdio 用逐行手写 JSON-RPC 文本，HTTP 用 `http_support`
//!    的阻塞式原始 HTTP/1.1 客户端。因此断言的是真 wire，不是 SDK 自证。
//! 2. **不跳过**：任何前置失败（进程起不来、端口不可达、超时）都显式 fail，不静默通过；
//!    唯一的 `#[ignore]` 是 `transport_stdio.rs` 里的子进程入口开关，不是被掩盖的断言。
//! 3. **执行形态是单进程（D-003）**：本模块**不再有任何 Docker/镜像/容器概念**；
//!    取而代之的是可以真实验证的本机事实——进程树、当前 uid、工作区根 cwd、PATH 上的 docker。
//!
//! 子进程 stderr 由独立线程排空（既避免管道写满阻塞 server，又保留启动诊断用于断言）。

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// 单次等待响应的默认上限。
pub const RECV_TIMEOUT: Duration = Duration::from_secs(120);

/// server 二进制路径（由 cargo 注入，就是 `src/main.rs` 编出来的那个）。
pub fn broker_bin() -> &'static str {
    env!("CARGO_BIN_EXE_local-mcp-server")
}

/// 当前进程的有效 uid（用于断言 Bash 以**同一用户**执行，而不是套了另一层身份）。
pub fn current_uid() -> u32 {
    // SAFETY: `getuid` 无参数、无副作用，只返回本进程的真实 uid。
    unsafe { libc::getuid() }
}

/// PATH 的一个变体：**把含 `docker` 可执行文件的目录剔除**（若本来就没有 docker，则原样返回）。
///
/// 用途：证明产品在本机执行路径上不依赖 Docker —— 与 `A-009` 的
/// “`docker` 不存在时功能不受影响”一致，且不靠 mock（真的换了 PATH，真的跑七工具）。
pub fn path_without_docker() -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    let kept: Vec<String> = std::env::split_paths(&current)
        .filter(|dir| !dir.join("docker").exists())
        .map(|dir| dir.to_string_lossy().to_string())
        .collect();
    kept.join(":")
}

/// PATH 上是否还有 docker（用于自检 [`path_without_docker`] 确实起作用）。
pub fn path_has_docker(path: &str) -> bool {
    std::env::split_paths(path).any(|dir| dir.join("docker").exists())
}

/// 一条进程表记录（`ps -eo pid=,ppid=,pgid=,command=`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    pub command: String,
}

/// 当前整机进程表（只读；用于真实的进程树断言，不做任何猜测）。
pub fn process_table() -> Vec<ProcessEntry> {
    let output = Command::new("ps")
        .args(["-eo", "pid=,ppid=,pgid=,command="])
        .output()
        .expect("ps 必须可用（进程树断言依赖它）");
    assert!(
        output.status.success(),
        "ps 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.trim().splitn(4, char::is_whitespace);
            let pid = parts.next()?.parse::<i32>().ok()?;
            let ppid = parts.next()?.parse::<i32>().ok()?;
            let pgid = parts.next()?.parse::<i32>().ok()?;
            let command = parts.next().unwrap_or_default().to_string();
            Some(ProcessEntry {
                pid,
                ppid,
                pgid,
                command,
            })
        })
        .collect()
}

/// `kill(2)` based liveness check for a process identity captured before exit.
/// EPERM means the process exists; only ESRCH proves that it is gone.
#[cfg(unix)]
pub fn process_exists(pid: i32) -> bool {
    // SAFETY: signal zero only probes the supplied process identity.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// `kill(2)` based liveness check for a process group identity.
#[cfg(unix)]
pub fn process_group_exists(pgid: i32) -> bool {
    if pgid <= 1 {
        return false;
    }
    // SAFETY: signal zero only probes the supplied process-group identity.
    let result = unsafe { libc::kill(-pgid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn current_process_group() -> i32 {
    // SAFETY: getpgid(0) queries this process only.
    unsafe { libc::getpgid(0) }
}

/// Assert, with a bounded wait, that both captured identities disappeared.
#[cfg(unix)]
pub fn wait_until_process_and_group_gone(pid: i32, pgid: i32, timeout: Duration) {
    assert_ne!(
        pgid,
        current_process_group(),
        "测试不得检查或杀死自身进程组"
    );
    assert!(pgid > 1, "后台任务必须提供有效 pgid: {pgid}");
    let deadline = Instant::now() + timeout;
    while process_exists(pid) || process_group_exists(pgid) {
        assert!(
            Instant::now() < deadline,
            "任务身份仍存活 pid={pid}, pgid={pgid}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `pid` 的**直接**子进程（真实 `ps` 观察，不是记账推断）。
pub fn children_of(pid: i32) -> Vec<ProcessEntry> {
    process_table()
        .into_iter()
        .filter(|entry| entry.ppid == pid)
        .collect()
}

/// `pid` 的全部后代进程（逐层展开）。
pub fn descendants_of(pid: i32) -> Vec<ProcessEntry> {
    let table = process_table();
    let mut collected = Vec::new();
    let mut frontier = vec![pid];
    while let Some(current) = frontier.pop() {
        for entry in table.iter().filter(|entry| entry.ppid == current) {
            collected.push(entry.clone());
            frontier.push(entry.pid);
        }
    }
    collected
}

/// 等 `pid` 不再有子进程（返回仍存在的子进程，空 = 已无残留）。
pub fn wait_until_no_children(pid: i32, timeout: Duration) -> Vec<ProcessEntry> {
    let deadline = Instant::now() + timeout;
    loop {
        let children = children_of(pid);
        if children.is_empty() || Instant::now() >= deadline {
            return children;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 等 `pid` 的进程表里出现满足条件的后代（例如后台任务真的 fork 出了进程）。
pub fn wait_for_descendant<F>(pid: i32, timeout: Duration, matches: F) -> Option<ProcessEntry>
where
    F: Fn(&ProcessEntry) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(entry) = descendants_of(pid).into_iter().find(|entry| matches(entry)) {
            return Some(entry);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 测试工作区：宿主临时目录，也是 server 的**唯一**能力边界（`--workspace`）。
pub struct Sandbox {
    temp: tempfile::TempDir,
}

impl Sandbox {
    /// 建一个空工作区。
    pub fn new(prefix: &str) -> Self {
        let temp = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("tempdir");
        Self { temp }
    }

    /// 工作区根（= 启动参数 `--workspace`，= Bash 的 cwd）。
    pub fn root(&self) -> &Path {
        self.temp.path()
    }

    /// 根的规范化路径（产品启动时也做同一变换，断言因此可比）。
    pub fn canonical_root(&self) -> PathBuf {
        std::fs::canonicalize(self.root()).expect("canonicalize workspace root")
    }

    /// 绝对路径。
    pub fn host_path(&self, relative: &str) -> PathBuf {
        self.root().join(relative)
    }

    /// 写入一个文件（自动建父目录）。
    pub fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.host_path(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&path, content).expect("write fixture");
        path
    }

    /// 读取一个文件。
    pub fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.host_path(relative))
            .unwrap_or_else(|err| panic!("read {relative}: {err}"))
    }

    /// 是否存在于宿主。
    pub fn exists(&self, relative: &str) -> bool {
        self.host_path(relative).exists()
    }

    /// 工作区根的**父目录**（根外）——用于证明 Bash 不受根边界限制（能力边界≠安全边界）。
    pub fn parent_dir(&self) -> PathBuf {
        self.root()
            .parent()
            .expect("workspace parent")
            .to_path_buf()
    }
}

/// server 子进程退出后的观察结果。
pub struct BrokerExit {
    /// 退出码（被信号杀死时为 `None`）。
    pub code: Option<i32>,
    /// stderr 全量文本（有界保留）。
    pub stderr: String,
}

/// 一个真实运行的 server 子进程（stdio 传输）。
pub struct StdioBroker {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    /// 断言过程中需要保留的非响应消息（通知等）。
    pub notifications: Vec<Value>,
    /// 已收但尚未被 `request` 消费的消息（乱序响应/通知）。
    pub pending: Vec<Value>,
    task_groups: Vec<i32>,
}

impl StdioBroker {
    /// 以 stdio 传输启动 server。
    ///
    /// `extra_args` 追加在固定参数之后（例如 `--task-ttl-secs`）。
    pub fn start(workspace: &Path, extra_args: &[&str]) -> Self {
        Self::start_with_env(workspace, extra_args, &[])
    }

    /// 同 [`StdioBroker::start`]，但额外注入环境变量（例如 PATH 变体）。
    pub fn start_with_env(workspace: &Path, extra_args: &[&str], env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(broker_bin());
        command
            .arg("--transport")
            .arg("stdio")
            .arg("--workspace")
            .arg(workspace)
            .args(extra_args)
            .env("RUST_LOG", "info")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn server");
        let stdin = child.stdin.take().expect("server stdin");
        let stdout = child.stdout.take().expect("server stdout");
        let stderr_pipe = child.stderr.take().expect("server stderr");

        let (sender, lines) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
        });

        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let sink = Arc::clone(&stderr);
        // 只保留有界尾巴：长跑也不会撑爆内存。
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr_pipe);
            for line in reader.lines().map_while(Result::ok) {
                let mut sink = sink.lock().expect("stderr lock");
                if sink.len() == 2000 {
                    sink.pop_front();
                }
                sink.push_back(line);
            }
        });

        Self {
            child,
            stdin: Some(stdin),
            lines,
            stderr,
            notifications: Vec::new(),
            pending: Vec::new(),
            task_groups: Vec::new(),
        }
    }

    /// 子进程 pid（进程树断言的起点）。
    pub fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    /// Register a task process group while it is still discoverable.
    pub fn track_process_group(&mut self, pgid: i32) {
        assert!(pgid > 1 && pgid != current_process_group());
        if !self.task_groups.contains(&pgid) {
            self.task_groups.push(pgid);
        }
    }

    /// 发送一条 JSON-RPC 消息（不等待响应）。
    pub fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().expect("server stdin closed");
        writeln!(stdin, "{message}").expect("write to server stdin");
        stdin.flush().expect("flush server stdin");
    }

    /// 发送原始文本行（用于畸形帧断言）。
    pub fn send_raw(&mut self, text: &str) {
        let stdin = self.stdin.as_mut().expect("server stdin closed");
        writeln!(stdin, "{text}").expect("write raw to server stdin");
        stdin.flush().expect("flush server stdin");
    }

    /// 读取一条 JSON 行（超时或管道关闭时返回 `None`）。
    fn recv_line(&mut self, timeout: Duration) -> Option<String> {
        match self.lines.recv_timeout(timeout) {
            Ok(line) => Some(line),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    /// 目前 stdout 上已到达的全部行（用于断言"stdout 只承载协议"）。
    pub fn drain_stdout_lines(&mut self) -> Vec<String> {
        let mut collected = Vec::new();
        while let Ok(line) = self.lines.try_recv() {
            collected.push(line);
        }
        collected
    }

    /// 读取下一条 JSON-RPC 消息。
    pub fn recv(&mut self, timeout: Duration) -> Value {
        let line = self
            .recv_line(timeout)
            .unwrap_or_else(|| panic!("等待 server 响应超时（{timeout:?}）"));
        serde_json::from_str(&line).unwrap_or_else(|err| {
            panic!("server stdout 不是 JSON（协议通道被污染）：{err}\n行内容：{line}")
        })
    }

    /// 发送请求并等待**同一 id** 的响应；途中收到的通知/其它消息先入 pending。
    pub fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.await_response(id)
    }

    /// 等待指定 id 的响应（`pending` 里已有的消息优先匹配）。
    pub fn await_response(&mut self, id: u64) -> Value {
        let wanted = json!(id);
        if let Some(position) = self
            .pending
            .iter()
            .position(|value| value.get("id") == Some(&wanted) && value.get("method").is_none())
        {
            return self.pending.remove(position);
        }
        loop {
            let message = self.recv(RECV_TIMEOUT);
            let is_response = message.get("method").is_none() && message.get("id").is_some();
            if is_response && message.get("id") == Some(&wanted) {
                return message;
            }
            if is_response {
                self.pending.push(message);
            } else {
                self.notifications.push(message);
            }
        }
    }

    /// legacy（`2025-11-25`）握手：`initialize` + `notifications/initialized`。
    pub fn legacy_handshake(&mut self) -> Value {
        let response = self.request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "e2e-local", "version": "1.0.0"},
            }),
        );
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        response
    }

    /// `tools/call`（legacy 风格请求体；modern 服务端同样接受无 `_meta` 的 legacy 请求）。
    pub fn call_tool(&mut self, id: u64, tool: &str, arguments: Value) -> Value {
        self.request(
            id,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        )
    }

    /// `tools/call` 的结果（JSON-RPC error 时 panic）。
    pub fn tool_result(&mut self, id: u64, tool: &str, arguments: Value) -> Value {
        let message = self.call_tool(id, tool, arguments);
        assert!(
            message.get("error").is_none(),
            "工具 {tool} 不应是 JSON-RPC 错误：{message}"
        );
        message.get("result").cloned().unwrap_or(Value::Null)
    }

    /// `resources/read` 的结果载荷文本。
    pub fn read_resource(&mut self, id: u64, uri: &str) -> Value {
        self.request(id, "resources/read", json!({"uri": uri}))
    }

    /// stderr 文本快照。
    pub fn stderr_text(&self) -> String {
        self.stderr
            .lock()
            .expect("stderr lock")
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 等待 stderr 出现某个子串（用于启动就绪判定），返回是否出现。
    pub fn wait_for_stderr(&self, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.stderr_text().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// 关闭 stdin（EOF）并等待进程结束；超时则 kill。
    pub fn shutdown(mut self) -> BrokerExit {
        drop(self.stdin.take());
        let stderr_sink = Arc::clone(&self.stderr);
        let code = wait_with_timeout(&mut self.child, Duration::from_secs(60));
        let stderr = stderr_sink
            .lock()
            .expect("stderr lock")
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        BrokerExit { code, stderr }
    }

    /// Send a Unix termination signal and wait for the server's graceful exit.
    #[cfg(unix)]
    pub fn shutdown_with_signal(mut self, signal: libc::c_int) -> BrokerExit {
        unsafe {
            libc::kill(self.child.id() as i32, signal);
        }
        let stderr_sink = Arc::clone(&self.stderr);
        let code = wait_with_timeout(&mut self.child, Duration::from_secs(60));
        let stderr = stderr_sink
            .lock()
            .expect("stderr lock")
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        BrokerExit { code, stderr }
    }
}

impl Drop for StdioBroker {
    fn drop(&mut self) {
        #[cfg(unix)]
        for pgid in self.task_groups.drain(..) {
            if pgid != current_process_group() && process_group_exists(pgid) {
                // SAFETY: only explicitly registered task groups are targeted.
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            }
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// 等子进程结束（带超时），返回退出码。
pub fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<i32> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }
}

/// 取一个当前空闲的回环端口。
///
/// 存在极小的"释放后又被别人占用"窗口；HTTP 用例在连接失败时会重试启动，
/// 因此不把它当作正确性问题。
pub fn free_loopback_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

/// HTTP server 子进程（真实 socket）。
pub struct HttpBroker {
    child: Child,
    stderr: Arc<Mutex<VecDeque<String>>>,
    addr: SocketAddr,
    lines: Receiver<String>,
    task_groups: Vec<i32>,
}

impl HttpBroker {
    /// 以 Streamable HTTP 启动 server 并等待监听就绪。
    pub fn start(workspace: &Path, extra_args: &[&str]) -> Self {
        Self::start_with_env(workspace, extra_args, &[])
    }

    /// 同 [`HttpBroker::start`]，但额外注入环境变量（例如 bearer token 的值来源）。
    ///
    /// token 只经进程环境传递：不进命令行（进程列表）、不落盘、不进 fixture。
    pub fn start_with_env(workspace: &Path, extra_args: &[&str], env: &[(&str, &str)]) -> Self {
        let port = free_loopback_port();
        let mut command = Command::new(broker_bin());
        command
            .arg("--transport")
            .arg("http")
            .arg("--bind")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--workspace")
            .arg(workspace)
            .args(extra_args)
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn server");
        let stdout = child.stdout.take().expect("stdout");
        let stderr_pipe = child.stderr.take().expect("stderr");

        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let sink = Arc::clone(&stderr);
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr_pipe);
            for line in reader.lines().map_while(Result::ok) {
                let mut sink = sink.lock().expect("stderr lock");
                if sink.len() == 2000 {
                    sink.pop_front();
                }
                sink.push_back(line);
            }
        });
        // stdout 在 HTTP 模式下不是协议通道；仍排空以免管道写满。
        let (sender, lines) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    return;
                }
            }
        });

        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let broker = Self {
            child,
            stderr,
            addr,
            lines,
            task_groups: Vec::new(),
        };
        broker.wait_ready(Duration::from_secs(60));
        broker
    }

    /// 监听地址。
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 子进程 pid（进程树断言的起点）。
    pub fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    /// Register a task process group while it is still discoverable.
    pub fn track_process_group(&mut self, pgid: i32) {
        assert!(pgid > 1 && pgid != current_process_group());
        if !self.task_groups.contains(&pgid) {
            self.task_groups.push(pgid);
        }
    }

    /// stdout 上意外出现的文本（HTTP 模式应为空）。
    pub fn stdout_lines(&mut self) -> Vec<String> {
        let mut collected = Vec::new();
        while let Ok(line) = self.lines.try_recv() {
            collected.push(line);
        }
        collected
    }

    /// stderr 文本快照。
    pub fn stderr_text(&self) -> String {
        self.stderr
            .lock()
            .expect("stderr lock")
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 等待端口可连接（最多 `timeout`）。
    fn wait_ready(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if TcpStream::connect_timeout(&self.addr, Duration::from_millis(250)).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "HTTP server 在 {timeout:?} 内未就绪（stderr：\n{}）",
            self.stderr_text()
        );
    }

    /// 关闭进程（SIGINT → 优雅关闭），返回退出码与 stderr。
    pub fn shutdown(mut self) -> BrokerExit {
        // SAFETY: 只对刚 spawn 的子进程发送 SIGINT。
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGINT);
        }
        let stderr_sink = Arc::clone(&self.stderr);
        let code = wait_with_timeout(&mut self.child, Duration::from_secs(60));
        let stderr = stderr_sink
            .lock()
            .expect("stderr lock")
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        BrokerExit { code, stderr }
    }

    /// Send a Unix termination signal and wait for the server's graceful exit.
    #[cfg(unix)]
    pub fn shutdown_with_signal(mut self, signal: libc::c_int) -> BrokerExit {
        unsafe {
            libc::kill(self.child.id() as i32, signal);
        }
        let stderr_sink = Arc::clone(&self.stderr);
        let code = wait_with_timeout(&mut self.child, Duration::from_secs(60));
        let stderr = stderr_sink
            .lock()
            .expect("stderr lock")
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        BrokerExit { code, stderr }
    }
}

impl Drop for HttpBroker {
    fn drop(&mut self) {
        #[cfg(unix)]
        for pgid in self.task_groups.drain(..) {
            if pgid != current_process_group() && process_group_exists(pgid) {
                // SAFETY: only explicitly registered task groups are targeted.
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            }
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// 证据导出钩子：仅当 `LOCAL_MCP_EVIDENCE_DIR` 被显式设置时落盘。
pub fn dump_evidence(file_name: &str, content: &str) {
    let Ok(dir) = std::env::var("LOCAL_MCP_EVIDENCE_DIR") else {
        return;
    };
    let path = Path::new(&dir).join(file_name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, content).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
}
