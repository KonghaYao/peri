//! Spike：Builtin MCP 的同进程内存 transport（**只做实验验证**，不含生产逻辑）。
//!
//! 背景：v4 设计允许 Builtin MCP —— Peri 内部装配 `rmcp::ServerHandler`，用
//! `tokio::io::duplex` 分半做内存 transport，client 侧仍走生产的
//! `serve_client_auto`（`client/transport.rs`）。本文件用真实代码回答三个决定后续
//! 设计的问题：
//!
//! - **Q1**：对端是**真实** `ServerHandler`（`discover` 用 rmcp 默认实现）时，Auto
//!   lifecycle 走 modern（inline `server/discover`）还是 legacy（`initialize`）？
//!   modern 路径下 `peer.peer_info()` 是否仍为 `Some`？（`initialize.rs:376-392`
//!   依赖它读 `channel_capable` / `server_info.version`。）
//! - **Q2**：client 侧 `close_with_timeout` 之后，同进程 server task 是否靠 duplex
//!   EOF **自然收敛**，还是必须显式 `abort()`？
//! - **Q3**：声明 1 个工具的真实 `ServerHandler`，经这条链路后
//!   `peer.list_all_tools()` 能否拿到它、`tools/call` 能否真实往返？
//!
//! 夹具与证据边界：
//! - server 半边是真实 `rmcp::serve_server`（不是 `readiness_test.rs` 那样手搓
//!   JSON-RPC 假 peer），client 半边是生产 `serve_client_auto`（真实 Auto
//!   lifecycle、真实 `peer_info`）。两侧都用 `(ReadHalf, WriteHalf)` 元组交给
//!   `IntoTransport`，即生产路径使用的 `transport-async-rw` 适配器。
//! - 「client 实际走了哪条握手路径」由 server 侧读半个上的 [`MethodTap`] 直接记录
//!   JSON-RPC method 序列（线路级证据）。**不能**靠覆写 handler 计数：`discover` /
//!   `initialize` 走 rmcp 默认实现时没有插桩点，计数会恒为 0 从而误判路径。
//! - 两种 `discover` 形态共用同一份 `get_info` / `list_tools` / `call_tool` 实现，
//!   唯一差异只在 `discover`，保证实验变量单一。
//! - duplex 缓冲 [`DUPLEX_BUF`] 取 8 KiB（与 `readiness_test.rs` 夹具一致）：本 spike
//!   的每帧是几百字节级的 JSON-RPC 行，远小于缓冲；两侧读写都在各自 task 中推进，
//!   不存在「写满缓冲又无人读」的自锁，8 KiB 足够且不放大单测内存。
//! - 每个用例都有界收尾（`close_with_timeout` + 有界等待 + 必要时 `abort`），不留
//!   orphan task；Q2 的等待上界是 [`SERVER_CONVERGE_TIMEOUT`]。

use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::Duration,
};

use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
        DiscoverRequestMethod, DiscoverResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
    },
    serve_client_with_lifecycle, serve_server,
    service::{QuitReason, RequestContext, RoleServer},
    ClientLifecycleMode, ErrorData as McpError, ServerHandler,
};
use tokio::{
    io::{AsyncRead, DuplexStream, ReadBuf},
    task::JoinHandle,
    time::timeout,
};

use super::apps::McpCapabilityProfile;
use super::client::{mcpp_client_info_for_profile, serve_client_auto, McpServiceWrapper};

// ─── constants ────────────────────────────────────────────────────────────────

/// duplex 双向缓冲上界：单帧 JSON-RPC 行只有几百字节，8 KiB 与现有夹具一致。
const DUPLEX_BUF: usize = 8 * 1024;
/// client 侧握手（`serve_client_auto` 内建）上界。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// client 侧关闭上界（Q2 与收尾共用）。
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
/// Q2：client 关闭后，server task 自然收敛的等待上界（有界，不无限 await）。
const SERVER_CONVERGE_TIMEOUT: Duration = Duration::from_millis(1000);

const SPIKE_TOOL: &str = "builtin_spike_echo";
const SPIKE_SERVER_NAME: &str = "builtin-spike-server";
const SPIKE_SERVER_VERSION: &str = "0.0.1-spike";
const SPIKE_REPLY: &str = "builtin-spike-reply:builtin_spike_echo";

// ─── probes ───────────────────────────────────────────────────────────────────

/// server 半边 `discover` 的两种形态：本 spike 唯一的实验变量。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiscoverShape {
    /// rmcp 默认实现：回真实 `DiscoverResult`（现代协议）。
    RmcpDefault,
    /// 覆写为 `-32601`，对齐 `readiness_test.rs` 假 peer 的做法，逼 Auto 回退 legacy。
    MethodNotFound,
}

/// server 侧观测事实：线路级 method 序列 + 真实落到 handler 的工具调用次数。
#[derive(Debug, Default)]
struct LifecycleProbe {
    /// server 读半个上实际收到的 JSON-RPC method 序列（含 notification）。
    methods: parking_lot::Mutex<Vec<String>>,
    list_tools: AtomicUsize,
    call_tool: AtomicUsize,
}

impl LifecycleProbe {
    fn record_method(&self, method: &str) {
        self.methods.lock().push(method.to_owned());
    }

    fn methods(&self) -> Vec<String> {
        self.methods.lock().clone()
    }

    fn list_tools(&self) -> usize {
        self.list_tools.load(Ordering::SeqCst)
    }

    fn call_tool(&self) -> usize {
        self.call_tool.load(Ordering::SeqCst)
    }

    /// 观测快照：断言失败信息里带上它，失败即证据。
    fn describe(&self) -> String {
        format!(
            "server 侧观测：methods={:?} tools/list={} tools/call={}",
            self.methods(),
            self.list_tools(),
            self.call_tool()
        )
    }
}

/// 观测用 `AsyncRead` 透传包装：记录 server 读到的每一行 JSON-RPC 的 method。
///
/// 这是「client 实际走了哪条握手路径」的线路级证据：不依赖 handler 覆写，也不依赖
/// 对 rmcp 内部实现的推断（默认实现的 `discover` / `initialize` 无法插桩）。
/// 实现是纯透传：直接读进调用方的 `ReadBuf`，只额外观察本次新填充的字节。
struct MethodTap<R> {
    inner: R,
    probe: Arc<LifecycleProbe>,
    pending: Vec<u8>,
}

impl<R> MethodTap<R> {
    fn new(inner: R, probe: Arc<LifecycleProbe>) -> Self {
        Self {
            inner,
            probe,
            pending: Vec::new(),
        }
    }

    /// 按换行切分已读字节，解析出每帧的 `method`（请求与 notification 都有该字段）。
    fn observe(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=newline).collect();
            let Ok(message) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            if let Some(method) = message.get("method").and_then(|value| value.as_str()) {
                self.probe.record_method(method);
            }
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for MethodTap<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let poll = Pin::new(&mut this.inner).poll_read(cx, buf);
        if buf.filled().len() > before {
            let observed = buf.filled()[before..].to_vec();
            this.observe(&observed);
        }
        poll
    }
}

// ─── server fixture ───────────────────────────────────────────────────────────

fn spike_schema() -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({ "type": "object", "properties": {} })
        .as_object()
        .expect("json! 对象字面量必为 object")
        .clone()
}

/// 两种形态共用的 `get_info`。
fn spike_server_info() -> ServerInfo {
    ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        .with_server_info(Implementation::new(SPIKE_SERVER_NAME, SPIKE_SERVER_VERSION))
}

/// 两种形态共用的 `tools/list`：声明恰好 1 个工具。
fn spike_list_tools(probe: &LifecycleProbe) -> ListToolsResult {
    probe.list_tools.fetch_add(1, Ordering::SeqCst);
    ListToolsResult::with_all_items(vec![Tool::new(
        SPIKE_TOOL,
        "builtin spike 夹具工具：回显固定字符串",
        spike_schema(),
    )])
}

/// 两种形态共用的 `tools/call`：返回可辨认的固定文本。
fn spike_call_tool(
    probe: &LifecycleProbe,
    request: &CallToolRequestParams,
) -> Result<CallToolResponse, McpError> {
    probe.call_tool.fetch_add(1, Ordering::SeqCst);
    if request.name.as_ref() != SPIKE_TOOL {
        return Err(McpError::invalid_params(
            format!("unknown tool: {}", request.name),
            None,
        ));
    }
    Ok(CallToolResponse::Complete(CallToolResult::success(vec![
        ContentBlock::text(SPIKE_REPLY),
    ])))
}

/// (a) `discover` 走 rmcp 默认实现的对端。
#[derive(Clone)]
struct DefaultDiscoverServer {
    probe: Arc<LifecycleProbe>,
}

impl ServerHandler for DefaultDiscoverServer {
    fn get_info(&self) -> ServerInfo {
        spike_server_info()
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(spike_list_tools(&self.probe))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        spike_call_tool(&self.probe, &request)
    }
}

/// (b) `discover` 覆写为 `method_not_found` 的对端（其余与 (a) 完全相同）。
#[derive(Clone)]
struct RejectDiscoverServer {
    probe: Arc<LifecycleProbe>,
}

impl ServerHandler for RejectDiscoverServer {
    fn get_info(&self) -> ServerInfo {
        spike_server_info()
    }

    async fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> Result<DiscoverResult, McpError> {
        Err(McpError::method_not_found::<DiscoverRequestMethod>())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(spike_list_tools(&self.probe))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        spike_call_tool(&self.probe, &request)
    }
}

// ─── wiring ───────────────────────────────────────────────────────────────────

/// server task 观测句柄：外层是 task join 结果，内层是 rmcp 服务的退出原因。
type ServerTask = JoinHandle<Result<QuitReason, tokio::task::JoinError>>;

/// 一条已握手的同进程链路 + server task 观测句柄 + server 侧观测。
struct BuiltinPair {
    service: McpServiceWrapper,
    server_task: ServerTask,
    probe: Arc<LifecycleProbe>,
}

impl BuiltinPair {
    /// 夹具收尾：关闭 client（这会释放 duplex 写半边），有界等待 server task，
    /// 未收敛时 abort，确保测试不留 orphan task。
    async fn shutdown(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        if timeout(SERVER_CONVERGE_TIMEOUT, &mut self.server_task)
            .await
            .is_err()
        {
            self.server_task.abort();
            let _ = self.server_task.await;
        }
    }
}

/// 在 server 半边装配真实 `rmcp::serve_server`（读半加 [`MethodTap`]），
/// 返回可观测的 task 句柄。
fn spawn_builtin_server<S>(server: S, io: DuplexStream, probe: Arc<LifecycleProbe>) -> ServerTask
where
    S: ServerHandler,
{
    let (read, write) = tokio::io::split(io);
    let tapped = MethodTap::new(read, probe);
    tokio::spawn(async move {
        let running = match serve_server(server, (tapped, write)).await {
            Ok(running) => running,
            Err(error) => panic!("builtin spike server 握手装配失败：{error}"),
        };
        running.waiting().await
    })
}

/// 装配一条同进程链路：client 半边走生产 `serve_client_auto`（Auto lifecycle）。
async fn connect(shape: DiscoverShape) -> BuiltinPair {
    let (client_io, server_io) = tokio::io::duplex(DUPLEX_BUF);
    let probe = Arc::new(LifecycleProbe::default());
    let server_task = match shape {
        DiscoverShape::RmcpDefault => spawn_builtin_server(
            DefaultDiscoverServer {
                probe: Arc::clone(&probe),
            },
            server_io,
            Arc::clone(&probe),
        ),
        DiscoverShape::MethodNotFound => spawn_builtin_server(
            RejectDiscoverServer {
                probe: Arc::clone(&probe),
            },
            server_io,
            Arc::clone(&probe),
        ),
    };
    let connect_result = serve_client_auto(
        tokio::io::split(client_io),
        None,
        None,
        &McpCapabilityProfile::disabled(),
        HANDSHAKE_TIMEOUT,
    )
    .await;
    match connect_result {
        Ok(Ok(service)) => BuiltinPair {
            service,
            server_task,
            probe,
        },
        Ok(Err(error)) => {
            server_task.abort();
            panic!(
                "client 侧握手失败（{shape:?}）；{}；错误：{error}",
                probe.describe()
            );
        }
        Err(elapsed) => {
            server_task.abort();
            panic!(
                "client 侧握手超时（{shape:?}）；{}；{elapsed}",
                probe.describe()
            );
        }
    }
}

/// 对照装配：client **跳过** `server/discover` 探测，直接用 legacy `initialize` 握手
/// （`ClientLifecycleMode::Initialize`，即 `serve_client_auto` 之外的纯 legacy 路径）。
///
/// 用途：隔离「回退 legacy」与「探测被拒后在同一连接上回退 legacy」两种情形，
/// 判定工具发现失败到底由哪一环引起。包装成 `McpServiceWrapper::Default` 与
/// `serve_client_auto` 的 `None` 分支保持同一形态。
async fn connect_legacy_only() -> BuiltinPair {
    let (client_io, server_io) = tokio::io::duplex(DUPLEX_BUF);
    let probe = Arc::new(LifecycleProbe::default());
    let server_task = spawn_builtin_server(
        DefaultDiscoverServer {
            probe: Arc::clone(&probe),
        },
        server_io,
        Arc::clone(&probe),
    );
    let running = match serve_client_with_lifecycle(
        mcpp_client_info_for_profile(&McpCapabilityProfile::disabled()),
        tokio::io::split(client_io),
        ClientLifecycleMode::Initialize,
    )
    .await
    {
        Ok(running) => running,
        Err(error) => {
            server_task.abort();
            panic!("legacy-only 握手失败；{}；错误：{error}", probe.describe());
        }
    };
    BuiltinPair {
        service: McpServiceWrapper::Default(running),
        server_task,
        probe,
    }
}

/// `tools/call` 结果里的首个文本块。
fn first_text(result: &CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    })
}

/// Q3 的共用断言体：该链路必须能真实发现并调用工具。
async fn assert_tool_round_trip(pair: BuiltinPair, label: &str) {
    let peer = pair.service.peer().clone();

    let tools = peer.list_all_tools().await.unwrap_or_else(|error| {
        panic!(
            "Q3 tools/list 失败（{label}）：{error}；{}",
            pair.probe.describe()
        )
    });
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        names,
        vec![SPIKE_TOOL],
        "Q3 tools/list 未返回声明的工具（{label}）；{}",
        pair.probe.describe()
    );

    let called = peer
        .call_tool_once(CallToolRequestParams::new(SPIKE_TOOL))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "Q3 tools/call 失败（{label}）：{error}；{}",
                pair.probe.describe()
            )
        });
    let reply = match &called {
        CallToolResponse::Complete(result) => first_text(result),
        other => panic!("Q3 tools/call 未返回完整结果（{label}）：{other:?}"),
    };
    assert_eq!(
        reply.as_deref(),
        Some(SPIKE_REPLY),
        "Q3 tools/call 往返文本不符（{label}）；{}",
        pair.probe.describe()
    );
    assert_eq!(
        (pair.probe.list_tools(), pair.probe.call_tool()),
        (1, 1),
        "Q3 server 侧应各收到一次 tools/list 与 tools/call（{label}）；{}",
        pair.probe.describe()
    );

    println!(
        "Q3[{label}] tools={names:?} reply={reply:?}；{}",
        pair.probe.describe()
    );
    pair.shutdown().await;
}

// ─── Q1 ───────────────────────────────────────────────────────────────────────

/// Q1(a)：`discover` 为 rmcp 默认实现的真实对端 —— Auto 走哪条路径、peer_info 是否 Some。
#[tokio::test]
async fn builtin_spike_q1_default_discover_uses_modern_and_keeps_peer_info() {
    let pair = connect(DiscoverShape::RmcpDefault).await;
    let peer = pair.service.peer().clone();
    let observed = pair.probe.describe();

    assert_eq!(
        pair.probe.methods(),
        vec!["server/discover".to_string()],
        "Q1(a)：握手阶段应只发 server/discover（发过 initialize 即已回退 legacy）；{observed}"
    );

    let info = peer
        .peer_info()
        .unwrap_or_else(|| panic!("Q1(a)：modern 路径下 peer.peer_info() 为 None；{observed}"));
    assert_eq!(
        info.protocol_version,
        ProtocolVersion::V_2026_07_28,
        "Q1(a)：modern 路径应选 preferred 版本；{observed}"
    );
    assert_eq!(
        info.server_info
            .as_ref()
            .map(|server| server.version.as_str()),
        Some(SPIKE_SERVER_VERSION),
        "Q1(a)：initialize.rs:390-392 读 server_info.version，必须仍可读；{observed}"
    );
    assert!(
        info.capabilities.tools.is_some(),
        "Q1(a)：initialize.rs:376-385 读 capabilities，必须仍可读；{observed}"
    );
    // initialize.rs:376-385 的 channel_capable 走 experimental["claude/channel"]：
    // 本夹具不声明该扩展，观测到 false 才是「读了、确实没有」的正确语义。
    let channel_capable = info
        .capabilities
        .experimental
        .as_ref()
        .and_then(|experimental| experimental.get("claude/channel"))
        .is_some();
    assert!(
        !channel_capable,
        "Q1(a)：夹具未声明 claude/channel，channel_capable 应为 false；{observed}"
    );

    println!(
        "Q1(a) modern：methods={:?} protocol={:?} server_info_version={:?} tools_capability={} channel_capable={}",
        pair.probe.methods(),
        info.protocol_version,
        info.server_info
            .as_ref()
            .map(|server| server.version.as_str()),
        info.capabilities.tools.is_some(),
        channel_capable
    );
    pair.shutdown().await;
}

/// Q1(b)：`discover` 覆写为 `-32601` 的真实对端 —— 对照现有假 peer 的做法。
#[tokio::test]
async fn builtin_spike_q1_rejected_discover_falls_back_to_legacy() {
    let pair = connect(DiscoverShape::MethodNotFound).await;
    let peer = pair.service.peer().clone();
    let observed = pair.probe.describe();
    let methods = pair.probe.methods();

    assert_eq!(
        methods.first().map(String::as_str),
        Some("server/discover"),
        "Q1(b)：Auto 必须先探测 server/discover；{observed}"
    );
    assert!(
        methods.iter().any(|method| method == "initialize"),
        "Q1(b)：-32601 应触发 legacy initialize 回退；{observed}"
    );

    let info = peer
        .peer_info()
        .unwrap_or_else(|| panic!("Q1(b)：legacy 路径下 peer.peer_info() 为 None；{observed}"));
    assert_eq!(
        info.server_info
            .as_ref()
            .map(|server| server.version.as_str()),
        Some(SPIKE_SERVER_VERSION),
        "Q1(b)：legacy 路径的 server_info.version 应来自 initialize 结果；{observed}"
    );

    println!(
        "Q1(b) legacy：methods={methods:?} protocol={:?} server_info_version={:?}",
        info.protocol_version,
        info.server_info
            .as_ref()
            .map(|server| server.version.as_str())
    );
    pair.shutdown().await;
}

// ─── Q2 ───────────────────────────────────────────────────────────────────────

/// Q2：client 侧 `close_with_timeout` 后，同进程 server task 是否自然收敛。
///
/// 两种 `discover` 形态都跑：收敛发生在 transport 层（EOF），应与握手路径无关。
#[tokio::test]
async fn builtin_spike_q2_server_task_converges_after_client_close() {
    for shape in [DiscoverShape::RmcpDefault, DiscoverShape::MethodNotFound] {
        let mut pair = connect(shape).await;
        assert!(
            !pair.server_task.is_finished(),
            "Q2 前提：握手完成后 server task 仍在运行（{shape:?}）；{}",
            pair.probe.describe()
        );

        let close = pair.service.close_with_timeout(CLOSE_TIMEOUT).await;
        let finished_immediately = pair.server_task.is_finished();

        let joined = timeout(SERVER_CONVERGE_TIMEOUT, &mut pair.server_task).await;
        let finished_after_wait = pair.server_task.is_finished();

        let joined = match joined {
            Ok(joined) => joined,
            Err(_) => {
                let abandoned = pair.server_task.is_finished();
                pair.server_task.abort();
                panic!(
                    "Q2（{shape:?}）：client 关闭后 server task 未在 {SERVER_CONVERGE_TIMEOUT:?} \
                     内收敛（is_finished={abandoned}，close={close:?}），必须显式 abort"
                );
            }
        };
        assert!(
            finished_after_wait,
            "Q2（{shape:?}）：有界等待后 is_finished() 仍为 false；close={close:?}"
        );

        match &joined {
            Ok(Ok(QuitReason::Closed)) => {}
            other => panic!(
                "Q2（{shape:?}）：server task 不是靠输入流 EOF 自然收敛\
                 （期望 Ok(Ok(QuitReason::Closed))，实际 {other:?}）；close={close:?}"
            ),
        }

        println!(
            "Q2[{shape:?}]：close={close:?} finished_immediately={finished_immediately} \
             finished_after_wait={finished_after_wait} server_quit={joined:?}"
        );
    }
}

// ─── Q3 ───────────────────────────────────────────────────────────────────────

/// Q3(a)：真实对端声明 1 个工具，modern 路径下的发现与调用往返。
#[tokio::test]
async fn builtin_spike_q3_tools_round_trip_modern() {
    assert_tool_round_trip(connect(DiscoverShape::RmcpDefault).await, "modern").await;
}

/// Q3(b)：**发现被拒后回退 legacy** 的连接上，工具发现是否可用。
///
/// 本用例钉住实测行为（与「两种形态都能用」的直觉相反）：server 在收到非
/// `initialize` 的首个请求后就已把会话标记为 inline lifecycle
/// （`rmcp-3.1.4/src/service/server.rs:562` 的 `peer.require_request_metadata()`），
/// 即使随后 client 在同一连接上回退 legacy `initialize` 成功，后续 `tools/list`
/// 仍因缺少 per-request `_meta` 被 `-32602` 拒绝。
#[tokio::test]
async fn builtin_spike_q3_legacy_fallback_breaks_tool_listing() {
    let pair = connect(DiscoverShape::MethodNotFound).await;
    let peer = pair.service.peer().clone();
    let handshake = pair.probe.describe();

    // 握手本身是「成功」的（peer_info 可读），失败发生在工具发现。
    assert!(
        peer.peer_info().is_some(),
        "Q3(b) 前提：回退 legacy 后 peer_info 应可读；{handshake}"
    );

    let error = match peer.list_all_tools().await {
        Ok(tools) => panic!(
            "Q3(b)：假设被推翻——探测被拒后回退 legacy 的连接竟然还能 tools/list：\
             {tools:?}；{handshake}"
        ),
        Err(error) => error.to_string(),
    };
    // 失败后重读观测：`tools/list` 是否真的上了线路（区别于「client 侧就没发」）。
    let observed = pair.probe.describe();
    assert_eq!(
        pair.probe.methods(),
        vec![
            "server/discover".to_string(),
            "initialize".to_string(),
            "notifications/initialized".to_string(),
            "tools/list".to_string(),
        ],
        "Q3(b)：期望线路序列为 discover → initialize → initialized → tools/list；{observed}"
    );
    assert!(
        error.contains("-32602"),
        "Q3(b)：期望 -32602（invalid params，缺 per-request _meta）；实际：{error}；{observed}"
    );
    assert!(
        error.contains("io.modelcontextprotocol/protocolVersion"),
        "Q3(b)：期望错误指向缺失的 inline lifecycle 元数据键；实际：{error}；{observed}"
    );
    assert_eq!(
        pair.probe.list_tools(),
        0,
        "Q3(b)：tools/list 根本没落到 handler（被 server 会话层拒绝）；{observed}"
    );

    println!("Q3(b) legacy 回退后 tools/list 被拒：{error}；{observed}");
    pair.shutdown().await;
}

/// 对照实验：client 从不发 `server/discover`、直接用 legacy `initialize` 握手时，
/// 同一份真实 server 的工具发现是**可用**的。
///
/// 结论用途：把「回退 legacy」本身与「探测被拒后在同一连接上回退」区分开——
/// 失败来自后者（server 会话已进入 inline 模式要求 per-request `_meta`），
/// 不是 legacy 协议本身不可用。
#[tokio::test]
async fn builtin_spike_legacy_only_handshake_lists_tools() {
    assert_tool_round_trip(connect_legacy_only().await, "legacy-only 直连").await;
}
