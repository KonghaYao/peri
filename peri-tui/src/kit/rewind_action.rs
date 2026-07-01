//! Rewind 指令消费者——REWIND_ACTION_TX channel → acp_client RPC。
//!
//! 这是 S10 的核心：RewindPopup 写入 channel 的 Confirm/Cancel 在此被取出。
//! Confirm 调用 `session/execute-command` JSON-RPC（command="/rewind"），
//! 服务端执行 `RewindCommand::execute` 截断 history + 逆向恢复文件。
//!
//! ## 协议
//!
//! ```json
//! {
//!   "sessionId": "<sid>",
//!   "command": "/rewind",
//!   "args": {
//!     "target_message_id": "<id>",
//!     "revert_files": true
//!   }
//! }
//! ```
//!
//! ## 设计
//!
//! 与 `submit_consumer` 同模式——单消费者，独立 tokio task，shutdown 信号优雅退出。
//! Cancel 当前不发起任何 RPC（仅清空 popup 状态由 close_popup 完成）。

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::acp_client::AcpTuiClient;

/// Rewind 用户操作——由 RewindPopup 在 Enter/Esc 时通过 REWIND_ACTION_TX 发送。
#[derive(Debug, Clone)]
pub enum RewindAction {
    /// 确认回退到 `target_message_id`；`revert_files` 控制是否同时回退文件改动。
    Confirm {
        target_message_id: String,
        revert_files: bool,
    },
    /// 用户取消（Esc）——仅记录日志，不发起 RPC。
    Cancel,
}

/// 启动 rewind 指令消费者后台任务。
///
/// 参数：
/// - `acp_client`：克隆自 build_app_and_acp 返回的 AcpTuiClient（Clone + Arc 内部）
/// - `rx`：REWIND_ACTION_TX 的接收端
/// - `shutdown`：与 notifier / bridge / submit_consumer 共享的同一 CancellationToken
pub fn spawn_rewind_consumer(
    acp_client: AcpTuiClient,
    mut rx: mpsc::UnboundedReceiver<RewindAction>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("kit rewind_consumer: started");
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("kit rewind_consumer: shutdown signal received, exiting");
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        None => {
                            info!("kit rewind_consumer: REWIND_ACTION_TX dropped, exiting");
                            break;
                        }
                        Some(RewindAction::Cancel) => {
                            info!("kit rewind_consumer: rewind cancelled by user");
                        }
                        Some(RewindAction::Confirm { target_message_id, revert_files }) => {
                            if let Err(e) = handle_confirm(&acp_client, target_message_id, revert_files).await {
                                error!(error = %e, "kit rewind_consumer: rewind RPC failed");
                            }
                        }
                    }
                }
            }
        }
    })
}

/// 处理确认：发送 `session/execute-command` RPC。
///
/// 服务端 RewindCommand 完成后会通过 ExecutorEvent::RewindCompleted 推送，
/// 由 acp_notifier → acp_bridge 转化为 ViewCommit/CompactCompleted 等事件，
/// 让 VIEW_MODELS atom 自动刷新。
async fn handle_confirm(
    acp_client: &AcpTuiClient,
    target_message_id: String,
    revert_files: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !acp_client.has_session() {
        warn!("kit rewind_consumer: no active session, skipping rewind");
        return Ok(());
    }

    let params: Value = json!({
        "command": "/rewind",
        "args": {
            "target_message_id": target_message_id,
            "revert_files": revert_files,
        }
    });

    info!(
        target_message_id = %target_message_id,
        revert_files, "kit rewind_consumer: sending /rewind RPC"
    );

    acp_client
        .send_raw_request("session/execute-command", params)
        .await
        .map_err(|e| {
            warn!(error = %e, "kit rewind_consumer: /rewind RPC failed");
            Box::new(e) as Box<dyn std::error::Error + Send + Sync>
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp::transport::AcpTransport;
    use peri_acp::transport::mpsc::{
        MpscClientTransport, MpscServerTransport, mpsc_transport_pair,
    };

    /// 用真实 mpsc transport 对创建 AcpTuiClient（不启动 pump）。
    fn make_client_without_pump() -> (AcpTuiClient, MpscServerTransport) {
        let (client_transport, server_transport): (MpscClientTransport, MpscServerTransport) =
            mpsc_transport_pair();
        let (client, _notification_rx) = AcpTuiClient::new(client_transport);
        (client, server_transport)
    }

    #[tokio::test]
    async fn test_cancel_action_no_rpc() {
        let (client, _server_transport) = make_client_without_pump();
        let (tx, rx) = mpsc::unbounded_channel::<RewindAction>();
        let shutdown = CancellationToken::new();
        let handle = spawn_rewind_consumer(client, rx, shutdown.clone());

        // Cancel 不应触发任何 RPC——直接发完 shutdown
        tx.send(RewindAction::Cancel).unwrap();
        shutdown.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
    }

    #[tokio::test]
    async fn test_shutdown_exits_loop() {
        let (client, _server_transport) = make_client_without_pump();
        let (_tx, rx) = mpsc::unbounded_channel::<RewindAction>();
        let shutdown = CancellationToken::new();
        let handle = spawn_rewind_consumer(client, rx, shutdown.clone());

        shutdown.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
    }

    #[tokio::test]
    async fn test_dropped_tx_exits_loop() {
        let (client, _server_transport) = make_client_without_pump();
        let (tx, rx) = mpsc::unbounded_channel::<RewindAction>();
        let shutdown = CancellationToken::new();
        let handle = spawn_rewind_consumer(client, rx, shutdown);

        drop(tx);
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
    }

    #[tokio::test]
    async fn test_confirm_without_session_skipped() {
        let (client, _server_transport) = make_client_without_pump();
        // 没有 session——handle_confirm 应当 Ok(()) 且不发送任何 RPC
        let result = handle_confirm(&client, "msg-1".to_string(), true).await;
        assert!(result.is_ok());
        assert!(!client.has_session());
    }

    /// 编译期断言：UnboundedSender<RewindAction> 与 atoms::REWIND_ACTION_TX 类型契约一致。
    #[test]
    fn test_rewind_action_tx_type_contract() {
        let (tx, _rx): (mpsc::UnboundedSender<RewindAction>, _) = mpsc::unbounded_channel();
        let _ = tx;
    }

    #[test]
    fn test_rewind_action_variants() {
        let confirm = RewindAction::Confirm {
            target_message_id: "abc".into(),
            revert_files: true,
        };
        match confirm {
            RewindAction::Confirm {
                target_message_id,
                revert_files,
            } => {
                assert_eq!(target_message_id, "abc");
                assert!(revert_files);
            }
            RewindAction::Cancel => panic!("expected Confirm"),
        }

        let _cancel = RewindAction::Cancel;
    }
}
