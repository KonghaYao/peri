//! Builtin MCP 运行时（owner E-03）：同进程 duplex 链路、server task 归属、有界关闭。
//!
//! 职责边界（IF-D12 / sub-plan E §4.3）：
//! - 本模块**只**负责把 handler 交给 `rmcp::serve_server`、持有 server task，并给出
//!   有界关闭语义。handler 的业务语义（工具清单、`tools/list`、`call_tool` 的结果映射）
//!   归 I-01 的 `mcp/builtin/web.rs` / `artifact.rs`。
//! - builtin **恒为同进程对象**：不引入子进程、不读 env、不写磁盘、不接触任何凭据。
//! - 三分类超时（IF-D1）的事实源是 [`crate::mcp::transport::TransportKind`]；本模块不
//!   复制判定（映射在 `mcp/initialize.rs` 的 `connect_timeout` / `transport_label`）。
//! - 每个实例一条**独立** duplex 与一个**独立** task（隔离契约：不得共用 transport）。

use std::path::Path;

use rmcp::{serve_server, service::QuitReason, ServerHandler};
use thiserror::Error;
use tokio::{
    io::{DuplexStream, ReadHalf, WriteHalf},
    task::JoinHandle,
};

/// builtin duplex 通道容量（A16）。
///
/// **capacity 只影响背压，不是单帧上限**：单帧大小不受它约束。两侧读写都在各自 task
/// 中推进，缓冲写满时 `poll_write` 返回 `Pending` 反压调用方，而不是截断或拒绝某一帧。
/// 因此大正文（WebFetch 级，数十~数百 KB）可以完整往返，**不得**按帧大小调整本值。
pub(crate) const BUILTIN_DUPLEX_BUF: usize = 8 * 1024;

/// server task 有界收敛等待上界。
///
/// 关闭顺序（§4.3 第 4 条）：① client service `close_with_timeout` ② 有界等待 server
/// task 靠输入流 EOF 收敛 ③ 未收敛才 `abort` + `await`。`abort` **不是**正常路径。
pub(crate) const BUILTIN_CONVERGE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(1000);

/// builtin 链路装配失败（typed）。
///
/// 错误文本只含实例名与固定规则文本：不含路径、env、URL 认证信息或任何凭据（§9 规则 7）。
#[derive(Debug, Error)]
pub(crate) enum BuiltinSpawnError {
    /// 实例名不在注册表内（`builtin_mcp::find` 只命中已实现实例）。
    ///
    /// 文本与 `transport::TransportError::UnknownBuiltinInstance` 一致：同一事实只有一种
    /// 用户可见措辞。
    #[error("builtin MCP 实例未注册: {instance}")]
    UnknownInstance { instance: String },
    /// 已注册实例尚无 handler 模块（`builtin_server_handler` 未覆盖该实例）。
    ///
    /// 该状态**不**降级：不 panic、不静默改造为 stdio / http，也不产生任何 ready 证据。
    /// wave 1 的两个已实现实例（web / artifact）均已接线，因此本变体服务于「注册表新增
    /// 已实现实例、且 handler 尚未落地」的中间态（后续波次的 cron / lsp / workspace）。
    #[error("builtin 实例 handler 尚未接线: {instance}")]
    HandlerNotWired { instance: String },
}

/// server task 的退出事实。
///
/// 不把「未进入服务就退出」伪装成正常关闭：握手装配失败与优雅关闭是两件事。
#[derive(Debug)]
pub(crate) enum BuiltinServerExit {
    /// 已进入服务并收敛（正常路径：client 关闭 → 输入流 EOF → `QuitReason::Closed`）。
    Quit(QuitReason),
    /// 尚未进入服务即退出（`serve_server` 装配/握手失败），文本已由 rmcp 脱敏。
    NotStarted(String),
    /// 未在等待上界内收敛，已被 `abort`。
    AbortedAfterTimeout,
    /// task 本身失败（panic / 被取消以外的 join 错误）。
    TaskFailed(String),
}

/// 一个 builtin 实例的 server task 句柄（pool 侧 task 表的 value）。
pub(crate) struct BuiltinServerTask {
    instance: String,
    handle: JoinHandle<BuiltinServerExit>,
}

impl BuiltinServerTask {
    /// 所属实例名（server name / 配置 key）。
    pub(crate) fn instance(&self) -> &str {
        &self.instance
    }

    /// task 是否已结束——「无 orphan」的可观察证据。
    pub(crate) fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// 有界收敛：先等 `timeout`；**未收敛才** `abort` + `await`。
    ///
    /// 返回退出事实。`AbortedAfterTimeout` 表示等待上界内没有靠 EOF 自然收敛，
    /// 是异常信号（正常关闭必须落在 `Quit`），调用方据此告警。
    pub(crate) async fn converge(&mut self, timeout: std::time::Duration) -> BuiltinServerExit {
        match tokio::time::timeout(timeout, &mut self.handle).await {
            Ok(Ok(exit)) => exit,
            Ok(Err(error)) => BuiltinServerExit::TaskFailed(error.to_string()),
            Err(_) => {
                self.handle.abort();
                let _ = (&mut self.handle).await;
                BuiltinServerExit::AbortedAfterTimeout
            }
        }
    }
}

/// 一条已装配的 builtin 链路：client 侧 duplex 分半 + server task 归属。
pub(crate) struct BuiltinTransport {
    /// 交给 `serve_client_auto` 的 client 侧 transport（与 stdio / http 同参进入同一条
    /// 处理链：握手、发现、句柄构造、提交语义完全同构）。
    pub(crate) io: (ReadHalf<DuplexStream>, WriteHalf<DuplexStream>),
    /// server 侧 task。**必须**登记进 pool 的 builtin task 表，否则关闭/重连留 orphan。
    pub(crate) server_task: BuiltinServerTask,
}

/// 装配一条同进程链路：`tokio::io::duplex` 分半 + 真实 `rmcp::serve_server` 在 task 中运行。
///
/// `ServerHandler` 带 `Sized` 约束（`rmcp-3.1.4/src/handler/server.rs:593`），无法用
/// `Box<dyn ServerHandler>` 抹平类型差异，因此 handler 以泛型参数进入；实例名 → handler
/// 的解析见 [`spawn_builtin_transport`]。
///
/// **不覆写 `discover`**（spike Q1(b)/Q3(b)）：一旦 builtin handler 覆写 `discover` 返回
/// `method_not_found`，Auto 会在同一连接上回退 legacy，此后该连接上的 `tools/list` 恒被
/// `-32602` 拒绝（工具发现在生产链路上必然失败）。本模块不使用任何自定义 discover。
pub(crate) fn spawn_builtin_transport_with_handler<H>(
    instance: &str,
    handler: H,
) -> BuiltinTransport
where
    H: ServerHandler,
{
    spawn_builtin_link(instance, handler, |read| read)
}

/// 唯一的链路装配实现。`wrap_server_read` 只用于测试的线路观测（生产传恒等函数）。
fn spawn_builtin_link<R, H>(
    instance: &str,
    handler: H,
    wrap_server_read: impl FnOnce(ReadHalf<DuplexStream>) -> R,
) -> BuiltinTransport
where
    H: ServerHandler,
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    let (client_io, server_io) = tokio::io::duplex(BUILTIN_DUPLEX_BUF);
    let (server_read, server_write) = tokio::io::split(server_io);
    let server_read = wrap_server_read(server_read);
    let task_instance = instance.to_string();
    let handle = tokio::spawn(async move {
        match serve_server(handler, (server_read, server_write)).await {
            // `waiting()` 消费 running service；输入流 EOF / 对端关闭即返回 QuitReason。
            Ok(running) => match running.waiting().await {
                Ok(reason) => BuiltinServerExit::Quit(reason),
                Err(error) => BuiltinServerExit::TaskFailed(error.to_string()),
            },
            Err(error) => BuiltinServerExit::NotStarted(error.to_string()),
        }
    });
    BuiltinTransport {
        io: tokio::io::split(client_io),
        server_task: BuiltinServerTask {
            instance: task_instance,
            handle,
        },
    }
}

/// 实例名 → handler 解析 + 链路装配（`initialize_config` / `reconnect` 的唯一入口）。
///
/// 调用方必须先经 [`crate::mcp::transport::require_known_builtin_instance`] 做实例解析；
/// 本函数重复同一校验，使直接调用本 seam 的代码也不可能为未注册实例建立传输。
///
/// **handler 接线点**：实例名 → handler 的分派归 I-01 的
/// [`super::web::builtin_server_handler`]（名字到模块的分派是**代码**事实，模块不能由
/// 注册表数据构造）。本函数只负责把 handler 交给 rmcp 并持有 task；注册表里「已实现但
/// 尚无 handler 模块」的实例（后续波次的 cron / lsp / workspace）在这里以 typed
/// [`BuiltinSpawnError::HandlerNotWired`] 收口——**不** panic、**不**静默降级成
/// stdio / http、**不**伪造 ready 证据。`cwd` 是 artifact 实例的文件解析根（web 忽略）。
pub(crate) fn spawn_builtin_transport(
    instance: &str,
    cwd: &Path,
) -> Result<BuiltinTransport, BuiltinSpawnError> {
    crate::mcp::transport::require_known_builtin_instance(instance).map_err(|_| {
        BuiltinSpawnError::UnknownInstance {
            instance: instance.to_string(),
        }
    })?;
    let handler = super::web::builtin_server_handler(instance, cwd).ok_or_else(|| {
        BuiltinSpawnError::HandlerNotWired {
            instance: instance.to_string(),
        }
    })?;
    Ok(spawn_builtin_transport_with_handler(instance, handler))
}

/// 测试专用：与 [`spawn_builtin_transport_with_handler`] 同链路，额外在 server 读半挂
/// 线路观测（记录收到的 JSON-RPC method 序列）。
///
/// 用途（A13 子项 ④ 的观测面）：断言 namespace 路由不串（`mcp__web__*` 只落 web 的
/// handler），以及 modern 路径下线路**不含** `initialize` 帧——真实 `rmcp::serve_server`
/// 的默认 `discover` / `initialize` 没有插桩点，只能从线路取证（spike Q1(b)）。
#[cfg(test)]
pub(crate) fn spawn_builtin_transport_with_tap<H>(
    instance: &str,
    handler: H,
) -> (BuiltinTransport, std::sync::Arc<BuiltinWireLog>)
where
    H: ServerHandler,
{
    let log = std::sync::Arc::new(BuiltinWireLog::default());
    let transport = spawn_builtin_link(instance, handler, |read| {
        MethodTap::new(read, std::sync::Arc::clone(&log))
    });
    (transport, log)
}

/// 测试专用：server 侧读到的 JSON-RPC method 序列（线路级证据）。
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct BuiltinWireLog {
    methods: parking_lot::Mutex<Vec<String>>,
}

#[cfg(test)]
impl BuiltinWireLog {
    pub(crate) fn methods(&self) -> Vec<String> {
        self.methods.lock().clone()
    }

    fn record(&self, method: &str) {
        self.methods.lock().push(method.to_string());
    }
}

/// 测试专用 `AsyncRead` 透传包装：按行切分已读字节，记录每帧的 `method`。
#[cfg(test)]
struct MethodTap<R> {
    inner: R,
    log: std::sync::Arc<BuiltinWireLog>,
    pending: Vec<u8>,
}

#[cfg(test)]
impl<R> MethodTap<R> {
    fn new(inner: R, log: std::sync::Arc<BuiltinWireLog>) -> Self {
        Self {
            inner,
            log,
            pending: Vec::new(),
        }
    }

    fn observe(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=newline).collect();
            let Ok(message) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            if let Some(method) = message.get("method").and_then(|value| value.as_str()) {
                self.log.record(method);
            }
        }
    }
}

#[cfg(test)]
impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for MethodTap<R> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let poll = std::pin::Pin::new(&mut this.inner).poll_read(cx, buf);
        if buf.filled().len() > before {
            let observed = buf.filled()[before..].to_vec();
            this.observe(&observed);
        }
        poll
    }
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;
