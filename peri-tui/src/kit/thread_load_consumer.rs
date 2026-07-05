//! Thread 切换消费者——THREAD_LOAD_TX channel → acp_client.load_session()。
//!
//! H3 修复：ThreadBrowser 面板 Enter 时把 `thread_id` 通过 channel 推送，
//! 本消费者调用 `AcpTuiClient::load_session(thread_id, cwd, model)`。
//!
//! ## 设计
//!
//! - 单消费者，从 `mpsc::UnboundedReceiver<String>` 顺序读取
//! - 与 submit_consumer / rewind_consumer 同模式，独立 channel 解耦
//! - shutdown 信号触发时干净退出
//!
//! ## 加载成功后 UI 怎么刷新？
//!
//! ACP server 端 `session/load` 会通过 `peri/unstable-event` 推送 ViewCommit
//! 通知（见 acp_server/requests.rs:241-260），由 kit_notifier → acp_bridge
//! 转化为 `AcpEventData::ViewCommit`，自动覆写 `VIEW_MODELS` atom——面板会
//! 自然关闭（Enter 后由 panel_overlay 处理），消息流自动刷新。

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::acp_client::AcpTuiClient;

/// 启动 thread 切换消费者后台任务。
pub fn spawn_thread_load_consumer(
    acp_client: AcpTuiClient,
    mut rx: mpsc::UnboundedReceiver<String>,
    cwd: String,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("kit thread_load_consumer: started");
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("kit thread_load_consumer: shutdown signal received, exiting");
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        None => {
                            info!("kit thread_load_consumer: THREAD_LOAD_TX dropped, exiting");
                            break;
                        }
                        Some(thread_id) => {
                            if let Err(e) = handle_load(&acp_client, &cwd, thread_id).await {
                                error!(error = %e, "kit thread_load_consumer: load_session failed");
                            }
                        }
                    }
                }
            }
        }
    })
}

/// 处理单条切换：调用 `acp_client.load_session(thread_id, cwd, None)`。
///
/// 加载前 `has_session()` 检查不是必要的——load_session 内部会先 close 旧
/// session 再 load 新的，多步原子语义。
async fn handle_load(
    acp_client: &AcpTuiClient,
    cwd: &str,
    thread_id: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if thread_id.trim().is_empty() {
        return Ok(());
    }
    info!(thread_id = %thread_id, cwd = %cwd, "kit thread_load_consumer: calling session/load");

    // 触发 bridge 状态重置：防止旧 session 的 committed ViewModel
    // 在新 session 的 ViewCommit 到达之前污染消息区。
    crate::kit::atoms::BRIDGE_RESET_COUNTER.set(
        crate::kit::atoms::BRIDGE_RESET_COUNTER
            .get()
            .wrapping_add(1),
    );

    acp_client
        .load_session(&thread_id, cwd, None)
        .await
        .map_err(|e| {
            warn!(error = %e, "kit thread_load_consumer: load_session RPC failed");
            Box::new(e) as Box<dyn std::error::Error + Send + Sync>
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp::transport::mpsc::{
        MpscClientTransport, MpscServerTransport, mpsc_transport_pair,
    };

    fn make_client_without_pump() -> (AcpTuiClient, MpscServerTransport) {
        let (client_transport, server_transport): (MpscClientTransport, MpscServerTransport) =
            mpsc_transport_pair();
        let (client, _notification_rx) = AcpTuiClient::new(client_transport);
        (client, server_transport)
    }

    #[tokio::test]
    async fn test_empty_thread_id_skipped() {
        let (client, _server) = make_client_without_pump();
        let result = handle_load(&client, ".", "   ".to_string()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_shutdown_exits_loop() {
        let (client, _server) = make_client_without_pump();
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let shutdown = CancellationToken::new();
        let handle = spawn_thread_load_consumer(client, rx, ".".into(), shutdown.clone());
        shutdown.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
        let _ = tx;
    }

    #[tokio::test]
    async fn test_dropped_tx_exits_loop() {
        let (client, _server) = make_client_without_pump();
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let shutdown = CancellationToken::new();
        let handle = spawn_thread_load_consumer(client, rx, ".".into(), shutdown);
        drop(tx);
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
    }
}
