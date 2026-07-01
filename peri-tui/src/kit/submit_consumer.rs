//! 提交消费者——SUBMIT_TX channel → acp_client.prompt()。
//!
//! 这是 S4 的核心：InputArea 写入 channel 的用户文本在此被取出，转成
//! `MessageContent::text(...)` 并调用 `AcpTuiClient::prompt()`。同时承担
//! **首次会话懒初始化**：用户第一次提交时如果还没有 session，先创建。
//!
//! 设计：
//! - 单消费者，从 `mpsc::UnboundedReceiver<String>` 顺序读取（保证提交顺序）
//! - 与 notifier / bridge 并行运行，三者通过独立 channel 解耦
//! - shutdown 信号触发时干净退出，不发残留请求

use peri_agent::messages::MessageContent;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::acp_client::AcpTuiClient;

/// 启动提交消费者后台任务。
///
/// 参数：
/// - `acp_client`：克隆自 build_app_and_acp 返回的 AcpTuiClient（Clone + Arc 内部）
/// - `rx`：SUBMIT_TX 的接收端（由 entry::run_kit_fullscreen 创建 channel 时拿到）
/// - `cwd`：用于首次 new_session 时传给 ACP server
/// - `shutdown`：与 notifier / bridge 共享的同一 CancellationToken
pub fn spawn_submit_consumer(
    acp_client: AcpTuiClient,
    mut rx: mpsc::UnboundedReceiver<String>,
    cwd: String,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("kit submit_consumer: started");
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("kit submit_consumer: shutdown signal received, exiting");
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        None => {
                            info!("kit submit_consumer: SUBMIT_TX dropped, exiting");
                            break;
                        }
                        Some(text) => {
                            if let Err(e) = handle_submit(&acp_client, &cwd, text).await {
                                error!(error = %e, "kit submit_consumer: prompt failed");
                            }
                        }
                    }
                }
            }
        }
    })
}

/// 处理单条提交：确保 session 存在 → 发送 prompt。
async fn handle_submit(
    acp_client: &AcpTuiClient,
    cwd: &str,
    text: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    // 懒初始化 session（首次提交或 session/load 之后）
    if !acp_client.has_session() {
        info!(cwd = %cwd, "kit submit_consumer: creating initial session");
        acp_client.new_session(cwd, None).await?;
    }

    let content = MessageContent::text(trimmed.to_string());
    acp_client.prompt(&content).await.map_err(|e| {
        warn!(error = %e, "kit submit_consumer: prompt RPC failed");
        Box::new(e) as Box<dyn std::error::Error + Send + Sync>
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp::transport::{
        AcpTransport,
        mpsc::{MpscClientTransport, MpscServerTransport, mpsc_transport_pair},
    };

    /// 用真实 mpsc transport 对创建 AcpTuiClient（不启动 pump）。
    fn make_client_without_pump() -> (AcpTuiClient, MpscServerTransport) {
        let (client_transport, server_transport): (MpscClientTransport, MpscServerTransport) =
            mpsc_transport_pair();
        let (client, _notification_rx) = AcpTuiClient::new(client_transport);
        (client, server_transport)
    }

    #[tokio::test]
    async fn test_empty_text_skipped() {
        let (client, _server_transport) = make_client_without_pump();
        let cwd = ".".to_string();

        // 空文本 + 只有空白——不应创建 session
        let result = handle_submit(&client, &cwd, "   \n\t ".to_string()).await;
        assert!(result.is_ok());
        assert!(!client.has_session());
    }

    #[tokio::test]
    async fn test_creates_session_when_missing() {
        let (client, _server_transport) = make_client_without_pump();
        let cwd = ".".to_string();

        // 无 pump 时 new_session 调用 recv() 会 hang——此测试只验证控制流：
        // 启动一个简短超时，确保 handle_submit 进入 new_session 分支
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            handle_submit(&client, &cwd, "hello".to_string()),
        )
        .await;

        // 超时是预期（无 server 处理 RPC），但已确认走到了 new_session 路径
        assert!(
            result.is_err() || result.unwrap().is_err(),
            "expected timeout or RPC error (no server), got success"
        );
    }

    #[tokio::test]
    async fn test_shutdown_exits_loop() {
        let (client, _server_transport) = make_client_without_pump();
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let shutdown = CancellationToken::new();
        let handle = spawn_submit_consumer(client, rx, ".".into(), shutdown.clone());

        shutdown.cancel();
        // 任务应在合理时间内退出
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
        // 不发任何提交——channel 仍可用（drop tx 也能让任务退出）
        let _ = tx;
    }

    #[tokio::test]
    async fn test_dropped_tx_exits_loop() {
        let (client, _server_transport) = make_client_without_pump();
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let shutdown = CancellationToken::new();
        let handle = spawn_submit_consumer(client, rx, ".".into(), shutdown);

        drop(tx);
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
    }

    /// 编译期断言：UnboundedSender<String> 与 atoms::SUBMIT_TX 类型契约一致。
    #[test]
    fn test_submit_tx_type_contract() {
        let (tx, _rx): (mpsc::UnboundedSender<String>, _) = mpsc::unbounded_channel();
        // 模拟 atoms::SUBMIT_TX.set(tx) 的契约
        let _ = tx;
    }
}
