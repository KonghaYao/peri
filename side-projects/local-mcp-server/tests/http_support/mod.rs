//! WP-006 HTTP 测试支撑：raw HTTP/1.1 客户端、夹具 `ServerHandler` 与真实 socket 服务端。
//!
//! 三条刻意选择：
//!
//! 1. **不使用 rmcp 的 client**：本模块自带阻塞式 HTTP/1.1 客户端（含 chunked 解码与 SSE
//!    解析），因此 A-006「至少一套不用 rmcp client 生成或解析」由构造保证，而不是靠声明。
//! 2. **夹具 handler 逐字消费 `tests/fixtures/schemas/*.json`**：`tools/list` 的
//!    description/inputSchema 与黄金夹具完全一致，缺失必填参数的文案也来自夹具的
//!    `required_error_messages`，因此 HTTP 用例验证的是冻结契约而不是我临时编的行为。
//!    真实 MCP core（WP-005 的 `SandboxServer`）接入后，同一套原始请求可直接复用
//!    （见 `Artifacts` 的接力说明）。
//! 3. **身份事实由夹具回显**：`structuredContent.identity` 暴露 principal / client_instance
//!    / 会话头 / 协议版本，供测试断言"身份来自传输层而不是客户端自报字段"。这是测试
//!    探针，不属于产品 wire 契约。
//!
//! 本模块被多个测试二进制共用，各自只用到子集，因此允许 dead_code。

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode, ErrorData,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ResultType, ServerCapabilities,
    ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use local_mcp_server::auth::principal_from_parts;
use local_mcp_server::config::{
    AuthConfig, Config, HttpConfig, TokenSource, TransportKind, WorkspaceConfig,
};
use local_mcp_server::transport::http::serve_http;
use local_mcp_server::wire::{
    resolve_tool_name, HEADER_MCP_METHOD, HEADER_MCP_NAME, MCP_LEGACY_PROTOCOL_VERSION,
    MCP_MODERN_PROTOCOL_VERSION, META_KEY_CLIENT_CAPABILITIES, META_KEY_CLIENT_INFO,
    META_KEY_PROTOCOL_VERSION, TOOL_NAMES,
};

/// 测试用 MCP 端点路径（SDK 不约束路径，服务端把它当作单一 MCP endpoint）。
pub const MCP_PATH: &str = "/mcp";

/// 1 号主体（bearer 模式）与 2 号主体（另一个 token）用到的常量由运行时随机生成。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

// ───────────────────────────────── 夹具工具表 ─────────────────────────────────

/// 单个工具的黄金夹具（`tests/fixtures/schemas/<tool>.json`）。
#[derive(Debug, Clone)]
pub struct ToolFixture {
    /// 规范工具名。
    pub name: String,
    /// 逐字 description。
    pub description: String,
    /// 逐字 inputSchema。
    pub schema: Value,
    /// 别名（只参与 `tools/call` 名称解析）。
    pub aliases: Vec<String>,
    /// `required` 列表。
    pub required: Vec<String>,
    /// 缺失必填参数的文案（键为字段名或 `a|b` 形式的组合）。
    pub required_messages: BTreeMap<String, String>,
}

impl ToolFixture {
    /// 转为 SDK 工具条目（description/inputSchema 与夹具逐字一致）。
    pub fn to_tool(&self) -> Tool {
        let schema: Map<String, Value> = self
            .schema
            .as_object()
            .cloned()
            .expect("fixture inputSchema 必须是对象");
        Tool::new(
            self.name.clone(),
            self.description.clone(),
            std::sync::Arc::new(schema),
        )
    }
}

/// 读取全部七个工具夹具（顺序与 `TOOL_NAMES` 一致）。
pub fn load_tool_fixtures() -> Vec<ToolFixture> {
    let index = read_json(&fixtures_dir().join("_index.json"));
    let aliases = index
        .get("aliases")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    TOOL_NAMES
        .iter()
        .map(|name| {
            let raw =
                read_json(&fixtures_dir().join(format!("{}.json", name.to_ascii_lowercase())));
            let input_schema = raw
                .get("inputSchema")
                .cloned()
                .expect("fixture inputSchema");
            let required = input_schema
                .get("required")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let required_messages = raw
                .get("required_error_messages")
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(key, value)| {
                            value.as_str().map(|text| (key.clone(), text.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            ToolFixture {
                name: (*name).to_string(),
                description: raw
                    .get("description")
                    .and_then(Value::as_str)
                    .expect("fixture description")
                    .to_string(),
                schema: input_schema,
                aliases: aliases
                    .get(*name)
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
                required,
                required_messages,
            }
        })
        .collect()
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schemas")
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("读取夹具 {} 失败: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("解析夹具 {} 失败: {error}", path.display()))
}

// ─────────────────────────────── 夹具 MCP handler ───────────────────────────────

/// 身份事实：MCP core 在真实实现里会把它交给工具执行层，这里回显给测试断言。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IdentityFacts {
    /// 传输层绑定的主体（未接线时为 `None`）。
    pub principal: Option<String>,
    /// 传输层绑定的连接实例。
    pub client_instance: Option<String>,
    /// 请求头里的 `Mcp-Session-Id`（legacy 生命周期）。
    pub session_header: Option<String>,
    /// SDK 判定的协议版本。
    pub protocol_version: Option<String>,
    /// 客户端自报的 `clientInfo.name`（**不参与**授权，仅用于对照）。
    pub client_info_name: Option<String>,
}

/// 一次 `tools/call` 的观察记录。
#[derive(Debug, Clone)]
pub struct CallObservation {
    /// 请求中的原始工具名（别名原样保留）。
    pub requested_name: String,
    /// 解析后的规范名（未知工具为 `None`）。
    pub canonical_name: Option<String>,
    /// 身份事实。
    pub identity: IdentityFacts,
    /// 是否返回了业务错误。
    pub is_error: bool,
}

/// 跨会话共享的调用观察表（SDK 会为每个会话/无状态请求各构造一次 handler）。
pub type ObservationSink = std::sync::Arc<std::sync::Mutex<Vec<CallObservation>>>;

/// 新建观察表。
pub fn observation_sink() -> ObservationSink {
    std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))
}

/// 读取观察表中的全部记录。
pub fn observed_calls(sink: &ObservationSink) -> Vec<CallObservation> {
    sink.lock().expect("observations lock").clone()
}

/// 夹具 `ServerHandler`：七工具、别名解析、缺失必填参数的业务错误、身份回显。
#[derive(Clone)]
pub struct FixtureHandler {
    fixtures: std::sync::Arc<Vec<ToolFixture>>,
    tools: std::sync::Arc<Vec<Tool>>,
    observations: ObservationSink,
}

impl Default for FixtureHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl FixtureHandler {
    /// 以黄金夹具构造（自带独立观察表）。
    pub fn new() -> Self {
        Self::with_sink(observation_sink())
    }

    /// 以黄金夹具构造，并写入共享观察表。
    pub fn with_sink(sink: ObservationSink) -> Self {
        let fixtures = load_tool_fixtures();
        let tools = fixtures.iter().map(ToolFixture::to_tool).collect();
        Self {
            fixtures: std::sync::Arc::new(fixtures),
            tools: std::sync::Arc::new(tools),
            observations: sink,
        }
    }

    /// 工厂函数（传给 [`serve_http`]）：每个会话构造一个新 handler，共享观察表。
    pub fn factory_for(
        sink: ObservationSink,
    ) -> impl Fn() -> Result<Self, std::io::Error> + Send + Sync + 'static {
        move || Ok(FixtureHandler::with_sink(sink.clone()))
    }

    /// 到目前为止观察到的调用（按顺序）。
    pub fn observations(&self) -> Vec<CallObservation> {
        observed_calls(&self.observations)
    }

    /// 夹具工具表。
    pub fn fixtures(&self) -> &[ToolFixture] {
        &self.fixtures
    }

    fn record(&self, observation: CallObservation) {
        self.observations
            .lock()
            .expect("observations lock")
            .push(observation);
    }

    fn fixture(&self, canonical: &str) -> Option<&ToolFixture> {
        self.fixtures
            .iter()
            .find(|fixture| fixture.name == canonical)
    }
}

/// 从请求上下文提取身份事实与客户端自报信息。
pub fn identity_facts(context: &RequestContext<RoleServer>) -> IdentityFacts {
    let parts = context.extensions.get::<http::request::Parts>();
    let principal = parts.and_then(principal_from_parts);
    IdentityFacts {
        principal: principal.map(|found| found.principal().to_string()),
        client_instance: principal.map(|found| found.client_instance().to_string()),
        session_header: parts
            .and_then(|parts| parts.headers.get("Mcp-Session-Id"))
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        protocol_version: context.protocol_version().map(|found| found.to_string()),
        client_info_name: context.client_info().map(|info| info.name.to_string()),
    }
}

impl ServerHandler for FixtureHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::new(
                "local-mcp-server-fixture",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions("WP-006 HTTP 传输夹具：七工具 schema 逐字来自黄金夹具。")
    }

    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        std::borrow::Cow::Owned(vec![
            ProtocolVersion::V_2026_07_28,
            ProtocolVersion::V_2025_11_25,
        ])
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        // 别名也解析：SDK 用本方法取 schema 做 `Mcp-Param-*` 校验。
        let canonical = resolve_tool_name(name)?;
        self.tools
            .iter()
            .find(|tool| tool.name.as_ref() == canonical)
            .cloned()
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.as_ref().clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let requested = request.name.to_string();
        let identity = identity_facts(&context);
        let Some(canonical) = resolve_tool_name(&requested) else {
            self.record(CallObservation {
                requested_name: requested.clone(),
                canonical_name: None,
                identity,
                is_error: true,
            });
            return Err(ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                format!("Unknown tool: {requested}"),
                None,
            ));
        };

        let arguments = request
            .arguments
            .clone()
            .map(Value::Object)
            .unwrap_or(Value::Object(Map::new()));
        let fixture = self.fixture(canonical).expect("fixture 必须存在");

        // 缺失必填参数 → 工具业务错误（与源实现同层），文案取自夹具。
        for key in &fixture.required {
            if arguments.get(key).is_none() {
                let message = fixture
                    .required_messages
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| format!("missing required parameter `{key}`"));
                self.record(CallObservation {
                    requested_name: requested,
                    canonical_name: Some(canonical.to_string()),
                    identity,
                    is_error: true,
                });
                return Ok(CallToolResponse::Complete(CallToolResult::error(vec![
                    ContentBlock::text(message),
                ])));
            }
        }

        self.record(CallObservation {
            requested_name: requested.clone(),
            canonical_name: Some(canonical.to_string()),
            identity: identity.clone(),
            is_error: false,
        });

        let structured = json!({
            "tool": canonical,
            "ok": true,
            "truncated": false,
            "arguments": arguments,
            "requested_name": requested,
            "identity": identity,
        });
        let mut result = CallToolResult::success(vec![ContentBlock::text(format!(
            "{canonical} ok (fixture)"
        ))]);
        result.structured_content = Some(structured);
        result.result_type = Some(ResultType::COMPLETE);
        Ok(CallToolResponse::Complete(result))
    }
}

// ──────────────────────────────── 真实 socket 服务端 ────────────────────────────────

/// 在后台线程里跑真实 HTTP 传输的服务端；Drop 即关闭。
pub struct TestServer {
    addr: SocketAddr,
    shutdown: CancellationToken,
    thread: Option<std::thread::JoinHandle<()>>,
    sink: ObservationSink,
}

impl TestServer {
    /// 以默认配置（回环、随机端口、无认证）启动夹具服务端。
    pub fn start() -> Self {
        Self::start_with(base_config())
    }

    /// 指定配置启动夹具服务端。
    pub fn start_with(config: Config) -> Self {
        let sink = observation_sink();
        Self::start_with_sink(config, FixtureHandler::factory_for(sink.clone()), sink)
    }

    /// 指定配置与 handler 工厂启动（WP-007 接真实 core 时复用本入口）。
    pub fn start_with_factory<H, F>(config: Config, factory: F, sink: ObservationSink) -> Self
    where
        H: ServerHandler + Send + 'static,
        F: Fn() -> Result<H, std::io::Error> + Send + Sync + 'static,
    {
        Self::start_with_sink(config, factory, sink)
    }

    /// 指定配置、handler 工厂与共享观察表启动。
    pub fn start_with_sink<H, F>(config: Config, factory: F, sink: ObservationSink) -> Self
    where
        H: ServerHandler + Send + 'static,
        F: Fn() -> Result<H, std::io::Error> + Send + Sync + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime");
            runtime.block_on(async move {
                let server = match serve_http(&config, factory).await {
                    Ok(server) => server,
                    Err(error) => {
                        let _ = tx.send(Err(error.to_string()));
                        return;
                    }
                };
                let addr = server.local_addr();
                let token = server.shutdown_token();
                if tx.send(Ok((addr, token.clone()))).is_err() {
                    return;
                }
                token.cancelled().await;
                server.shutdown().await;
            });
        });

        let (addr, shutdown) = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => panic!("HTTP 传输启动失败: {error}"),
            Err(error) => panic!("等待 HTTP 传输启动超时: {error}"),
        };
        Self {
            addr,
            shutdown,
            thread: Some(thread),
            sink,
        }
    }

    /// 服务端监听地址。
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 共享观察表（夹具 handler 的调用记录）。
    pub fn observations(&self) -> Vec<CallObservation> {
        observed_calls(&self.sink)
    }

    /// 建立一条新的原始 TCP 连接（每条连接对应一个连接实例）。
    pub fn connect(&self) -> RawClient {
        RawClient::connect(self.addr)
    }

    /// 显式关闭并等待后台线程退出。
    pub fn shutdown(&mut self) {
        self.shutdown.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ─────────────────────────────── 阻塞式 raw HTTP 客户端 ───────────────────────────────

/// 一条原始响应。
#[derive(Debug, Clone)]
pub struct RawResponse {
    /// HTTP 状态码。
    pub status: u16,
    /// 原因短语。
    pub reason: String,
    /// 响应头（保持原始大小写）。
    pub headers: Vec<(String, String)>,
    /// 解码后的 body（chunked 已还原）。
    pub body: Vec<u8>,
}

impl RawResponse {
    /// 按名字取响应头（大小写不敏感）。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// body 的文本形式。
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// body 解析为 JSON。
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("body 不是 JSON: {error}\n{}", self.body_text()))
    }

    /// `Content-Type` 头。
    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
    }

    /// 是否为 SSE 响应。
    pub fn is_event_stream(&self) -> bool {
        self.content_type()
            .is_some_and(|value| value.starts_with("text/event-stream"))
    }

    /// 把 body 当作 SSE 解析出所有 JSON 消息（忽略注释/保活/非 JSON 数据行）。
    pub fn sse_messages(&self) -> Vec<Value> {
        let text = self.body_text();
        let mut messages = Vec::new();
        let mut data_lines: Vec<String> = Vec::new();
        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() {
                if !data_lines.is_empty() {
                    let joined = data_lines.join("\n");
                    if let Ok(value) = serde_json::from_str::<Value>(&joined) {
                        messages.push(value);
                    }
                    data_lines.clear();
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
        }
        if !data_lines.is_empty() {
            if let Ok(value) = serde_json::from_str::<Value>(&data_lines.join("\n")) {
                messages.push(value);
            }
        }
        messages
    }

    /// 取第一条 JSON-RPC 消息（SSE 或普通 JSON）。
    pub fn first_message(&self) -> Value {
        if self.is_event_stream() {
            self.sse_messages()
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("SSE 响应没有 JSON 消息:\n{}", self.body_text()))
        } else {
            self.json()
        }
    }
}

/// 一个待发送的原始请求。
#[derive(Debug, Clone)]
pub struct RawRequest {
    /// 方法（GET/POST/DELETE…）。
    pub method: String,
    /// 请求目标（默认 [`MCP_PATH`]）。
    pub target: String,
    /// 附加头（保持顺序）。
    pub headers: Vec<(String, String)>,
    /// body 字节。
    pub body: Vec<u8>,
    /// 是否省略 `Host` 头（用于缺失 Host 策略）。
    pub omit_host: bool,
    /// HTTP 版本行。
    pub version: String,
}

impl RawRequest {
    /// 默认带 `Host`/`Accept`/`Content-Type`/`Content-Length` 的 JSON POST。
    pub fn post(body: Value) -> Self {
        Self {
            method: "POST".to_string(),
            target: MCP_PATH.to_string(),
            headers: vec![
                (
                    "Accept".to_string(),
                    "application/json, text/event-stream".to_string(),
                ),
                ("Content-Type".to_string(), "application/json".to_string()),
            ],
            body: serde_json::to_vec(&body).expect("serialize body"),
            omit_host: false,
            version: "HTTP/1.1".to_string(),
        }
    }

    /// 覆盖/追加一个头（同名时替换）。
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers
            .retain(|(key, _)| !key.eq_ignore_ascii_case(name));
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// 删除一个头。
    pub fn without_header(mut self, name: &str) -> Self {
        self.headers
            .retain(|(key, _)| !key.eq_ignore_ascii_case(name));
        self
    }

    /// 替换 body（不改头）。
    pub fn body_bytes(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    /// 指定方法与该方法的空 body。
    pub fn method(mut self, method: &str) -> Self {
        self.method = method.to_string();
        self
    }

    /// 指定请求目标。
    pub fn target(mut self, target: &str) -> Self {
        self.target = target.to_string();
        self
    }

    /// 省略 `Host` 头。
    pub fn omit_host(mut self) -> Self {
        self.omit_host = true;
        self
    }
}

/// 一次交换所处的阶段：决定「对端关闭连接」时能否安全重发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExchangePhase {
    /// 请求字节还没写完。对端已关闭 ⇒ 字节没送达 ⇒ 请求不可能被处理。
    Write,
    /// 请求写完，但一个响应字节都没读到（本连接上不存在被丢弃的响应）。
    ReadBeforeResponse,
    /// 响应已经读到一半才失败：绝不重发，直接报错。
    ReadMidResponse,
}

/// 一次交换的传输失败（带阶段信息，供重发判定与证据）。
#[derive(Debug)]
struct ExchangeFailure {
    phase: ExchangePhase,
    error: std::io::Error,
}

impl ExchangeFailure {
    fn new(phase: ExchangePhase, error: std::io::Error) -> Self {
        Self { phase, error }
    }

    /// 是否为「对端关闭/重置连接」类错误（macOS 上 `EPIPE`=32、`ECONNRESET`=54 都映射到这些 kind）。
    ///
    /// 刻意**不含**超时（`WouldBlock`/`TimedOut`）：超时意味着对端可能仍在处理请求，
    /// 重发有重复执行的风险，必须炸出来而不是静默重试。
    fn peer_closed(&self) -> bool {
        matches!(
            self.error.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::UnexpectedEof
        )
    }

    /// 能否换连接重发：只在「请求字节没有换来任何响应」的阶段成立。
    fn resendable(&self) -> bool {
        self.peer_closed()
            && matches!(
                self.phase,
                ExchangePhase::Write | ExchangePhase::ReadBeforeResponse
            )
    }

    fn describe(&self) -> String {
        let phase = match self.phase {
            ExchangePhase::Write => "写请求",
            ExchangePhase::ReadBeforeResponse => "读响应首行（尚未读到任何响应字节）",
            ExchangePhase::ReadMidResponse => "读响应中途（响应已开始）",
        };
        format!("{phase}失败: {}", self.error)
    }
}

/// 写前连接探测结果。
enum Probe {
    /// 连接仍可用（对端没关闭，也没有残留未读字节）。
    Usable,
    /// 对端已经关闭（FIN/RST 已到达本机）；原因字符串用于证据。
    PeerClosed(String),
    /// 连接上还有上一次交换没读完的字节：wire 层异常，不静默丢弃。
    StaleBytes(usize),
}

/// 建立一条带超时的回环 TCP 连接（首次连接与重连共用）。
fn connect_stream(addr: SocketAddr) -> TcpStream {
    let stream =
        TcpStream::connect(addr).unwrap_or_else(|error| panic!("连接 {addr} 失败: {error}"));
    stream
        .set_read_timeout(Some(DEFAULT_TIMEOUT))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(DEFAULT_TIMEOUT))
        .expect("write timeout");
    stream
}

/// 原始 HTTP/1.1 客户端（阻塞、支持 keep-alive 与 chunked）。
///
/// **对「服务端已关闭连接」是确定性的**（GAP-009 / F-GATES-02 的修复点）：
///
/// - 写前用非阻塞 `peek` 探测连接可复用性，对端已关闭（FIN/整条连接被重置）就换新连接；
/// - 写请求时撞上 `EPIPE`/`ECONNRESET`（对端在本机写入前就已关闭）时，换新连接**逐字重发
///   同一条请求**（同一个 JSON-RPC id、同一组头、同一个 body），至多一次；
/// - 只在**复用连接**（本连接已成功交换过 ≥1 次）上重发；新连接第一次就失败属于真实异常，
///   必须炸出来而不是被重试掩盖；
/// - 响应已经读到一半才失败绝不重发；
/// - 每次重连都计数（[`RawClient::reconnects`]）并写进 wire 日志，重连不是静默行为。
///
/// 这样处理的原因（真实 broker 的形态）：`src/transport/http.rs` 的 401 与 SDK 的
/// 400/403/413 都在读完请求体之前返回；**当请求体此刻还没被读进服务端缓冲**（分片到达，
/// 负载越高越常见）时，连接随未读 body 一起被丢弃，对端看到的是 RST 而不是干净的 FIN，
/// 而响应本身不带 `connection: close`，客户端无法靠响应头预判
/// （实测见 `artifacts/test-results/FIXHTTP-r4/50-probe-keepalive.json`）。
///
/// 重发只在「本连接上没有任何响应字节」时发生；本产品里这类连接关闭只出现在
/// 「未读 body 即拒绝」的路径上，那些路径不会把请求交给 MCP core，因此不会重复执行工具调用。
/// 残余风险（如实登记）：若服务端在**已经处理**请求之后异常断开，重发会把同一条请求再送一次；
/// 这种情况会由用例自身的断言（工具结果、broker 退出码、容器残留）暴露，而不是被静默掩盖。
pub struct RawClient {
    reader: BufReader<TcpStream>,
    addr: SocketAddr,
    /// 当前连接上已完成的交换次数（0 = 刚建立的新连接，不允许重发）。
    exchanges: usize,
    /// 因对端关闭而换连接的次数（证据探针，不参与任何断言放宽）。
    reconnects: usize,
}

impl RawClient {
    /// 连接服务端。
    pub fn connect(addr: SocketAddr) -> Self {
        Self {
            reader: BufReader::new(connect_stream(addr)),
            addr,
            exchanges: 0,
            reconnects: 0,
        }
    }

    /// 服务端地址。
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 本客户端因对端关闭而重连的累计次数（证据探针）。
    pub fn reconnects(&self) -> usize {
        self.reconnects
    }

    /// 发送一个请求并读取一条响应。
    pub fn send(&mut self, request: &RawRequest) -> RawResponse {
        let (head, body) = self.encode_request(request);
        let response = self.transact(head.as_bytes(), &body, "send");
        if wire_log_path().is_some() {
            let mut transcript = String::new();
            transcript.push_str("=== REQUEST ===\n");
            transcript.push_str(&redact_credentials(&head));
            transcript.push_str(&String::from_utf8_lossy(&body));
            transcript.push_str("\n=== RESPONSE ===\n");
            transcript.push_str(&format!(
                "HTTP/1.1 {} {}\n",
                response.status, response.reason
            ));
            for (name, value) in &response.headers {
                transcript.push_str(&format!("{name}: {value}\n"));
            }
            transcript.push('\n');
            transcript.push_str(&String::from_utf8_lossy(&response.body));
            transcript.push('\n');
            append_wire_log(&transcript);
        }
        response
    }

    /// 发送裸字节（用于构造畸形请求）。
    pub fn send_raw(&mut self, raw: &[u8]) -> RawResponse {
        self.transact(raw, &[], "send_raw")
    }

    /// 关闭写方向，观察服务端是否按 EOF 收尾。
    pub fn shutdown_write(&mut self) {
        let _ = self.reader.get_ref().shutdown(std::net::Shutdown::Write);
    }

    /// 读到 EOF 或超时；返回是否读到 EOF。
    pub fn read_to_eof(&mut self) -> bool {
        let mut buffer = [0u8; 512];
        loop {
            match self.reader.read(&mut buffer) {
                Ok(0) => return true,
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    }

    /// 构造请求字节：头与 body 分开返回（头用于 wire 日志脱敏，body 原样追加）。
    fn encode_request(&self, request: &RawRequest) -> (String, Vec<u8>) {
        let mut head = format!(
            "{} {} {}\r\n",
            request.method, request.target, request.version
        );
        let has_host = request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("host"));
        if !request.omit_host && !has_host {
            head.push_str(&format!("Host: {}\r\n", self.addr));
        }
        for (name, value) in &request.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str(&format!("Content-Length: {}\r\n", request.body.len()));
        head.push_str("\r\n");
        (head, request.body.clone())
    }

    /// 写前探测 + 一次交换；对端关闭时换连接**逐字重发**同一条请求（至多一次）。
    fn transact(&mut self, head: &[u8], body: &[u8], label: &str) -> RawResponse {
        let mut request = Vec::with_capacity(head.len() + body.len());
        request.extend_from_slice(head);
        request.extend_from_slice(body);

        self.ensure_reusable();
        match self.exchange(&request) {
            Ok(response) => response,
            Err(failure) if self.exchanges > 0 && failure.resendable() => {
                self.reconnect(&format!("{label}: {}", failure.describe()));
                self.exchange(&request).unwrap_or_else(|retry| {
                    panic!("{label}: 换连接重发后仍然失败：{}", retry.describe())
                })
            }
            Err(failure) => panic!("{label}: {}", failure.describe()),
        }
    }

    /// 写前判定连接可复用性：对端已关闭就换一条新连接（此时请求尚未发出，丢掉连接无副作用）。
    fn ensure_reusable(&mut self) {
        match self.probe() {
            Probe::Usable => {}
            Probe::PeerClosed(reason) => {
                self.reconnect(&format!("写前探测：对端已关闭（{reason}）"));
            }
            Probe::StaleBytes(count) => panic!(
                "连接上残留 {count} 字节未读数据：上一次响应之后服务端多写了字节，\
                 raw 客户端拒绝静默丢弃（wire 层异常，不是连接关闭）"
            ),
        }
    }

    /// 非阻塞 `peek` 探测（不消费任何字节）。
    fn probe(&mut self) -> Probe {
        let stream = self.reader.get_ref();
        if stream.set_nonblocking(true).is_err() {
            // 探测不可用时不做猜测：照常写入，写阶段的 EPIPE 仍会被 transact 兜住。
            return Probe::Usable;
        }
        let mut byte = [0u8; 1];
        let result = stream.peek(&mut byte);
        let _ = stream.set_nonblocking(false);
        match result {
            Ok(0) => Probe::PeerClosed("FIN：对端关闭了连接".to_string()),
            Ok(count) => Probe::StaleBytes(count.max(1)),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Probe::Usable,
            Err(error) => Probe::PeerClosed(error.to_string()),
        }
    }

    /// 换一条全新的 TCP 连接；计数并记录原因（重连不是静默行为）。
    fn reconnect(&mut self, because: &str) {
        let stream = connect_stream(self.addr);
        self.reader = BufReader::new(stream);
        self.exchanges = 0;
        self.reconnects += 1;
        append_wire_log(&format!(
            "=== RECONNECT ===\n原因: {because}\n新连接目标: {}\n重连序号: {}\n",
            self.addr, self.reconnects
        ));
    }

    /// 一次写入 + 一次读取；失败时返回带阶段信息的错误（不做任何重试）。
    fn exchange(&mut self, request: &[u8]) -> Result<RawResponse, ExchangeFailure> {
        self.write_all(request)
            .map_err(|error| ExchangeFailure::new(ExchangePhase::Write, error))?;
        let response = self.read_response()?;
        self.exchanges += 1;
        Ok(response)
    }

    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let stream = self.reader.get_mut();
        stream.write_all(bytes)?;
        stream.flush()
    }

    fn read_response(&mut self) -> Result<RawResponse, ExchangeFailure> {
        let status_line = self.read_line(ExchangePhase::ReadBeforeResponse)?;
        let mut parts = status_line.splitn(3, ' ');
        let version = parts.next().unwrap_or_default().to_string();
        assert!(
            version.starts_with("HTTP/1."),
            "非法状态行: {status_line:?}"
        );
        let status: u16 = parts
            .next()
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("非法状态码: {status_line:?}"));
        let reason = parts.next().unwrap_or_default().trim().to_string();

        let mut headers = Vec::new();
        loop {
            let line = self.read_line(ExchangePhase::ReadMidResponse)?;
            if line.trim().is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_string(), value.trim().to_string()));
            }
        }

        let header = |name: &str| -> Option<String> {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.clone())
        };

        let body = if header("transfer-encoding")
            .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
        {
            self.read_chunked_body()
        } else if let Some(length) = header("content-length").and_then(|value| value.parse().ok()) {
            self.read_exact_body(length)
        } else {
            self.read_until_eof_body()
        };

        Ok(RawResponse {
            status,
            reason,
            headers,
            body,
        })
    }

    /// 读一行；`phase` 决定这个失败能否安全重发。
    fn read_line(&mut self, phase: ExchangePhase) -> Result<String, ExchangeFailure> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => Err(ExchangeFailure::new(
                phase,
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "连接在读取响应时被关闭"),
            )),
            Ok(_) => Ok(line),
            Err(error) => Err(ExchangeFailure::new(phase, error)),
        }
    }

    /// 读一行；响应已经开始之后失败即不可重发，直接炸出来（保留修复前的报错口径）。
    fn read_line_strict(&mut self) -> String {
        self.read_line(ExchangePhase::ReadMidResponse)
            .unwrap_or_else(|failure| panic!("读取响应行失败: {}", failure.error))
    }

    fn read_exact_body(&mut self, length: usize) -> Vec<u8> {
        let mut body = vec![0u8; length];
        self.reader
            .read_exact(&mut body)
            .unwrap_or_else(|error| panic!("读取 body 失败: {error}"));
        body
    }

    fn read_until_eof_body(&mut self) -> Vec<u8> {
        let mut body = Vec::new();
        let _ = self.reader.read_to_end(&mut body);
        body
    }

    fn read_chunked_body(&mut self) -> Vec<u8> {
        let mut body = Vec::new();
        loop {
            let size_line = self.read_line_strict();
            let size_text = size_line.trim();
            let size_text = size_text.split(';').next().unwrap_or(size_text);
            let size = usize::from_str_radix(size_text.trim(), 16)
                .unwrap_or_else(|_| panic!("非法 chunk 长度: {size_line:?}"));
            if size == 0 {
                // trailer 段直到空行
                loop {
                    let line = self.read_line_strict();
                    if line.trim().is_empty() {
                        break;
                    }
                }
                break;
            }
            body.extend(self.read_exact_body(size));
            // chunk 结尾 CRLF
            let _ = self.read_line_strict();
        }
        body
    }
}

// ─────────────────────────────────── 配置助手 ───────────────────────────────────

/// 基础 HTTP 配置：回环随机端口、无认证、默认 body 上限。
pub fn base_config() -> Config {
    Config {
        transport: TransportKind::Http,
        http: HttpConfig {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            ..HttpConfig::default()
        },
        auth: AuthConfig::default(),
        workspace: WorkspaceConfig {
            root: std::env::temp_dir(),
        },
        ..Config::default()
    }
}

/// 运行时随机生成、写入私有临时文件的 token（不硬编码、不进入 git）。
pub struct TokenFile {
    dir: tempfile::TempDir,
    path: PathBuf,
    token: String,
}

impl TokenFile {
    /// 生成随机 token 并写盘（0600）。
    pub fn random() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        let token = format!("wp006-{}-{}", std::process::id(), uuid_like_random());
        std::fs::write(&path, &token).expect("写 token 文件");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
            permissions.set_mode(0o600);
            std::fs::set_permissions(&path, permissions).expect("chmod 600");
        }
        Self { dir, path, token }
    }

    /// token 值（只用于测试进程内断言，不写日志）。
    pub fn token(&self) -> &str {
        &self.token
    }

    /// 文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 对应的 `AuthConfig`。
    pub fn auth_config(&self) -> AuthConfig {
        AuthConfig {
            token: Some(TokenSource::File {
                path: self.path.clone(),
            }),
        }
    }

    /// 保活临时目录（测试期不删除）。
    pub fn keep(&self) -> &tempfile::TempDir {
        &self.dir
    }
}

/// 不引入额外依赖的 64 位随机串（uuid crate 已在依赖表内，直接使用）。
fn uuid_like_random() -> String {
    format!("{}", uuid::Uuid::new_v4().simple())
}

/// 原始 wire 记录文件：仅当 `WP006_RAW_WIRE_LOG` 指向一个路径时启用。
///
/// 这是证据导出开关（WP-008/WP-010 可核对真实字节），不改变任何断言行为；
/// 记录内容只包含测试自己构造的请求，不含 token（认证用例单独断言不泄露）。
fn wire_log_path() -> Option<std::path::PathBuf> {
    std::env::var_os("WP006_RAW_WIRE_LOG").map(std::path::PathBuf::from)
}

/// 证据文件里脱敏 `Authorization` 行（只影响导出文本，不影响真实发送的字节）。
fn redact_credentials(head: &str) -> String {
    head.lines()
        .map(|line| {
            if line.to_ascii_lowercase().starts_with("authorization:") {
                "Authorization: <redacted>".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n"
}

/// 追加一条 transcript（跨测试线程串行化），并在落盘后做导出后自检。
///
/// 与修复前的区别只有失败语义：打开/写入失败不再静默丢弃，导出后自检"wire 条数 > 0"。
/// 请求字节、请求序列与任何断言都不受本函数影响（它只在 `WP006_RAW_WIRE_LOG` 启用时工作）。
fn append_wire_log(entry: &str) {
    static LOCK: Mutex<()> = Mutex::new(());
    let Some(path) = wire_log_path() else {
        return;
    };
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => file,
        // 父目录不存在等情形以前是静默 `return`，会让"跑了测试但没有夹具"看起来正常。
        Err(error) => panic!(
            "WP006_RAW_WIRE_LOG 证据导出失败：无法打开 {path:?}（{error}）；\
             原始 wire 不得静默丢弃（该目录需预先存在）"
        ),
    };
    if let Err(error) = file.write_all(entry.as_bytes()) {
        panic!("WP006_RAW_WIRE_LOG 证据导出失败：写入 {path:?} 出错（{error}）");
    }
    if let Err(reason) = verify_wire_log_export(&path) {
        panic!("{reason}");
    }
}

/// 导出后自检：落盘文件必须在磁盘上真实含 > 0 条 wire 记录。
///
/// 记录数 = `=== REQUEST ===` 条数 + `=== RECONNECT ===` 条数（本模块导出的两种条目）。
/// 判据取自**磁盘内容**而不是内存计数，因此"目录不存在导致文件根本没建"、"写入未落盘"、
/// "空夹具"这三类静默证据缺口都会在此显式失败，而不是让证据面悄悄残缺。
/// 返回记录条数；失败时返回面向人的原因（由 `append_wire_log` 转成 panic）。
fn verify_wire_log_export(path: &Path) -> Result<usize, String> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        format!(
            "WP006_RAW_WIRE_LOG 证据导出失败：无法读回 {path:?}（{error}）——\
             导出文件不存在或不可读，属证据缺口，不是断言放宽"
        )
    })?;
    let records =
        text.matches("=== REQUEST ===").count() + text.matches("=== RECONNECT ===").count();
    if records == 0 {
        return Err(format!(
            "WP006_RAW_WIRE_LOG 证据导出自检失败：{path:?} 含 0 条 wire 记录（空夹具）；\
             导出开关启用时必须产出可核对的实际字节"
        ));
    }
    Ok(records)
}

// ─────────────────────────────── 生命周期请求构造 ───────────────────────────────

/// modern（`2026-07-28`）请求：自带 `_meta` 必填键，并带标准头。
///
/// `name` 仅在需要 `Mcp-Name` 的方法（`tools/call`、`resources/read`、`prompts/get`）传入。
pub fn modern_request(method: &str, params: Value, name: Option<&str>, id: u64) -> RawRequest {
    let mut params = params;
    if let Value::Object(map) = &mut params {
        let entry = map
            .entry("_meta".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(meta) = entry {
            meta.insert(
                META_KEY_PROTOCOL_VERSION.to_string(),
                Value::String(MCP_MODERN_PROTOCOL_VERSION.to_string()),
            );
            meta.insert(
                META_KEY_CLIENT_CAPABILITIES.to_string(),
                Value::Object(Map::new()),
            );
            meta.insert(
                META_KEY_CLIENT_INFO.to_string(),
                json!({ "name": "raw-http-fixture", "version": "1.0.0" }),
            );
        }
    }
    let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let mut request =
        RawRequest::post(body).header("MCP-Protocol-Version", MCP_MODERN_PROTOCOL_VERSION);
    request = request.header(HEADER_MCP_METHOD, method);
    if let Some(name) = name {
        request = request.header(HEADER_MCP_NAME, name);
    }
    request
}

/// modern `tools/call`。
pub fn modern_tools_call(tool: &str, arguments: Value, id: u64) -> RawRequest {
    modern_request(
        "tools/call",
        json!({ "name": tool, "arguments": arguments }),
        Some(tool),
        id,
    )
}

/// modern `tools/list`（无 `Mcp-Name`）。
pub fn modern_tools_list(id: u64) -> RawRequest {
    modern_request("tools/list", json!({}), None, id)
}

/// modern `server/discover`（请求只带 `_meta`）。
pub fn modern_discover(id: u64) -> RawRequest {
    modern_request("server/discover", json!({}), None, id)
}

/// legacy（`2025-11-25`）请求：不带 `_meta`，可携带会话头。
pub fn legacy_request(method: &str, params: Value, id: u64, session: Option<&str>) -> RawRequest {
    let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let mut request = RawRequest::post(body)
        .header("MCP-Protocol-Version", MCP_LEGACY_PROTOCOL_VERSION)
        .header(HEADER_MCP_METHOD, method);
    if let Some(session) = session {
        request = request.header("Mcp-Session-Id", session);
    }
    request
}

/// legacy `initialize`。
pub fn legacy_initialize(id: u64) -> RawRequest {
    RawRequest::post(json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": MCP_LEGACY_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "raw-http-fixture", "version": "1.0.0" },
        }
    }))
    .header("MCP-Protocol-Version", MCP_LEGACY_PROTOCOL_VERSION)
    .header(HEADER_MCP_METHOD, "initialize")
}

/// legacy `notifications/initialized`。
pub fn legacy_initialized_notification(session: &str) -> RawRequest {
    RawRequest::post(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .header("MCP-Protocol-Version", MCP_LEGACY_PROTOCOL_VERSION)
        .header(HEADER_MCP_METHOD, "notifications/initialized")
        .header("Mcp-Session-Id", session)
}

/// legacy `tools/call`（带会话头）。
pub fn legacy_tools_call(tool: &str, arguments: Value, id: u64, session: &str) -> RawRequest {
    legacy_request(
        "tools/call",
        json!({ "name": tool, "arguments": arguments }),
        id,
        Some(session),
    )
    .header(HEADER_MCP_NAME, tool)
}

/// 从响应里取出 JSON-RPC 消息（SSE 或 JSON）。
pub fn rpc_message(response: &RawResponse) -> Value {
    response.first_message()
}

/// 从响应里取出 `result`；若是 JSON-RPC error 则 panic 并显示错误。
pub fn rpc_result(response: &RawResponse) -> Value {
    let message = rpc_message(response);
    if let Some(error) = message.get("error") {
        panic!(
            "期望 result，得到 error: {error}\nstatus={} body={}",
            response.status,
            response.body_text()
        );
    }
    message.get("result").cloned().unwrap_or(Value::Null)
}

/// 从响应里取出 JSON-RPC error 的 `code`。
pub fn rpc_error_code(response: &RawResponse) -> i64 {
    rpc_message(response)
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("期望 JSON-RPC error，得到: {}", response.body_text()))
}

/// 从响应里取出 JSON-RPC error 的 `message`。
pub fn rpc_error_message(response: &RawResponse) -> String {
    rpc_message(response)
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `tools/call` 结果的 `structuredContent`。
pub fn structured_content(response: &RawResponse) -> Value {
    rpc_result(response)
        .get("structuredContent")
        .cloned()
        .unwrap_or(Value::Null)
}

/// 夹具回显的身份事实。
pub fn identity_of(response: &RawResponse) -> IdentityFacts {
    let raw = structured_content(response)
        .get("identity")
        .cloned()
        .unwrap_or(Value::Null);
    serde_json::from_value(raw).expect("identity 反序列化")
}

// ───────────────────────── 证据导出自检的两态自测 ─────────────────────────

/// `verify_wire_log_export` 的自测：**空**夹具必须被拒绝、**非空**夹具必须被计数。
///
/// 这两个用例只碰临时文件，不启动 server、不发任何请求，因此不改变 wire 行为与请求序列；
/// 作用是把"导出后自检"本身也纳入可执行验证，而不是只靠人工核对。
#[cfg(test)]
mod wire_log_export_selfcheck_tests {
    use super::verify_wire_log_export;
    use std::io::Write;

    #[test]
    fn empty_export_is_rejected() {
        let dir = tempfile::tempdir().expect("临时目录");
        let empty = dir.path().join("empty-wire.log");
        std::fs::File::create(&empty).expect("建空夹具");
        let reason = verify_wire_log_export(&empty).expect_err("空夹具必须被拒绝");
        assert!(
            reason.contains("0 条 wire 记录"),
            "原因应点明空夹具：{reason}"
        );

        let missing = dir.path().join("never-created.log");
        let reason = verify_wire_log_export(&missing).expect_err("不存在必须被拒绝");
        assert!(reason.contains("无法读回"), "原因应点明不可读：{reason}");
    }

    #[test]
    fn non_empty_export_is_counted() {
        let dir = tempfile::tempdir().expect("临时目录");
        let log = dir.path().join("wire.log");
        let mut file = std::fs::File::create(&log).expect("建夹具");
        writeln!(
            file,
            "=== REQUEST ===\nGET /mcp\n=== RESPONSE ===\nHTTP/1.1 200 OK"
        )
        .expect("写第一条");
        writeln!(file, "=== RECONNECT ===\n原因: 写前探测").expect("写重连条目");
        writeln!(file, "=== REQUEST ===\n第二组").expect("写第二条");
        assert_eq!(verify_wire_log_export(&log).expect("非空夹具必须通过"), 3);
    }
}
