//! WP-005：stdio **真实子进程** + 原始 JSON-RPC 的 wire 集成测试。
//!
//! ## 为什么是这个形状
//!
//! - 产品 `main.rs` 的传输装配属于 WP-007，本包不得修改其他 owner 的文件，因此这里用
//!   `current_exe()` 重新执行**本测试二进制**作为子进程入口（见
//!   [`test_stdio_server_entry_shim`]），在子进程里运行真实的
//!   [`local_mcp_server::transport::stdio::serve_stdio`]。
//! - 请求与断言用**手写 JSON 文本**（不是 rmcp client 生成的），因此验证的是真 wire，
//!   而不是 SDK 自证。
//! - 夹具 `tests/fixtures/protocol/**` 是这些用例的数据源：断言内容与夹具同源，
//!   不是散落在代码里的魔数。
//!
//! ## 子进程 stdout 的纯净性
//!
//! 父进程把协议管道作为 fd 3 交给子进程，子进程入口把它 `dup2` 成 fd 1；libtest 自身
//! 的进度输出留在原来的 fd 1（父进程置为 null）。这样"stdout 每一行都是 JSON-RPC"
//! 才是可断言的事实，而不是被测试框架噪声稀释的近似。
//!
//! 工具执行由**脚本执行器**完成：它只回显 MCP core 传下来的工具名与参数。因此本文件
//! 证明的是"MCP core 到 executor seam 的 wire 保真"（路由、别名、参数透传、结果/错误
//! 映射、通知、关闭语义）；七工具的真实语义与单进程本机执行由 `e2e_stdio.rs` /
//! `e2e_http.rs` 负责。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use local_mcp_server::error::ToolError;
use local_mcp_server::mcp::resources::{task_uri, TaskStatusSource, TASKS_URI};
use local_mcp_server::mcp::SandboxServer;
use local_mcp_server::wire::{
    BoxFuture, StructuredOutput, TaskSnapshot, TaskStatus, ToolExecutor, ToolRequest, ToolResponse,
    TOOL_ALIASES, TOOL_NAMES,
};

/// 子进程入口开关。
const SERVER_ENV: &str = "LOCAL_MCP_TEST_STDIO_SERVER";
/// 父进程交给子进程的协议管道 fd（子进程入口把它换成 stdout）。
const PROTOCOL_FD: i32 = 3;
/// 单次等待响应的默认上限。
const RECV_TIMEOUT: Duration = Duration::from_secs(10);

const LIFECYCLE_RAW: &str = include_str!("fixtures/protocol/lifecycle.json");
const TOOL_CALLS_RAW: &str = include_str!("fixtures/protocol/tool_calls.json");
const ERROR_CASES_RAW: &str = include_str!("fixtures/protocol/error_cases.json");
const RESOURCES_RAW: &str = include_str!("fixtures/protocol/resources.json");
const PAGINATION_RAW: &str = include_str!("fixtures/protocol/pagination.json");

// ─────────────────────────────── 子进程入口 ───────────────────────────────

/// 子进程入口：只有设置了 [`SERVER_ENV`] 时才有行为。
///
/// 该用例被标记为 `#[ignore]`，因为它本身不是断言，而是"测试二进制可以充当真实
/// stdio server"的开关；`cargo test` 不会执行它，父进程用
/// `--exact test_stdio_server_entry_shim --ignored` 单独拉起它。
#[test]
#[ignore = "子进程入口：由本文件其它用例以 LOCAL_MCP_TEST_STDIO_SERVER=1 启动"]
fn test_stdio_server_entry_shim() {
    if std::env::var(SERVER_ENV).is_err() {
        return;
    }
    run_stdio_server_child();
}

/// 在子进程里运行真实 stdio server，结束后直接退出进程（不回到 libtest）。
///
/// 退出码**对齐产品退出码表**（`local_mcp_server::error::exit_code`）：
/// `serve_stdio` 出错 = `TRANSPORT_FAILED(4)`，子进程自身的装配失败 = `INTERNAL(70)`。
/// 这个 shim 代替 `src/main.rs` 承载传输层，因此不能用另一套私有码，
/// 否则父进程的退出码断言证明的不是产品语义。
fn run_stdio_server_child() -> ! {
    // 父进程用 fd 3 传入协议管道：换成 stdout 后，SDK 写出的每一行都进协议管道，
    // 而 libtest 自身的进度输出留在原来的 fd 1（父进程置为 null）。
    if unsafe { libc::dup2(PROTOCOL_FD, 1) } != 1 {
        eprintln!("dup2({PROTOCOL_FD}, 1) 失败");
        std::process::exit(local_mcp_server::error::exit_code::INTERNAL);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("构建子进程 tokio runtime");
    let outcome = runtime.block_on(async {
        let server = SandboxServer::for_stdio(
            Arc::new(ScriptedExecutor),
            Some(Arc::new(ScriptedTasks::default()) as Arc<dyn TaskStatusSource>),
        );
        local_mcp_server::transport::stdio::serve_stdio(server).await
    });

    match outcome {
        Ok(_) => std::process::exit(local_mcp_server::error::exit_code::OK),
        Err(error) => {
            eprintln!("stdio server 结束于错误：{error}");
            std::process::exit(local_mcp_server::error::exit_code::TRANSPORT_FAILED);
        }
    }
}

// ─────────────────────────────── 脚本执行器 ───────────────────────────────

/// 只回显输入的执行器：证明 MCP core 到 executor seam 的传参保真。
struct ScriptedExecutor;

fn ok_response(request: &ToolRequest) -> ToolResponse {
    let structured = StructuredOutput::ok(request.name)
        .with_extra("echo_arguments", request.arguments.clone())
        .with_extra("principal", json!(request.context.principal))
        .with_extra("client_instance", json!(request.context.client_instance));
    ToolResponse::ok(format!("{} ok", request.name), structured)
}

impl ToolExecutor for ScriptedExecutor {
    fn execute<'a>(
        &'a self,
        request: ToolRequest,
    ) -> BoxFuture<'a, Result<ToolResponse, ToolError>> {
        Box::pin(async move {
            let mode = request
                .arguments
                .get("__test_mode")
                .and_then(Value::as_str)
                .unwrap_or("ok");
            match mode {
                // 业务错误：工具跑过了，调用方应看到文本（isError=true）。
                "business_error" => Ok(ToolResponse::tool_error(
                    "Error: scripted business failure",
                    StructuredOutput::error(request.name)
                        .with_extra("echo_arguments", request.arguments.clone()),
                )),
                // 协议错误：服务器内部失败。
                "internal_error" => Err(ToolError::Internal {
                    message: "scripted internal failure".to_string(),
                }),
                // 协议错误：后端不可用（fail closed，不得回退宿主执行）。
                "backend_down" => Err(ToolError::BackendUnavailable {
                    message: "scripted docker daemon unavailable".to_string(),
                }),
                // 长任务：用于验证 notifications/cancelled 能停止在途请求。
                "slow" => {
                    let sleep_ms = request
                        .arguments
                        .get("sleep_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(5_000);
                    tokio::select! {
                        _ = request.context.cancellation.cancelled() => Err(ToolError::Internal {
                            message: "scripted request cancelled".to_string(),
                        }),
                        _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => Ok(ok_response(&request)),
                    }
                }
                _ => Ok(ok_response(&request)),
            }
        })
    }
}

// ─────────────────────────────── 脚本任务源 ───────────────────────────────

/// 脚本任务源：
/// - 自己的任务：owner/instance 用调用方传入的可信身份回填（因此必然属于该连接），
///   第一次查询返回 running，之后返回 completed（用于验证状态变化通知）；
/// - 他人的任务：owner/instance 固定为别的值，协议层必须挡住。
#[derive(Default)]
struct ScriptedTasks {
    polls: Mutex<HashMap<String, usize>>,
}

impl ScriptedTasks {
    fn snapshot_for(&self, principal: &str, instance: &str, task_id: &str) -> Option<TaskSnapshot> {
        if task_id == panic_task_id() {
            // 故障注入：验证订阅轮询任务消失时，服务端必须主动通知订阅流已下线。
            panic!("脚本任务源按夹具要求故障：{task_id}");
        }
        if task_id == owned_task_id() {
            let mut polls = self.polls.lock().expect("polls 锁");
            let poll = polls.entry(task_id.to_string()).or_insert(0);
            let status = if *poll < flip_after_polls() {
                TaskStatus::Running
            } else {
                TaskStatus::Completed
            };
            *poll += 1;
            let mut snapshot = base_snapshot(task_id, principal, instance, status);
            if status == TaskStatus::Completed {
                snapshot.exit_code = Some(0);
                snapshot.ended_at = Some("2026-09-11T12:00:05Z".to_string());
            }
            return Some(snapshot);
        }
        if task_id == foreign_task_id() {
            return Some(base_snapshot(
                task_id,
                "principal-someone-else",
                "conn-someone-else",
                TaskStatus::Running,
            ));
        }
        None
    }
}

impl TaskStatusSource for ScriptedTasks {
    fn snapshots<'a>(
        &'a self,
        principal: &'a str,
        client_instance: &'a str,
    ) -> BoxFuture<'a, Vec<TaskSnapshot>> {
        let owned = self.snapshot_for(principal, client_instance, &owned_task_id());
        let foreign = base_snapshot(
            &foreign_task_id(),
            "principal-someone-else",
            "conn-someone-else",
            TaskStatus::Running,
        );
        Box::pin(async move {
            let mut snapshots = Vec::new();
            snapshots.extend(owned);
            snapshots.push(foreign);
            snapshots
        })
    }

    fn snapshot<'a>(
        &'a self,
        principal: &'a str,
        client_instance: &'a str,
        task_id: &'a str,
    ) -> BoxFuture<'a, Option<TaskSnapshot>> {
        let snapshot = self.snapshot_for(principal, client_instance, task_id);
        Box::pin(async move { snapshot })
    }
}

fn base_snapshot(task_id: &str, owner: &str, instance: &str, status: TaskStatus) -> TaskSnapshot {
    TaskSnapshot {
        task_id: task_id.to_string(),
        owner: owner.to_string(),
        client_instance: instance.to_string(),
        status,
        pid: Some(4242),
        pgid: Some(4242),
        stdout_log: Some("/workspace/.sandbox/tasks/output.log".to_string()),
        stderr_log: None,
        exit_code: None,
        started_at: "2026-09-11T12:00:00Z".to_string(),
        ended_at: None,
    }
}

fn owned_task_id() -> String {
    resources_fixture()["owned_task_id"]
        .as_str()
        .expect("夹具必须给出 owned_task_id")
        .to_string()
}

fn foreign_task_id() -> String {
    resources_fixture()["foreign_task_id"]
        .as_str()
        .expect("夹具必须给出 foreign_task_id")
        .to_string()
}

/// 自己的任务在第几次查询后从 running 翻成 completed（夹具驱动，避免魔数）。
fn flip_after_polls() -> usize {
    resources_fixture()["flip_after_polls"]
        .as_u64()
        .expect("夹具必须给出 flip_after_polls") as usize
}

/// 故障注入用的任务 id（查询即 panic）。
fn panic_task_id() -> String {
    resources_fixture()["panic_task_id"]
        .as_str()
        .expect("夹具必须给出 panic_task_id")
        .to_string()
}

fn resources_fixture() -> Value {
    serde_json::from_str(RESOURCES_RAW).expect("resources 夹具必须是合法 JSON")
}

// ─────────────────────────────── 子进程管理 ───────────────────────────────

/// 一个真实的 stdio server 子进程：请求走 stdin，协议行走独立管道，诊断走 stderr。
struct StdioServer {
    child: Child,
    stdin: Option<ChildStdin>,
    protocol: Receiver<String>,
    diagnostics: Receiver<String>,
}

impl StdioServer {
    fn spawn() -> Self {
        let exe = std::env::current_exe().expect("取得当前测试二进制路径");
        let (protocol_read, protocol_write) = std::io::pipe().expect("创建协议管道");
        let write_fd = protocol_write.as_raw_fd();

        let mut command = Command::new(exe);
        command
            .args(["--exact", "test_stdio_server_entry_shim", "--ignored"])
            .env(SERVER_ENV, "1")
            .stdin(Stdio::piped())
            // libtest 自身的输出留在 null，协议管道只承载 server 写出的字节。
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(write_fd, PROTOCOL_FD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // dup2 在 fd 别名情况下不会清 CLOEXEC，这里显式保证 fd 3 跨 exec 存活。
                let flags = libc::fcntl(PROTOCOL_FD, libc::F_GETFD);
                if flags < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(PROTOCOL_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = command.spawn().expect("启动 stdio 子进程");
        // 父进程必须释放写端，否则子进程退出后父进程读不到 EOF。
        drop(protocol_write);

        let stdin = child.stdin.take();
        let stderr = child.stderr.take().expect("stderr 管道");
        let protocol = spawn_line_reader(protocol_read);
        let diagnostics = spawn_line_reader(stderr);
        Self {
            child,
            stdin,
            protocol,
            diagnostics,
        }
    }

    /// 写一行原始 JSON（自动加换行，即 stdio 的 NDJSON 帧）。
    fn send(&mut self, message: &Value) {
        self.send_raw(&message.to_string());
    }

    /// 写一行原始文本（用于故意构造畸形帧）。
    fn send_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin 仍然打开");
        stdin.write_all(line.as_bytes()).expect("写入请求");
        stdin.write_all(b"\n").expect("写入帧分隔符");
        stdin.flush().expect("刷新请求");
    }

    /// 读一条协议消息；超时或 EOF 直接失败（附带 stderr 便于定位）。
    fn recv(&self, timeout: Duration) -> Value {
        self.recv_optional(timeout).unwrap_or_else(|| {
            panic!(
                "等待协议消息超时（{timeout:?}）\n{}",
                self.diagnostics_dump()
            )
        })
    }

    fn recv_optional(&self, timeout: Duration) -> Option<Value> {
        match self.protocol.recv_timeout(timeout) {
            Ok(line) => Some(parse_protocol_line(&line)),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                panic!("协议管道在收到响应前关闭\n{}", self.diagnostics_dump())
            }
        }
    }

    /// 断言在给定窗口内**没有**针对该 id 的响应（用于取消语义）。
    fn expect_no_response_for(&self, id: &Value, window: Duration) {
        let deadline = Instant::now() + window;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            if let Some(message) = self.recv_optional(remaining.min(Duration::from_millis(100))) {
                assert_ne!(
                    message.get("id"),
                    Some(id),
                    "被取消的请求不得再收到结果：{message}"
                );
            }
        }
    }

    /// 关闭 stdin 触发 EOF（P-10）。
    fn close_stdin(&mut self) {
        self.stdin.take();
    }

    /// 等待子进程退出并返回退出码。
    fn wait_for_exit(&mut self, timeout: Duration) -> i32 {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().expect("等待子进程") {
                return status.code().unwrap_or(-1);
            }
            assert!(Instant::now() < deadline, "子进程在 {timeout:?} 内没有退出");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// 收集当前已产生的 stderr 诊断行。
    fn diagnostics_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        while let Ok(line) = self.diagnostics.recv_timeout(Duration::from_millis(20)) {
            lines.push(line);
        }
        lines
    }

    /// 失败诊断用：把子进程 stderr 拼进断言消息。
    fn diagnostics_dump(&self) -> String {
        let lines = self.diagnostics_lines();
        if lines.is_empty() {
            String::from("（子进程 stderr 为空）")
        } else {
            format!("子进程 stderr:\n{}", lines.join("\n"))
        }
    }

    fn protocol_line_count(&self) -> usize {
        let mut count = 0;
        while self
            .protocol
            .recv_timeout(Duration::from_millis(20))
            .is_ok()
        {
            count += 1;
        }
        count
    }
}

impl Drop for StdioServer {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn spawn_line_reader<R: std::io::Read + Send + 'static>(reader: R) -> Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            match line {
                Ok(line) => {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    receiver
}

/// 协议行必须是合法 JSON 对象：这条断言就是 P-09 的"stdout 只允许 MCP 消息"。
fn parse_protocol_line(line: &str) -> Value {
    let value: Value = serde_json::from_str(line)
        .unwrap_or_else(|error| panic!("stdout 出现非 JSON 行（{error}）：{line}"));
    assert!(
        value.is_object(),
        "stdout 的每一行都必须是 JSON-RPC 对象：{line}"
    );
    value
}

// ─────────────────────────────── 请求构造 ───────────────────────────────

fn lifecycle_fixture() -> Value {
    serde_json::from_str(LIFECYCLE_RAW).expect("lifecycle 夹具必须是合法 JSON")
}

fn modern_meta() -> Value {
    let fixture = lifecycle_fixture();
    json!({
        "io.modelcontextprotocol/protocolVersion": fixture["modern"]["protocol_version"],
        "io.modelcontextprotocol/clientCapabilities": fixture["modern"]["client_capabilities"],
        "io.modelcontextprotocol/clientInfo": fixture["modern"]["client_info"],
    })
}

fn legacy_initialize(id: i64) -> Value {
    let fixture = lifecycle_fixture();
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": fixture["legacy"]["protocol_version"],
            "capabilities": fixture["legacy"]["client_capabilities"],
            "clientInfo": fixture["legacy"]["client_info"],
        }
    })
}

fn initialized_notification() -> Value {
    json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
}

/// 构造 modern 请求（自动带齐必填 `_meta`）。
fn modern_request(id: i64, method: &str, mut params: Value) -> Value {
    let object = params.as_object_mut().expect("params 必须是对象");
    object.insert("_meta".to_string(), modern_meta());
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// 用 legacy 会话完成握手，返回已完成 initialize 的子进程。
fn legacy_session() -> StdioServer {
    let mut server = StdioServer::spawn();
    server.send(&legacy_initialize(1));
    let response = server.recv(RECV_TIMEOUT);
    assert_eq!(response["id"], 1, "initialize 必须回显 id");
    server.send(&initialized_notification());
    server
}

fn tool_names(message: &Value) -> Vec<String> {
    message["result"]["tools"]
        .as_array()
        .expect("tools/list 必须返回数组")
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("工具条目必须有 name")
                .to_string()
        })
        .collect()
}

// ─────────────────────────────── 用例 ───────────────────────────────

/// 冻结夹具（七工具）——tools/list 必须与它们逐字一致。
fn schema_fixtures() -> Vec<Value> {
    [
        include_str!("fixtures/schemas/read.json"),
        include_str!("fixtures/schemas/write.json"),
        include_str!("fixtures/schemas/edit.json"),
        include_str!("fixtures/schemas/glob.json"),
        include_str!("fixtures/schemas/grep.json"),
        include_str!("fixtures/schemas/folder_operations.json"),
        include_str!("fixtures/schemas/bash.json"),
    ]
    .iter()
    .map(|raw| serde_json::from_str(raw).expect("schema 夹具必须是合法 JSON"))
    .collect()
}

fn tool_call_cases() -> Vec<Value> {
    serde_json::from_str::<Value>(TOOL_CALLS_RAW).expect("tool_calls 夹具必须是合法 JSON")["cases"]
        .as_array()
        .expect("tool_calls 夹具必须给出 cases 数组")
        .clone()
}

fn error_cases() -> Vec<Value> {
    serde_json::from_str::<Value>(ERROR_CASES_RAW).expect("error_cases 夹具必须是合法 JSON")
        ["cases"]
        .as_array()
        .expect("error_cases 夹具必须给出 cases 数组")
        .clone()
}

/// A-005 / FC-MCP-03：legacy initialize → initialized → tools/list，恰好七工具且逐字冻结。
#[test]
fn test_stdio_legacy_handshake_lists_exactly_seven_frozen_tools() {
    let mut server = StdioServer::spawn();
    server.send(&legacy_initialize(1));
    let handshake = server.recv(RECV_TIMEOUT);
    assert_eq!(handshake["id"], 1);
    assert_eq!(handshake["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        handshake["result"]["serverInfo"]["name"],
        lifecycle_fixture()["expected"]["server_name"]
    );
    assert!(
        handshake["result"]["capabilities"]["tools"].is_object(),
        "必须声明 tools 能力：{}",
        handshake["result"]["capabilities"]
    );
    assert_eq!(
        handshake["result"]["capabilities"]["resources"]["subscribe"],
        true
    );
    assert!(handshake["result"]["instructions"]
        .as_str()
        .unwrap_or_default()
        .contains("sandbox://tasks"));
    server.send(&initialized_notification());

    server.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}));
    let listed = server.recv(RECV_TIMEOUT);
    assert_eq!(listed["id"], 2);
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 7);
    assert_eq!(tool_names(&listed), TOOL_NAMES.to_vec());
    assert!(
        listed["result"].get("resultType").is_none(),
        "legacy 对端的结果不得带 resultType（SEP-2322 只属于 modern）"
    );
    assert_eq!(
        listed["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        lifecycle_fixture()["expected"]["server_name"]
    );

    for (index, fixture) in schema_fixtures().iter().enumerate() {
        let entry = &listed["result"]["tools"][index];
        assert_eq!(entry["name"], fixture["tool"]);
        assert_eq!(
            entry["description"], fixture["description"],
            "{} 的 description 必须与冻结夹具逐字一致",
            fixture["tool"]
        );
        assert_eq!(
            entry["inputSchema"], fixture["inputSchema"],
            "{} 的 inputSchema 必须与冻结夹具逐字一致",
            fixture["tool"]
        );
    }

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}

/// FC-MCP-03 / P-06 / P-07：modern 直接请求（无 initialize）、required `_meta`、
/// 不支持版本 → `-32022` 且 `data.supported`/`data.requested` 完整。
#[test]
fn test_stdio_modern_lifecycle_direct_requests_and_version_negotiation() {
    let fixture = lifecycle_fixture();
    let mut server = StdioServer::spawn();

    // 不支持版本：首个请求即可判定，连接在错误后仍然可用（inline 生命周期）。
    let mut unsupported = modern_request(1, "tools/list", json!({}));
    unsupported["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] =
        fixture["unsupported_protocol_version"].clone();
    server.send(&unsupported);
    let error = server.recv(RECV_TIMEOUT);
    assert_eq!(error["error"]["code"], -32022);
    let supported: Vec<&str> = error["error"]["data"]["supported"]
        .as_array()
        .expect("-32022 必须带 data.supported")
        .iter()
        .map(|value| value.as_str().unwrap_or_default())
        .collect();
    for expected in fixture["expected"]["supported_versions"]
        .as_array()
        .expect("夹具必须给出支持版本")
    {
        assert!(
            supported.contains(&expected.as_str().unwrap_or_default()),
            "data.supported 必须包含 {expected}"
        );
    }
    assert_eq!(
        error["error"]["data"]["requested"],
        fixture["unsupported_protocol_version"]
    );

    // server/discover：MUST 实现，且不依赖 initialize。
    server.send(&modern_request(2, "server/discover", json!({})));
    let discover = server.recv(RECV_TIMEOUT);
    assert_eq!(discover["result"]["resultType"], "complete");
    let versions: Vec<&str> = discover["result"]["supportedVersions"]
        .as_array()
        .expect("discover 必须返回 supportedVersions")
        .iter()
        .map(|value| value.as_str().unwrap_or_default())
        .collect();
    assert_eq!(versions, vec!["2026-07-28", "2025-11-25"]);
    assert!(discover["result"]["capabilities"]["tools"].is_object());
    assert_eq!(
        discover["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        fixture["expected"]["server_name"]
    );
    assert!(discover["result"]["ttlMs"].as_u64().unwrap_or(0) > 0);
    assert_eq!(discover["result"]["cacheScope"], "public");

    // 直接 tools/list（未经 initialize）：modern 元数据齐全即可服务。
    server.send(&modern_request(3, "tools/list", json!({})));
    let listed = server.recv(RECV_TIMEOUT);
    assert_eq!(listed["result"]["resultType"], "complete");
    assert_eq!(tool_names(&listed).len(), 7);
    assert_eq!(
        listed["result"]["ttlMs"].as_u64(),
        fixture["expected"]["tools_list_ttl_ms"].as_u64()
    );
    assert_eq!(listed["result"]["cacheScope"], "public");

    // 缺必填 _meta：-32602（SDK 的 inline 契约校验），并且该连接不再被视为已建立。
    let mut bare = StdioServer::spawn();
    bare.send(&json!({"jsonrpc": "2.0", "id": 9, "method": "tools/list", "params": {}}));
    let rejected = bare.recv(RECV_TIMEOUT);
    assert_eq!(rejected["error"]["code"], -32602);
    assert!(
        rejected["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("_meta"),
        "错误必须说明缺失的是请求元数据：{rejected}"
    );
    assert_eq!(
        bare.wait_for_exit(RECV_TIMEOUT),
        local_mcp_server::error::exit_code::TRANSPORT_FAILED,
        "未建立生命周期的连接以错误退出（server 初始化失败，不是伪造的成功）"
    );

    server.close_stdin();
    assert_eq!(
        server.wait_for_exit(RECV_TIMEOUT),
        local_mcp_server::error::exit_code::OK
    );
}

/// R-006 的 stdio 半边：七工具全部经真实子进程的 raw wire 调用成功。
#[test]
fn test_stdio_all_seven_tools_succeed_over_raw_wire() {
    let cases = tool_call_cases();
    assert_eq!(cases.len(), 7, "七个规范工具都必须有用例");

    let mut server = StdioServer::spawn();
    server.send(&modern_request(1, "server/discover", json!({})));
    let _ = server.recv(RECV_TIMEOUT);

    let mut id = 100;
    for case in &cases {
        let tool = case["tool"].as_str().expect("用例必须给出 tool");
        let arguments = case["arguments"].clone();
        server.send(&modern_request(
            id,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        ));
        let response = server.recv(RECV_TIMEOUT);

        assert_eq!(response["id"], id, "{tool} 的响应必须配对请求 id");
        assert!(
            response.get("error").is_none(),
            "{tool} 不应产生协议错误：{response}"
        );
        let result = &response["result"];
        assert_eq!(result["resultType"], "complete");
        assert_eq!(
            result["isError"], false,
            "{tool} 成功结果 isError 必须为 false"
        );
        assert_eq!(
            result["content"][0]["text"],
            Value::String(format!("{tool} ok")),
            "{tool} 的文本内容必须原样来自工具结果"
        );
        assert_eq!(result["structuredContent"]["tool"], tool);
        assert_eq!(result["structuredContent"]["ok"], true);
        assert_eq!(
            result["structuredContent"]["echo_arguments"], arguments,
            "{tool} 的参数必须原样透传到执行层（协议层不得增删字段）"
        );
        assert_eq!(
            result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            lifecycle_fixture()["expected"]["server_name"]
        );
        id += 1;
    }

    // 别名不是额外条目：列表仍然恰好七个。
    server.send(&modern_request(id, "tools/list", json!({})));
    let listed = server.recv(RECV_TIMEOUT);
    assert_eq!(tool_names(&listed), TOOL_NAMES.to_vec());

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}

/// 别名（`reading`/`Shell`）只在 `tools/call` 生效，并且不改变工具的输入字段。
#[test]
fn test_stdio_alias_names_route_to_canonical_tools_without_changing_arguments() {
    let mut server = StdioServer::spawn();
    server.send(&modern_request(1, "server/discover", json!({})));
    let _ = server.recv(RECV_TIMEOUT);

    for (index, (alias, canonical)) in TOOL_ALIASES.iter().enumerate() {
        let arguments = if *canonical == "Bash" {
            json!({"command": "true"})
        } else {
            json!({"file_path": "notes.md"})
        };
        server.send(&modern_request(
            20 + index as i64,
            "tools/call",
            json!({"name": alias, "arguments": arguments}),
        ));
        let response = server.recv(RECV_TIMEOUT);
        assert_eq!(
            response["result"]["structuredContent"]["tool"], *canonical,
            "别名 {alias} 必须解析成规范名 {canonical}"
        );
        assert_eq!(
            response["result"]["structuredContent"]["echo_arguments"], arguments,
            "别名路径必须原样透传参数"
        );
        assert_eq!(
            response["result"]["structuredContent"]["echo_arguments"]
                .as_object()
                .map(|fields| fields.len()),
            Some(1),
            "Bash/Read 的输入字段不得被协议层注入其它键"
        );
    }

    server.send(&modern_request(30, "tools/list", json!({})));
    let listed = server.recv(RECV_TIMEOUT);
    let names = tool_names(&listed);
    for (alias, _) in TOOL_ALIASES {
        assert!(!names.contains(&alias.to_string()), "别名不得成为工具条目");
    }

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}

/// FC-MCP-02：业务错误与协议错误的边界，外加"任何情况下不得发出 -32002"。
#[test]
fn test_stdio_error_families_are_distinct_and_never_emit_legacy_codes() {
    let cases = error_cases();
    assert!(cases.len() >= 3, "至少需要三类独立错误用例");

    let allowed_codes = [
        -32700, -32600, -32601, -32602, -32603, -32020, -32021, -32022,
    ];
    let mut server = StdioServer::spawn();
    server.send(&modern_request(1, "server/discover", json!({})));
    let _ = server.recv(RECV_TIMEOUT);

    let mut id = 200;
    for case in &cases {
        let name = case["name"].as_str().expect("用例必须有 name");
        let tool = case["tool"].as_str().expect("用例必须有 tool");
        let arguments = case["arguments"].clone();
        server.send(&modern_request(
            id,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        ));
        let response = server.recv(RECV_TIMEOUT);
        let expectation = &case["expect"];

        if let Some(code) = response["error"]["code"].as_i64() {
            assert_ne!(code, -32002, "{name}：-32002 在本协议版本必须不得发出");
        }

        match expectation["kind"].as_str().unwrap_or_default() {
            "jsonrpc_error" => {
                assert!(
                    response.get("result").is_none(),
                    "{name}：协议错误不得带 result：{response}"
                );
                let code = response["error"]["code"]
                    .as_i64()
                    .unwrap_or_else(|| panic!("{name}：必须带错误码：{response}"));
                assert!(
                    allowed_codes.contains(&code),
                    "{name}：错误码 {code} 不在冻结集合内"
                );
                if let Some(expected) = expectation["code"].as_i64() {
                    assert_eq!(code, expected, "{name}：错误码不符合冻结预期");
                }
                if let Some(prefix) = expectation["message_prefix"].as_str() {
                    let message = response["error"]["message"].as_str().unwrap_or_default();
                    assert!(
                        message.starts_with(prefix),
                        "{name}：错误文本必须以 {prefix:?} 开头，实际 {message:?}"
                    );
                }
            }
            "tool_error" => {
                assert!(
                    response.get("error").is_none(),
                    "{name}：业务错误不得变成协议错误：{response}"
                );
                assert_eq!(
                    response["result"]["isError"], expectation["is_error"],
                    "{name}：isError 必须为真"
                );
                assert_eq!(
                    response["result"]["content"][0]["text"], expectation["text"],
                    "{name}：业务错误文本必须可见"
                );
                assert_eq!(
                    response["result"]["structuredContent"]["ok"], false,
                    "{name}：结构化结果必须如实标记失败"
                );
            }
            other => panic!("{name}：未知期望类型 {other}"),
        }
        id += 1;
    }

    // 错误之后连接必须仍然可用（协议错误不是连接级崩溃）。
    server.send(&modern_request(id, "tools/list", json!({})));
    let listed = server.recv(RECV_TIMEOUT);
    assert_eq!(tool_names(&listed).len(), 7);

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}

/// P-11：`notifications/cancelled` 停止在途请求，且不再发送该请求的结果。
#[test]
fn test_stdio_cancellation_stops_in_flight_tool_call() {
    let mut server = StdioServer::spawn();
    server.send(&modern_request(1, "server/discover", json!({})));
    let _ = server.recv(RECV_TIMEOUT);

    server.send(&modern_request(
        41,
        "tools/call",
        json!({
            "name": "Bash",
            "arguments": {"command": "sleep 30", "__test_mode": "slow", "sleep_ms": 30000}
        }),
    ));
    // 给执行器一点时间进入等待，再取消：验证的是"在途取消"，不是"取消已完成的请求"。
    std::thread::sleep(Duration::from_millis(250));
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 41, "reason": "wp005 取消用例"}
    }));
    server.expect_no_response_for(&json!(41), Duration::from_millis(1500));

    // 取消只影响该请求：连接与后续请求照常服务。
    server.send(&modern_request(42, "tools/list", json!({})));
    let listed = server.recv(RECV_TIMEOUT);
    assert_eq!(listed["id"], 42);
    assert_eq!(tool_names(&listed).len(), 7);

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}

/// P-10：stdin EOF 即优雅退出；未知通知被静默忽略且不破坏连接。
#[test]
fn test_stdio_eof_terminates_process_cleanly() {
    let mut server = legacy_session();

    // 未知/非标准通知必须被忽略（兼容策略），而不是报错或关闭连接。
    server.send(&json!({"jsonrpc": "2.0", "method": "notifications/unknown-by-wp005"}));
    server.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}));
    let listed = server.recv(RECV_TIMEOUT);
    assert_eq!(listed["id"], 2, "未知通知之后连接必须仍然可用");
    assert_eq!(tool_names(&listed).len(), 7);

    server.close_stdin();
    assert_eq!(
        server.wait_for_exit(RECV_TIMEOUT),
        0,
        "stdin EOF 是正常关闭，必须以 0 退出"
    );
    assert_eq!(
        server.protocol_line_count(),
        0,
        "关闭后 stdout 不得再出现任何协议字节"
    );
}

/// 任务资源的所有者边界（FC-STATE-01 的 stdio 侧证据）。
#[test]
fn test_stdio_task_resources_are_owner_scoped() {
    let resources = resources_fixture();
    let owned = resources["owned_task_id"].as_str().unwrap().to_string();
    let foreign = resources["foreign_task_id"].as_str().unwrap().to_string();

    let mut server = StdioServer::spawn();
    server.send(&modern_request(1, "server/discover", json!({})));
    let _ = server.recv(RECV_TIMEOUT);

    server.send(&modern_request(2, "resources/list", json!({})));
    let listed = server.recv(RECV_TIMEOUT);
    let uris: Vec<String> = listed["result"]["resources"]
        .as_array()
        .expect("resources/list 必须返回数组")
        .iter()
        .map(|resource| resource["uri"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(uris.contains(&TASKS_URI.to_string()));
    assert!(uris.contains(&task_uri(&owned)));
    assert!(
        !uris.contains(&task_uri(&foreign)),
        "他人的任务资源不得出现在列表中：{uris:?}"
    );
    assert_eq!(listed["result"]["cacheScope"], "private");
    assert_eq!(listed["result"]["ttlMs"], 0);

    server.send(&modern_request(
        3,
        "resources/read",
        json!({"uri": task_uri(&owned)}),
    ));
    let read = server.recv(RECV_TIMEOUT);
    // 规范 server/utilities/caching：`resultType: "complete"` 的结果**MUST**带缓存提示；
    // 任务正文按主体隔离，因此 ttls=0（立即可陈旧）且 cacheScope=private（不得跨授权上下文共享）。
    assert_eq!(
        read["result"]["resultType"], "complete",
        "resources/read 必须返回 complete 结果：{read}"
    );
    assert_eq!(read["result"]["ttlMs"], 0);
    assert_eq!(read["result"]["cacheScope"], "private");
    let contents = &read["result"]["contents"][0];
    assert_eq!(contents["uri"], task_uri(&owned));
    assert_eq!(contents["mimeType"], "application/json");
    let payload: Value =
        serde_json::from_str(contents["text"].as_str().expect("资源正文必须是文本"))
            .expect("资源正文必须是合法 JSON");
    assert_eq!(payload["task_id"], Value::String(owned.clone()));
    assert_eq!(payload["status"], "running");
    assert_eq!(
        payload["stdout_log"], "/workspace/.sandbox/tasks/output.log",
        "日志路径必须如实暴露，供调用方用 Read/Bash 读取"
    );

    // 他人的任务：与"不存在"同形，不泄露存在性。
    server.send(&modern_request(
        4,
        "resources/read",
        json!({"uri": task_uri(&foreign)}),
    ));
    let denied = server.recv(RECV_TIMEOUT);
    assert_eq!(denied["error"]["code"], -32602);
    assert!(denied["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .starts_with("Resource not found: "));
    assert_eq!(
        denied["error"]["data"]["uri"],
        task_uri(&foreign),
        "资源类错误必须带 data.uri（规范 server/resources#error-handling 示例形状）"
    );

    for malformed in resources["malformed_uris"].as_array().unwrap() {
        server.send(&modern_request(
            5,
            "resources/read",
            json!({"uri": malformed}),
        ));
        let invalid = server.recv(RECV_TIMEOUT);
        assert_eq!(
            invalid["error"]["code"], -32602,
            "畸形 URI 必须被拒绝：{malformed}"
        );
    }

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}

/// P-18：modern `subscriptions/listen` 先 ack（带 `subscriptionId`），
/// 状态变化后发 `notifications/resources/updated`，取消即结束订阅。
#[test]
fn test_stdio_modern_subscription_acks_then_notifies_resource_updated() {
    let owned = owned_task_id();
    let uri = task_uri(&owned);

    let mut server = StdioServer::spawn();
    server.send(&modern_request(1, "server/discover", json!({})));
    let _ = server.recv(RECV_TIMEOUT);

    server.send(&modern_request(
        42,
        "subscriptions/listen",
        json!({"notifications": {"resourceSubscriptions": [uri]}}),
    ));
    let ack = server.recv(RECV_TIMEOUT);
    assert_eq!(
        ack["method"], "notifications/subscriptions/acknowledged",
        "第一条必须是 acknowledged"
    );
    assert_eq!(
        ack["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"], 42,
        "ack 必须带订阅请求 id"
    );
    assert_eq!(
        ack["params"]["notifications"]["resourceSubscriptions"][0],
        uri
    );
    assert!(
        ack.get("id").is_none(),
        "通知不得带 id（JSON-RPC 通知形状）"
    );

    let updated = server.recv(RECV_TIMEOUT);
    assert_eq!(updated["method"], "notifications/resources/updated");
    assert_eq!(updated["params"]["uri"], uri);
    assert_eq!(
        updated["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        42
    );

    // 取消 listen 请求：订阅结束，且被取消的请求不会再收到结果。
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 42}
    }));
    server.expect_no_response_for(&json!(42), Duration::from_millis(800));

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}

/// legacy 订阅路径：`resources/subscribe` 做所有者判定，变化发
/// `notifications/resources/updated`，`resources/unsubscribe` 幂等。
#[test]
fn test_stdio_legacy_resource_subscribe_and_unsubscribe() {
    let owned = owned_task_id();
    let foreign = foreign_task_id();
    let owned_uri = task_uri(&owned);

    let mut server = legacy_session();

    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "resources/subscribe",
        "params": {"uri": task_uri(&foreign)}
    }));
    let denied = server.recv(RECV_TIMEOUT);
    assert_eq!(
        denied["error"]["code"], -32602,
        "订阅他人任务必须被拒绝：{denied}"
    );

    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "resources/subscribe",
        "params": {"uri": owned_uri}
    }));
    // 订阅响应与更新通知的顺序不作假设：先到的可能是任意一条。
    let first = server.recv(RECV_TIMEOUT);
    let (subscribe_result, updated) = if first["id"] == 3 {
        (first, server.recv(RECV_TIMEOUT))
    } else {
        (server.recv(RECV_TIMEOUT), first)
    };
    assert_eq!(subscribe_result["id"], 3);
    assert!(
        subscribe_result.get("error").is_none(),
        "订阅自己的任务必须成功：{subscribe_result}"
    );
    assert_eq!(updated["method"], "notifications/resources/updated");
    assert_eq!(updated["params"]["uri"], owned_uri);

    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "resources/unsubscribe",
        "params": {"uri": owned_uri}
    }));
    let unsubscribed = server.recv(RECV_TIMEOUT);
    assert_eq!(unsubscribed["id"], 4);
    assert!(unsubscribed.get("error").is_none());

    // 幂等：再次取消订阅同一 URI 仍然成功。
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "resources/unsubscribe",
        "params": {"uri": owned_uri}
    }));
    let again = server.recv(RECV_TIMEOUT);
    assert_eq!(again["id"], 5);
    assert!(again.get("error").is_none());

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}

/// P-09 的另一半：诊断只走 stderr，且 stderr 不得出现协议消息。
#[test]
fn test_stdio_diagnostics_never_masquerade_as_protocol_messages() {
    let mut server = legacy_session();
    server.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}));
    let _ = server.recv(RECV_TIMEOUT);
    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);

    for line in server.diagnostics_lines() {
        let parsed = serde_json::from_str::<Value>(&line);
        assert!(
            parsed
                .map(|value| value.get("jsonrpc").is_none())
                .unwrap_or(true),
            "stderr 不得承载 JSON-RPC 消息：{line}"
        );
    }
}

/// 规范 `basic/patterns/cancellation`：服务端主动下线订阅流时 **MUST** 发送
/// `notifications/cancelled` 并引用该 `subscriptions/listen` 的请求 id。
///
/// 触发方式：故障注入的任务源在第一次轮询时 panic，轮询任务因此消失——这正是"服务端
/// 不能再提供该订阅流"的情形。若不发送该通知，客户端会永久等待一个已经死掉的订阅。
#[test]
fn test_stdio_subscription_teardown_notifies_cancelled() {
    let uri = task_uri(&panic_task_id());

    let mut server = StdioServer::spawn();
    server.send(&modern_request(1, "server/discover", json!({})));
    let _ = server.recv(RECV_TIMEOUT);

    server.send(&modern_request(
        43,
        "subscriptions/listen",
        json!({"notifications": {"resourceSubscriptions": [uri]}}),
    ));
    let ack = server.recv(RECV_TIMEOUT);
    assert_eq!(ack["method"], "notifications/subscriptions/acknowledged");
    assert_eq!(
        ack["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        43
    );

    let mut cancelled = None;
    for _ in 0..4 {
        let message = server.recv(RECV_TIMEOUT);
        if message["method"] == "notifications/cancelled" {
            cancelled = Some(message);
            break;
        }
        // 订阅流结束后 SDK 可能还发出该请求的最终结果；规范允许客户端忽略它。
        assert_eq!(message["id"], 43, "只允许该订阅请求自身的消息：{message}");
    }
    let cancelled = cancelled.expect("服务端下线订阅流时必须发送 notifications/cancelled");
    assert_eq!(cancelled["params"]["requestId"], 43);
    assert!(cancelled.get("id").is_none(), "通知不得带 id：{cancelled}");

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}

/// 客户端可控字段的安全边界（规范 `basic/patterns/mrtr` §4/§5 与
/// `basic/patterns/progress`）。
///
/// - `requestState` / `inputResponses` 是**攻击者可控**输入：本服务不消费它们，
///   也不把它们透传给执行层，因此它们无法影响授权或业务逻辑；
/// - `progressToken` 出现在 `_meta` 时，服务端 MAY 不发任何进度通知（本服务不发），
///   且不得因此改变请求结果。
#[test]
fn test_stdio_client_state_and_progress_token_never_influence_execution() {
    let mut server = StdioServer::spawn();
    server.send(&modern_request(1, "server/discover", json!({})));
    let _ = server.recv(RECV_TIMEOUT);

    let arguments = json!({"file_path": "notes.md"});
    let mut request = modern_request(
        50,
        "tools/call",
        json!({"name": "Read", "arguments": arguments}),
    );
    request["params"]["inputResponses"] = json!({"forged": {"action": "accept"}});
    request["params"]["requestState"] = json!("forged-state");
    request["params"]["_meta"]["progressToken"] = json!("forged-progress-token");
    server.send(&request);

    let response = server.recv(RECV_TIMEOUT);
    assert!(
        response.get("error").is_none(),
        "客户端元数据不得让合法请求失败：{response}"
    );
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["structuredContent"]["echo_arguments"], arguments,
        "攻击者可控字段不得进入执行层参数"
    );
    // 本服务不发送任何 notifications/progress；若出现即为协议违规。
    let extra = server.recv_optional(Duration::from_millis(400));
    assert!(
        extra.is_none(),
        "本服务不发送进度通知，也不得发送其它多余消息：{extra:?}"
    );

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}

/// FC-MCP-03 / P-07/P-08：modern 客户端**不需要任何握手**。
///
/// `server/discover` 对客户端是可选的（规范只要求服务端 MUST 实现它），因此这里把
/// `tools/call` 作为连接的**第一条**消息：既没有 `initialize`，也没有 `server/discover`。
/// 若实现偷偷依赖握手状态（例如"没 discovery 过就不路由工具"），本用例会失败。
#[test]
fn test_stdio_direct_modern_call_needs_no_handshake() {
    let arguments = json!({"file_path": "notes.md"});
    let mut server = StdioServer::spawn();

    server.send(&modern_request(
        1,
        "tools/call",
        json!({"name": "Read", "arguments": arguments}),
    ));
    let response = server.recv(RECV_TIMEOUT);
    assert_eq!(response["id"], 1);
    assert!(
        response.get("error").is_none(),
        "modern 直接调用不得被握手状态拒绝：{response}"
    );
    assert_eq!(response["result"]["resultType"], "complete");
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["structuredContent"]["echo_arguments"], arguments,
        "首条消息直接调用时参数同样必须原样透传"
    );

    // 握手从未发生：不得有 initialize 结果或初始化通知混进协议通道。
    let extra = server.recv_optional(Duration::from_millis(300));
    assert!(
        extra.is_none(),
        "未握手的连接不得自行产生握手期消息：{extra:?}"
    );

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}

/// FC-MCP-05 / P-11：畸形或未知的取消通知必须被忽略，而不是打断连接。
///
/// 规范 `basic/patterns/cancellation` 的 Error Handling：未知 requestId、已完成请求与
/// **畸形通知**都 SHOULD 被忽略（通知是 fire-and-forget，没有可回复的地方）。因此这些
/// 输入既不得产生错误响应，也不得让连接停止服务。
#[test]
fn test_stdio_malformed_and_unknown_cancellations_are_ignored() {
    let mut server = StdioServer::spawn();
    server.send(&modern_request(1, "server/discover", json!({})));
    let _ = server.recv(RECV_TIMEOUT);

    // 未知 requestId（规范允许忽略：引用未发出的请求）。
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 9999, "reason": "unknown by design"}
    }));
    // 畸形：缺 params。
    server.send(&json!({"jsonrpc": "2.0", "method": "notifications/cancelled"}));
    // 畸形：requestId 不是字符串/整数（规范要求 string|integer）。
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": {"nested": true}}
    }));
    // 未知通知方法。
    server.send(&json!({"jsonrpc": "2.0", "method": "notifications/unknown-by-wp005"}));

    // 连接必须照常服务：通知之后的第一条消息必须是 tools/list 的响应。
    server.send(&modern_request(2, "tools/list", json!({})));
    let listed = server.recv(RECV_TIMEOUT);
    assert_eq!(
        listed["id"], 2,
        "畸形/未知通知不得产生任何响应或打断连接：{listed}"
    );
    assert_eq!(tool_names(&listed).len(), 7);

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}

/// 分页：本服务的列表面是单页，任何调用方游标都是无效游标 → `-32602`
/// （规范 `server/utilities/pagination#error-handling`）。
///
/// 之所以要拒绝而不是忽略：忽略会让调用方以为自己拿到了"下一页"，而实际上收到的
/// 是第一页的重复内容，调用方会永远翻不到头。
#[test]
fn test_stdio_pagination_cursors_are_rejected() {
    let fixture: Value =
        serde_json::from_str(PAGINATION_RAW).expect("pagination 夹具必须是合法 JSON");
    let expected_code = fixture["expected"]["code"]
        .as_i64()
        .expect("夹具必须给出错误码");
    let prefix = fixture["expected"]["message_prefix"]
        .as_str()
        .expect("夹具必须给出错误文本前缀");

    let mut server = StdioServer::spawn();
    server.send(&modern_request(1, "server/discover", json!({})));
    let _ = server.recv(RECV_TIMEOUT);

    for (id, method, cursor) in [
        (2, "tools/list", fixture["invalid_cursor"].clone()),
        // 空字符串同样是"不是本服务发出的游标"：只有服务端自己发出的游标才是合法游标。
        (3, "tools/list", fixture["empty_cursor"].clone()),
        (4, "resources/list", fixture["invalid_cursor"].clone()),
    ] {
        server.send(&modern_request(id, method, json!({ "cursor": cursor })));
        let rejected = server.recv(RECV_TIMEOUT);
        assert_eq!(rejected["id"], id);
        assert!(
            rejected.get("result").is_none(),
            "{method} 的无效游标不得返回结果页：{rejected}"
        );
        assert_eq!(
            rejected["error"]["code"], expected_code,
            "{method} 的无效游标必须是 Invalid params：{rejected}"
        );
        assert!(
            rejected["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .starts_with(prefix),
            "{method} 的错误文本必须以 {prefix:?} 开头：{rejected}"
        );
    }

    // 不提交游标时行为不变：恰好七项，且从不声明 nextCursor（单页事实）。
    server.send(&modern_request(5, "tools/list", json!({})));
    let listed = server.recv(RECV_TIMEOUT);
    assert_eq!(tool_names(&listed), TOOL_NAMES.to_vec());
    assert!(
        listed["result"].get("nextCursor").is_none(),
        "单页列表不得声明 nextCursor：{listed}"
    );

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}

/// `CallToolRequest.arguments` 是可选的：缺席时按**空对象**处理。
///
/// 协议层不得因为"没有 arguments"就报形状错误，也不得替调用方编造默认参数——谁该报缺参
/// 由工具语义决定（WP-002/WP-003），协议层只负责如实转发。
#[test]
fn test_stdio_call_tool_without_arguments_is_an_empty_object() {
    let mut server = StdioServer::spawn();
    server.send(&modern_request(1, "tools/call", json!({"name": "Read"})));
    let response = server.recv(RECV_TIMEOUT);
    assert!(
        response.get("error").is_none(),
        "缺席 arguments 不是协议错误：{response}"
    );
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(
        response["result"]["structuredContent"]["echo_arguments"],
        json!({}),
        "缺席 arguments 必须等价于空对象，不得编造字段"
    );

    server.close_stdin();
    assert_eq!(server.wait_for_exit(RECV_TIMEOUT), 0);
}
///
/// P-19 / FC-MCP-01：工具注册面**不随连接、不随协议时代变化**。
///
/// 规范 `server/tools#capabilities`："列表 MAY 为空但不得随连接变化"，并 SHOULD 保持确定性
/// 顺序。这里用两个独立进程（一个 legacy 会话、一个 modern 无握手）取得同一份 `tools/list`，
/// 要求条目**逐字相同**：只有结果外壳（`resultType`/`ttlMs`/`cacheScope`）允许因时代而不同。
#[test]
fn test_stdio_tool_surface_is_identical_across_eras_and_connections() {
    let mut legacy = legacy_session();
    legacy.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}));
    let legacy_listed = legacy.recv(RECV_TIMEOUT);
    legacy.close_stdin();
    assert_eq!(legacy.wait_for_exit(RECV_TIMEOUT), 0);

    let mut modern = StdioServer::spawn();
    modern.send(&modern_request(1, "tools/list", json!({})));
    let modern_listed = modern.recv(RECV_TIMEOUT);
    modern.close_stdin();
    assert_eq!(modern.wait_for_exit(RECV_TIMEOUT), 0);

    let legacy_tools = legacy_listed["result"]["tools"]
        .as_array()
        .expect("legacy tools/list 必须返回数组");
    let modern_tools = modern_listed["result"]["tools"]
        .as_array()
        .expect("modern tools/list 必须返回数组");
    assert_eq!(modern_tools.len(), 7);
    assert_eq!(
        legacy_tools, modern_tools,
        "同一服务的注册面不得随连接或协议时代变化"
    );
    assert_eq!(tool_names(&legacy_listed), TOOL_NAMES.to_vec());
}
