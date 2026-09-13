//! stdio 传输（WP-005）：stdout 只承载 NDJSON-RPC 消息。
//!
//! ## wire 约束（P-09/P-10）
//!
//! - 帧格式：**一行一个 JSON-RPC 消息**，`\n` 结尾（新版行分隔，无 `Content-Length`）。
//! - stdout **只允许**合法 MCP 消息：任何 `println!`/`eprintln!` 都会污染协议通道。
//!   诊断一律走 stderr（`main.rs` 已把 tracing 绑定到 stderr）。
//! - stdin 关闭（EOF）即优雅结束：本函数在传输关闭后返回，由调用方决定退出码；
//!   后台 `Bash` 任务与其它执行面状态由任务注册表持有，随 `main.rs` 的关闭流程回收
//!   （传输不拥有、也不清理执行面）。
//!
//! 传输层刻意保持"薄"：它只负责选帧与关闭语义，协议行为全部在
//! [`crate::mcp::server::SandboxServer`]，与 Streamable HTTP 共用同一个 handler
//! （R-006 的"两种传输共用同一 MCP core"）。

use rmcp::service::{serve_server_with_ct, QuitReason, ServerInitializeError};
use rmcp::transport::io::stdio;
use tokio_util::sync::CancellationToken;

use crate::mcp::server::SandboxServer;

/// stdio 服务生命周期错误。
#[derive(Debug, thiserror::Error)]
pub enum StdioServeError {
    /// 首包不是 `initialize`，也不是带齐必填 `_meta` 的 modern 请求（或传输在握手期关闭）。
    ///
    /// 装箱：`ServerInitializeError` 体积很大，直接作为 variant 会让整个 `Result` 变大。
    #[error("stdio 初始化失败：{0}")]
    Initialize(Box<ServerInitializeError>),
    /// 服务任务异常结束（panic/取消）。
    #[error("stdio 服务任务异常结束：{0}")]
    Service(#[from] tokio::task::JoinError),
    /// Process signal handlers could not be registered.
    #[error("stdio signal setup failed: {0}")]
    Signal(#[source] std::io::Error),
}

/// 用真实 stdin/stdout 提供 MCP 服务，直到 stdin EOF 或传输关闭。
///
/// 返回 [`QuitReason`] 后调用方应正常退出（EOF 是客户端的正常关闭动作，不是错误）。
pub async fn serve_stdio(server: SandboxServer) -> Result<QuitReason, StdioServeError> {
    let shutdown = CancellationToken::new();
    let running = serve_server_with_ct(server, stdio(), shutdown.clone())
        .await
        .map_err(|error| StdioServeError::Initialize(Box::new(error)))?;
    let running_cancel = running.cancellation_token();
    let waiter = tokio::spawn(async move { running.waiting().await });
    tokio::pin!(waiter);
    tokio::select! {
        result = &mut waiter => Ok(result??),
        signal = wait_for_termination_signal() => {
            signal.map_err(StdioServeError::Signal)?;
            running_cancel.cancel();
            Ok((&mut waiter).await??)
        }
    }
}

/// 同 [`serve_stdio`]，但接受外部关闭信号（`main.rs` 的信号处理或集成测试用）。
pub async fn serve_stdio_with_shutdown(
    server: SandboxServer,
    shutdown: CancellationToken,
) -> Result<QuitReason, StdioServeError> {
    let running = serve_server_with_ct(server, stdio(), shutdown.clone())
        .await
        .map_err(|error| StdioServeError::Initialize(Box::new(error)))?;
    let running_cancel = running.cancellation_token();
    let waiter = tokio::spawn(async move { running.waiting().await });
    tokio::pin!(waiter);
    tokio::select! {
        result = &mut waiter => Ok(result??),
        _ = shutdown.cancelled() => {
            running_cancel.cancel();
            Ok((&mut waiter).await??)
        },
    }
}

/// Wait for a process termination signal, reporting handler setup failures.
async fn wait_for_termination_signal() -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm = signal(SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = async {
                sigterm.recv().await;
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
    }
    Ok(())
}
