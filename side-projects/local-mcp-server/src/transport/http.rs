//! Streamable HTTP 传输、暴露面控制与身份接线（WP-006 独占 scope）。
//!
//! 本模块只做**传输**：把真实的 TCP/HTTP 请求交给 rmcp 的 Streamable HTTP 服务，
//! 并在它之前完成认证与身份绑定、在它之后记录 legacy 会话绑定。它不实现任何工具语义、
//! 不构造 `ServerHandler`（由 WP-005 提供、WP-007 在 `main.rs` 装配）。
//!
//! 关键行为（对照 `artifacts/designs/WP-001/protocol-baseline.md` 的 P-12..P-17）：
//!
//! | 行为 | 实现位置 |
//! | --- | --- |
//! | 默认只绑回环、Host 白名单、Origin 可配置 | [`rmcp_server_config`] |
//! | `MCP-Protocol-Version` / `Mcp-Method` / `Mcp-Name` 与 body 一致性 → 400 + `-32020` | SDK（不绕过，见 [`serve_http`] 注释） |
//! | `Accept` / `Content-Type` 协商、body 上限 413、会话 404、方法 405 | SDK |
//! | bearer 认证 401、principal/连接实例绑定、跨主体会话拒绝 | [`crate::auth`] |
//! | modern 无状态（不依赖会话头）与 legacy 会话 | SDK + 本模块的绑定表 |
//! | 优雅关闭（会话取消 + 连接收尾） | [`HttpServer::shutdown`] |
//!
//! 不改变 SDK 语义的两条边界：
//! 1. `allowed_hosts` 为空时**不是**「允许全部 Host」：本模块映射为回环默认白名单
//!    （`localhost`/`127.0.0.1`/`::1` 加上实际 bind 的地址）。SDK 自身把空列表解释为
//!    「放行所有 Host」，那是本服务不接受的行为。
//! 2. `allowed_origins` 为空表示不校验 Origin（冻结契约语义，缺失 Origin 一律放行）；
//!    此时 DNS rebinding 防护由 Host 白名单承担，启动时给出显式警告。

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderValue, Request, Response, StatusCode};
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use rmcp::transport::common::http_header::HEADER_SESSION_ID;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::ServerHandler;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::auth::{
    install_principal, mint_client_instance, AuthError, AuthRejection, AuthenticatedPrincipal,
    Authenticator, SessionBinding, SessionBindings,
};
use crate::config::{Config, HttpConfig};
use crate::wire::ClientInstanceId;

/// `allowed_hosts` 为空时的回环默认白名单（防 DNS rebinding）。
pub const DEFAULT_LOOPBACK_ALLOWED_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// 关闭时等待连接收尾的上限。
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// 连接前言（首个请求头）扫描上限：超过即不再自建判定，原样交给 hyper。
///
/// hyper 自身的头缓冲上限（默认 400 KiB）远大于此值，因此这里只限制**自建判定**的
/// 成本上界，不改变 hyper 能接受的请求。
pub const MAX_PREFACE_BYTES: usize = 8 * 1024;

/// 收到首个请求头的第一个字节后，允许它读完的上限。
///
/// 只对"已经开始发头"的连接生效：空闲连接（一个字节都没发）不受影响，语义上等价于
/// hyper 默认的无头超时。
pub const PREFACE_HEAD_TIMEOUT: Duration = Duration::from_secs(5);

/// 传输层启动/运行错误；启动失败必须 fail closed（由 `main` 映射退出码）。
#[derive(Debug, thiserror::Error)]
pub enum HttpTransportError {
    /// 非回环绑定未被显式授权（配置校验的纵深防御，不应发生）。
    #[error("http transport: non-loopback bind is not authorized")]
    NonLoopbackBind,
    /// token 来源不可用：必须拒绝启动，绝不降级为无认证。
    #[error("http transport: {0}")]
    Auth(#[from] AuthError),
    /// 监听失败。
    #[error("http transport: cannot bind {addr}: {source}")]
    Bind {
        /// 目标地址。
        addr: SocketAddr,
        /// 底层 IO 错误。
        source: io::Error,
    },
    /// Process signal handlers could not be registered.
    #[error("http transport: signal setup failed: {0}")]
    Signal(#[source] io::Error),
}

/// 把冻结的 [`HttpConfig`] 映射为 SDK 传输配置。
///
/// 这里**只做映射**，不引入新语义；唯一的纠偏是空 `allowed_hosts` 必须变成回环白名单，
/// 而不是 SDK 默认的「放行所有 Host」。
pub fn rmcp_server_config(
    config: &HttpConfig,
    cancellation_token: CancellationToken,
) -> StreamableHttpServerConfig {
    StreamableHttpServerConfig::default()
        .with_allowed_hosts(effective_allowed_hosts(config))
        .with_allowed_origins(config.allowed_origins.clone())
        .with_max_request_body_bytes(config.max_request_body_bytes)
        // legacy 会话保留（P-17）：modern 请求由 SDK 恒为无状态，不受本开关影响。
        .with_legacy_session_mode(true)
        // 保留 SDK 默认：成功响应走请求级 SSE；错误走 JSON 并携带协议状态码。
        .with_json_response(false)
        .with_cancellation_token(cancellation_token)
}

/// 计算生效的 Host 白名单。
fn effective_allowed_hosts(config: &HttpConfig) -> Vec<String> {
    if !config.allowed_hosts.is_empty() {
        return config.allowed_hosts.clone();
    }
    let mut hosts: Vec<String> = DEFAULT_LOOPBACK_ALLOWED_HOSTS
        .iter()
        .map(|host| (*host).to_string())
        .collect();
    let bind_ip = config.bind.ip().to_string();
    if !hosts.contains(&bind_ip) {
        hosts.push(bind_ip);
    }
    hosts
}

/// HTTP 传输的运行期安全状态（认证器 + 会话绑定表）。
#[derive(Debug)]
struct HttpSecurityState {
    authenticator: Authenticator,
    bindings: SessionBindings,
}

/// 已启动的 HTTP 传输。
pub struct HttpServer {
    local_addr: SocketAddr,
    shutdown: CancellationToken,
    tracker: TaskTracker,
    accept_task: tokio::task::JoinHandle<()>,
}

impl HttpServer {
    /// 实际监听地址（`bind` 端口为 0 时为内核分配的端口）。
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 关闭信号（供 `main.rs` 的信号处理层复用同一 token）。
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Wait for an external cancellation or a process termination signal.
    pub async fn wait_for_shutdown(&self) -> Result<(), HttpTransportError> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};

            let mut sigterm =
                signal(SignalKind::terminate()).map_err(HttpTransportError::Signal)?;
            tokio::select! {
                _ = self.shutdown.cancelled() => {}
                result = tokio::signal::ctrl_c() => {
                    result.map_err(HttpTransportError::Signal)?;
                    tracing::info!("收到中断信号，开始优雅关闭");
                }
                _ = sigterm.recv() => {
                    tracing::info!("收到终止信号，开始优雅关闭");
                }
            }
        }
        #[cfg(not(unix))]
        {
            tokio::select! {
                _ = self.shutdown.cancelled() => {}
                result = tokio::signal::ctrl_c() => {
                    result.map_err(HttpTransportError::Signal)?;
                    tracing::info!("收到中断信号，开始优雅关闭");
                }
            }
        }
        Ok(())
    }

    /// 优雅关闭：取消会话与在途请求 → 停止接受新连接 → 等待连接收尾。
    pub async fn shutdown(self) {
        self.shutdown.cancel();
        self.tracker.close();
        if tokio::time::timeout(SHUTDOWN_GRACE, self.tracker.wait())
            .await
            .is_err()
        {
            tracing::warn!(
                grace_ms = SHUTDOWN_GRACE.as_millis() as u64,
                "HTTP 连接在宽限期内未全部收尾"
            );
        }
        let _ = self.accept_task.await;
    }
}

/// 启动 Streamable HTTP 传输。
///
/// `service_factory` 是 MCP core 的构造工厂（WP-005 的 `ServerHandler` 实现，每个
/// 会话/无状态请求各构造一次，与 SDK 语义一致）。本函数：
///
/// 1. 校验回环绑定（非回环必须显式授权，且配置必须已有 token 来源）；
/// 2. 从 [`crate::config::AuthConfig`] 的**来源**加载 token，失败即拒绝启动；
/// 3. 绑定 TCP 端口并逐连接生成不可猜的连接实例 id；
/// 4. 每个请求：认证 → 会话归属校验 → 注入可信主体 → 交给 SDK → 记录会话绑定。
///
/// 不绕过 SDK 的 modern `_meta`、`MCP-Protocol-Version`、`Mcp-Method` / `Mcp-Name` /
/// `Mcp-Param-*` 与 body 一致性校验：这些检查发生在 `StreamableHttpService::handle`
/// 内部，任何绕过都会破坏 `-32020` 证据链。
pub async fn serve_http<S, F>(
    config: &Config,
    service_factory: F,
) -> Result<HttpServer, HttpTransportError>
where
    S: ServerHandler + Send + 'static,
    F: Fn() -> Result<S, io::Error> + Send + Sync + 'static,
{
    if !config.bind_is_loopback() && !config.allow_non_loopback {
        return Err(HttpTransportError::NonLoopbackBind);
    }

    let authenticator = Authenticator::from_config(&config.auth)?;
    if config.http.allowed_origins.is_empty() {
        tracing::warn!(
            "allowed_origins 为空：Origin 头不参与校验（冻结契约语义）；\
             DNS rebinding 防护由 Host 回环白名单承担"
        );
    }

    let shutdown = CancellationToken::new();
    let rmcp_config = rmcp_server_config(&config.http, shutdown.child_token());
    let effective_hosts = effective_allowed_hosts(&config.http);
    let session_manager = Arc::new(LocalSessionManager::default());
    let rmcp_service: StreamableHttpService<S, LocalSessionManager> =
        StreamableHttpService::new(service_factory, session_manager, rmcp_config);

    let listener = TcpListener::bind(config.http.bind)
        .await
        .map_err(|source| HttpTransportError::Bind {
            addr: config.http.bind,
            source,
        })?;
    let local_addr = listener
        .local_addr()
        .map_err(|source| HttpTransportError::Bind {
            addr: config.http.bind,
            source,
        })?;

    tracing::info!(
        addr = %local_addr,
        loopback = config.bind_is_loopback(),
        hosts = ?effective_hosts,
        origins = ?config.http.allowed_origins,
        max_body_bytes = config.http.max_request_body_bytes,
        auth = authenticator.method().as_str(),
        "Streamable HTTP 传输已启动"
    );

    let state = Arc::new(HttpSecurityState {
        authenticator,
        bindings: SessionBindings::new(),
    });
    let tracker = TaskTracker::new();
    let accept_task = tokio::spawn(accept_loop(
        listener,
        rmcp_service,
        state,
        shutdown.clone(),
        tracker.clone(),
    ));

    Ok(HttpServer {
        local_addr,
        shutdown,
        tracker,
        accept_task,
    })
}

/// 接受连接循环：每条连接一个不可猜实例 id 与一个优雅关闭子 token。
async fn accept_loop<S>(
    listener: TcpListener,
    rmcp_service: StreamableHttpService<S, LocalSessionManager>,
    state: Arc<HttpSecurityState>,
    shutdown: CancellationToken,
    tracker: TaskTracker,
) where
    S: ServerHandler + Send + 'static,
{
    loop {
        let accepted = tokio::select! {
            _ = shutdown.cancelled() => break,
            accepted = listener.accept() => accepted,
        };

        match accepted {
            Ok((stream, peer)) => {
                let _ = stream.set_nodelay(true);
                let connection_instance = mint_client_instance();
                let connection_shutdown = shutdown.child_token();
                let rmcp_service = rmcp_service.clone();
                let state = state.clone();
                tracker.spawn(async move {
                    serve_connection(
                        stream,
                        peer,
                        rmcp_service,
                        state,
                        connection_instance,
                        connection_shutdown,
                    )
                    .await;
                });
            }
            Err(error) => {
                tracing::warn!(error = %error, "accept 失败，继续监听");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

/// 单条 TCP 连接的 HTTP/1.1 服务。
///
/// 在交给 hyper 之前先做一次**连接前言判定**（CL 与 TE 并存 → 400）：hyper 的 http1
/// 解析器遇到这两种头并存会丢弃 `Content-Length`（hyper 1.11.0
/// `proto/h1/role.rs` 的 `parse_headers`），服务层看到的头里只剩 TE——冲突在服务层
/// 不可观察（`tests/transport_http.rs` 的 framing 用例记录了这个前后事实）。
async fn serve_connection<S>(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    rmcp_service: StreamableHttpService<S, LocalSessionManager>,
    state: Arc<HttpSecurityState>,
    connection_instance: ClientInstanceId,
    shutdown: CancellationToken,
) where
    S: ServerHandler + Send + 'static,
{
    let mut stream = stream;
    let preface = match scan_preface(&mut stream, &shutdown).await {
        Ok(preface) => preface,
        // 读失败（对端复位/半途关闭）：连接已不可用，与 hyper 的读错误同族处理。
        Err(error) => {
            tracing::debug!(%peer, error = %error, "连接前言读取失败");
            return;
        }
    };
    if let Preface::ConflictingFraming = preface {
        tracing::warn!(%peer, "拒绝同时声明 Content-Length 与 Transfer-Encoding 的请求");
        let body = "Bad Request: Content-Length and Transfer-Encoding must not be combined";
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\n\
             content-type: text/plain; charset=utf-8\r\n\
             content-length: {}\r\n\
             connection: close\r\n\
             \r\n{body}",
            body.len()
        );
        {
            use tokio::io::AsyncWriteExt;
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
        return;
    }
    let prefix = match preface {
        Preface::PassThrough(prefix) => prefix,
        Preface::ConflictingFraming => unreachable!("上面已经返回"),
    };

    let io = TokioIo::new(PrefixedStream::new(prefix, stream));
    let service = hyper::service::service_fn(move |request: Request<Incoming>| {
        let rmcp_service = rmcp_service.clone();
        let state = state.clone();
        let connection_instance = connection_instance.clone();
        async move {
            Ok::<_, Infallible>(
                handle_http_request(rmcp_service, state, connection_instance, request).await,
            )
        }
    });

    let connection = http1::Builder::new().serve_connection(io, service);
    tokio::pin!(connection);

    let mut shutdown_requested = false;
    let result = loop {
        tokio::select! {
            result = connection.as_mut() => break result,
            _ = shutdown.cancelled(), if !shutdown_requested => {
                shutdown_requested = true;
                connection.as_mut().graceful_shutdown();
            }
        }
    };

    match result {
        Ok(()) => tracing::debug!(%peer, "HTTP 连接关闭"),
        Err(error) => tracing::debug!(%peer, error = %error, "HTTP 连接异常结束"),
    }
}

/// 连接前言判定的结果。
#[derive(Debug, PartialEq, Eq)]
enum Preface {
    /// 首个请求头同时声明 `Content-Length` 与 `Transfer-Encoding`：显式拒绝。
    ConflictingFraming,
    /// 其余情形：把已读到的字节**原样**交回 hyper（语义与不扫描时一致）。
    PassThrough(Vec<u8>),
}

/// 在 hyper 解析之前读取并判定**首个**请求头。
///
/// 判定规则：请求头里同时出现 `Content-Length` 与 `Transfer-Encoding` 两个头字段
/// （大小写不敏感，只看完整成行到 `CRLF` 的头行）即 [`Preface::ConflictingFraming`]。
/// RFC 9112 §6.1 要求这类消息按错误处理，而不是替调用方选择一种解释。
///
/// 覆盖边界（如实声明，不声称完整）：
/// - 只判定每条连接的**首个**请求头；同一连接上的后续请求由 hyper 归一（丢弃 CL、
///   按 TE 解码），本层不可见；
/// - 请求头超过 [`MAX_PREFACE_BYTES`] 时不再自建判定，原样交给 hyper；
/// - 首个字节到达后头必须在 [`PREFACE_HEAD_TIMEOUT`] 内读完，否则按异常连接关闭
///   （fail closed，不把未判定的连接交给上层）；一个字节都没发的空闲连接不受影响，
///   由 hyper 按既有语义处理。
async fn scan_preface(
    stream: &mut tokio::net::TcpStream,
    shutdown: &CancellationToken,
) -> io::Result<Preface> {
    let mut buffer: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(head_end) = find_head_end(&buffer) {
            let head = &buffer[..head_end];
            return Ok(if has_conflicting_framing_in_head(head) {
                Preface::ConflictingFraming
            } else {
                Preface::PassThrough(buffer)
            });
        }
        if buffer.len() >= MAX_PREFACE_BYTES {
            // 超长头：不做自建判定，原样交给 hyper（含已读字节）。
            return Ok(Preface::PassThrough(buffer));
        }
        let read = tokio::select! {
            _ = shutdown.cancelled() => return Ok(Preface::PassThrough(buffer)),
            read = read_next(stream, &mut chunk, buffer.is_empty()) => read?,
        };
        if read == 0 {
            // 对端在头未读完时关闭/半关闭：把已有字节交回，由 hyper 按既有语义收尾。
            return Ok(Preface::PassThrough(buffer));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// 读一段：空闲连接（一个字节都没收到）不设超时；已开始发头则受
/// [`PREFACE_HEAD_TIMEOUT`] 约束（超时即 fail closed）。
async fn read_next(
    stream: &mut tokio::net::TcpStream,
    chunk: &mut [u8],
    idle: bool,
) -> io::Result<usize> {
    use tokio::io::AsyncReadExt;

    if idle {
        return stream.read(chunk).await;
    }
    match tokio::time::timeout(PREFACE_HEAD_TIMEOUT, stream.read(chunk)).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "request head did not complete within the preface timeout",
        )),
    }
}

/// 请求头结束位置（`\r\n\r\n` 之后的下标）；未结束返回 `None`。
fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

/// 请求头（不含 body）里是否同时出现 `Content-Length` 与 `Transfer-Encoding`。
fn has_conflicting_framing_in_head(head: &[u8]) -> bool {
    let mut content_length = false;
    let mut transfer_encoding = false;
    for line in head.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            continue; // 请求行或空行
        };
        let name = &line[..colon];
        if name.eq_ignore_ascii_case(b"content-length") {
            content_length = true;
        } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
            transfer_encoding = true;
        }
    }
    content_length && transfer_encoding
}

/// 「已读字节 + 底层流」的适配器：把前言扫描读到的字节原样还给 hyper。
struct PrefixedStream<S> {
    prefix: Vec<u8>,
    consumed: usize,
    inner: S,
}

impl<S> PrefixedStream<S> {
    fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self {
            prefix,
            consumed: 0,
            inner,
        }
    }
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for PrefixedStream<S> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.consumed < this.prefix.len() {
            let remaining = &this.prefix[this.consumed..];
            let take = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..take]);
            this.consumed += take;
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for PrefixedStream<S> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        data: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        let this = self.get_mut();
        std::pin::Pin::new(&mut this.inner).poll_write(cx, data)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        let this = self.get_mut();
        std::pin::Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        let this = self.get_mut();
        std::pin::Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

/// 传输层单请求处理：认证 → 会话归属 → 注入主体 → SDK → 记录会话绑定。
async fn handle_http_request<S>(
    rmcp_service: StreamableHttpService<S, LocalSessionManager>,
    state: Arc<HttpSecurityState>,
    connection_instance: ClientInstanceId,
    mut request: Request<Incoming>,
) -> Response<BoxBody<Bytes, Infallible>>
where
    S: ServerHandler + Send + 'static,
{
    let method = request.method().clone();
    let submitted_session = request
        .headers()
        .get(HEADER_SESSION_ID)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    // 1. 认证：唯一可信身份来源。
    let principal = match state
        .authenticator
        .authenticate(request.headers(), &connection_instance)
    {
        Ok(principal) => principal,
        Err(rejection) => {
            tracing::warn!(reason = rejection.reason(), "拒绝未通过认证的请求");
            return unauthorized_response(rejection);
        }
    };

    // 2. legacy 会话归属：跨主体一律按「会话不存在」处理，不泄露存在性。
    let mut effective_instance = principal.client_instance().to_string();
    if let Some(session_id) = submitted_session.as_deref() {
        match decide_session_use(
            principal.principal(),
            state.bindings.resolve(session_id).as_ref(),
        ) {
            SessionDecision::UseBoundInstance(bound) => {
                // 同一主体的会话：实例固定为创建会话时的实例（legacy session owner）。
                effective_instance = bound;
            }
            SessionDecision::UseConnectionInstance => {}
            SessionDecision::RejectForeignPrincipal => {
                tracing::warn!(principal = %principal.principal(), "拒绝跨主体使用会话 id");
                return session_not_found_response();
            }
        }
    }
    let principal: AuthenticatedPrincipal =
        principal.with_client_instance(effective_instance.clone());
    install_principal(request.extensions_mut(), principal.clone());

    // 3. 交给 SDK：modern `_meta`、header/body 一致性、body 上限、会话路由都在其中。
    let response = rmcp_service.handle(request).await;

    // 4. 会话绑定/解绑：只记录，不改变 SDK 的路由结果。
    if let Some(session_id) = response
        .headers()
        .get(HEADER_SESSION_ID)
        .and_then(|value| value.to_str().ok())
    {
        state
            .bindings
            .bind(session_id, principal.principal(), &effective_instance);
    } else if method == http::Method::DELETE {
        if let Some(session_id) = submitted_session.as_deref() {
            if response.status().is_success() {
                state.bindings.unbind(session_id);
            }
        }
    }

    response
}

/// legacy 会话使用决策。
///
/// 抽成纯函数是为了让"跨主体拒绝"这条分支可被独立测试：当前配置模型只有单个 token
/// （单主体），因此该分支在真实 wire 上不可达；把它显式化并覆盖单测，比留一段
/// 无法触达的条件更诚实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionDecision {
    /// 同一主体的既有会话：使用会话绑定的实例。
    UseBoundInstance(String),
    /// 无绑定或会话未知：使用本次连接的实例（未知会话稍后由 SDK 回 404）。
    UseConnectionInstance,
    /// 绑定属于其他主体：按会话不存在处理（不泄露存在性）。
    RejectForeignPrincipal,
}

/// 依据当前主体与既有绑定决定会话归属。
pub fn decide_session_use(
    current_principal: &str,
    binding: Option<&SessionBinding>,
) -> SessionDecision {
    match binding {
        Some(binding) if binding.principal == current_principal => {
            SessionDecision::UseBoundInstance(binding.client_instance.clone())
        }
        Some(_) => SessionDecision::RejectForeignPrincipal,
        None => SessionDecision::UseConnectionInstance,
    }
}

/// 401：不区分「缺失」与「不匹配」，不回显提交内容。
fn unauthorized_response(rejection: AuthRejection) -> Response<BoxBody<Bytes, Infallible>> {
    let mut response = text_response(rejection.status(), rejection.public_message());
    if let Ok(value) = HeaderValue::from_str(rejection.www_authenticate()) {
        response
            .headers_mut()
            .insert(http::header::WWW_AUTHENTICATE, value);
    }
    response
}

/// 404：与 SDK 的未知会话响应逐字一致，避免暴露会话归属。
fn session_not_found_response() -> Response<BoxBody<Bytes, Infallible>> {
    text_response(StatusCode::NOT_FOUND, "Not Found: Session not found")
}

/// 纯文本响应（与 SDK 传输层错误响应形态一致）。
fn text_response(status: StatusCode, message: &str) -> Response<BoxBody<Bytes, Infallible>> {
    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from(message.to_string())).boxed())
        .expect("static transport response is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_config(hosts: &[&str], origins: &[&str]) -> HttpConfig {
        HttpConfig {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            allowed_hosts: hosts.iter().map(|host| (*host).to_string()).collect(),
            allowed_origins: origins.iter().map(|origin| (*origin).to_string()).collect(),
            max_request_body_bytes: 4096,
        }
    }

    #[test]
    fn empty_allowed_hosts_become_loopback_allowlist_not_allow_all() {
        let hosts = effective_allowed_hosts(&http_config(&[], &[]));
        assert!(hosts.contains(&"localhost".to_string()));
        assert!(hosts.contains(&"127.0.0.1".to_string()));
        assert!(hosts.contains(&"::1".to_string()));
        assert!(hosts.contains(&"127.0.0.1".to_string()));
        assert_eq!(hosts.len(), 3, "bind ip 已在默认白名单内，不应重复");
    }

    #[test]
    fn explicit_allowed_hosts_are_not_augmented() {
        let hosts = effective_allowed_hosts(&http_config(&["example.test:8443"], &[]));
        assert_eq!(hosts, vec!["example.test:8443".to_string()]);
    }

    #[test]
    fn custom_loopback_bind_ip_is_added_to_default_allowlist() {
        let mut config = http_config(&[], &[]);
        config.bind = SocketAddr::from(([127, 0, 0, 2], 0));
        let hosts = effective_allowed_hosts(&config);
        assert!(hosts.contains(&"127.0.0.2".to_string()));
    }

    #[test]
    fn rmcp_config_maps_frozen_fields() {
        let http = http_config(&["localhost"], &["https://allowed.test"]);
        let token = CancellationToken::new();
        let config = rmcp_server_config(&http, token.clone());
        assert_eq!(config.allowed_hosts, vec!["localhost".to_string()]);
        assert_eq!(
            config.allowed_origins,
            vec!["https://allowed.test".to_string()]
        );
        assert_eq!(config.max_request_body_bytes, 4096);
        assert!(config.legacy_session_mode, "legacy 会话模式保持开启");
        assert!(!config.json_response, "成功响应保持请求级 SSE");
        assert!(!config.stateless_protocol_metadata_required);
        assert!(!config.cancellation_token.is_cancelled());
        token.cancel();
        assert!(
            config.cancellation_token.is_cancelled(),
            "关闭信号必须传递到 SDK"
        );
    }

    fn binding(principal: &str, instance: &str) -> SessionBinding {
        SessionBinding {
            principal: principal.to_string(),
            client_instance: instance.to_string(),
        }
    }

    #[test]
    fn session_decision_uses_bound_instance_for_same_principal() {
        let bound = binding("bearer-a", "http-conn-1");
        assert_eq!(
            decide_session_use("bearer-a", Some(&bound)),
            SessionDecision::UseBoundInstance("http-conn-1".to_string())
        );
    }

    #[test]
    fn session_decision_rejects_foreign_principal() {
        let bound = binding("bearer-a", "http-conn-1");
        assert_eq!(
            decide_session_use("bearer-b", Some(&bound)),
            SessionDecision::RejectForeignPrincipal
        );
    }

    #[test]
    fn session_decision_falls_back_to_connection_for_unknown_session() {
        assert_eq!(
            decide_session_use("bearer-a", None),
            SessionDecision::UseConnectionInstance
        );
    }
}
