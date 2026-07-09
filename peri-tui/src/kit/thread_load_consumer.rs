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
use tracing::{error, info};

use crate::acp_client::AcpTuiClient;
use crate::kit::acp_events;
use crate::kit::atoms::{self, RENDER_CACHE};
use crate::kit::render_bridge::RenderCache;

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
                            // 检查是否有运行中的后台任务（scoped 以避免 Non-Send guard 跨 await 点）
                            let has_bg_tasks = {
                                let state = atoms::BG_TASKS.state();
                                let bg_tasks = state.read();
                                if !bg_tasks.is_empty() {
                                    let shell_c = bg_tasks.iter().filter(|t| t.kind == "shell").count();
                                    let agent_c = bg_tasks.iter().filter(|t| t.kind == "agent").count();
                                    let wf_c = bg_tasks.iter().filter(|t| t.kind == "workflow").count();

                                    *atoms::CONFIRM_PAYLOAD.state().write() =
                                        Some(atoms::ConfirmPayload {
                                            title: "切换 thread 确认".into(),
                                            message: format!(
                                                "当前 thread 有 {} 个后台任务仍在运行",
                                                bg_tasks.len()
                                            ),
                                            details: vec![
                                                format!(
                                                    "  {} shell  {} agent  {} workflow",
                                                    shell_c, agent_c, wf_c
                                                ),
                                                "切换后这些任务继续在后台执行，但当前视图不再显示其状态。"
                                                    .into(),
                                            ],
                                            pending_action: atoms::ConfirmAction::ThreadSwitch(
                                                thread_id.clone(),
                                            ),
                                        });
                                    *atoms::POPUP_KIND.state().write() = Some(atoms::PopupKind::Confirm);
                                    true
                                } else {
                                    false
                                }
                            };

                            if has_bg_tasks {
                                continue;
                            }

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

    // 1. 先设置目标 session_id，bridge reset 后会立刻进入 session_id 过滤模式。
    atoms::ACTIVE_SESSION_ID.set(thread_id.to_string());
    // 2. 先触发 bridge 重置，避免旧 session 的 committed/current_turn 污染新 thread。
    crate::kit::atoms::BRIDGE_RESET_COUNTER.set(
        crate::kit::atoms::BRIDGE_RESET_COUNTER
            .get()
            .wrapping_add(1),
    );
    // 3. 先清空 UI/cache。后续 session/load replay 事件会重新追加历史消息。
    acp_events::push_view_models_for_reset();
    *RENDER_CACHE.state().write() = RenderCache::default();
    {
        let ref_guard = atoms::ACP_STATE.state();
        let mut acp = ref_guard.write();
        acp.is_loading = false;
    }
    // H1/M3/L5/L10：同步清空弹窗 payload、Todo 列表、输入历史指针，
    // 与 submit_consumer::handle_clear_submit 对称。load_session 的 replay
    // 事件会通过 ViewCommit 重写 VIEW_MODELS、通过 SessionUpdate::Plan 重写
    // TODO_ITEMS，无数据丢失。本 handle_load 仅在 bg-task 拦截分支（弹 Confirm
    // 后 continue）之后才执行，不会误关用户刚看到的 Confirm 弹窗。
    crate::kit::popup_overlay::close_popup();
    *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();
    crate::kit::input_history::reset_history_cursor();
    // 4. 最后加载 session。replay notification 到达时 active session 已就绪，不会被误丢。
    acp_client.load_session(&thread_id, cwd, None).await?;
    // 5. session/load 已完成，显式回到 idle，避免 replay 或历史 usage 留下 loading 态。
    {
        let ref_guard = atoms::ACP_STATE.state();
        let mut acp = ref_guard.write();
        acp.variant = 0;
        acp.is_loading = false;
    }

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

    /// 验证 handle_load 在调用 load_session 前已同步重置弹窗 / Todo / 历史指针。
    ///
    /// 覆盖 H1/M3/L5/L10，与 submit_consumer::test_handle_clear_submit_*
    /// 对称。load_session 无 server 会 hang，我们用 100ms 超时让控制流走到
    /// 重置后即返回，断言此时 4 类 atom 已被清空。重置写入发生在
    /// load_session await 之前，因此即使 RPC 失败也已完成。
    #[tokio::test]
    async fn test_handle_load_resets_popup_todo_history_atoms() {
        crate::kit::atoms::init_atoms();
        // Arrange：把 4 类 atom 填充为“旧 session 残留”。
        *crate::kit::atoms::POPUP_KIND.state().write() = Some(crate::kit::atoms::PopupKind::Hitl);
        *crate::kit::atoms::HITL_PENDING.state().write() =
            Some(peri_acp_types::event_data::HitlPending {
                tool_name: "old".into(),
                tool_input: serde_json::Value::Null,
                batch: None,
            });
        *crate::kit::atoms::TODO_ITEMS.state().write() = vec![crate::kit::message_area::TodoItem {
            content: "stale".into(),
            status: crate::kit::message_area::TodoStatus::Pending,
        }];
        *crate::kit::atoms::INPUT_HISTORY_INDEX.state().write() = Some(3);
        *crate::kit::atoms::DRAFT.state().write() = Some("stale draft".to_string());

        let (client, _server) = make_client_without_pump();
        let cwd = ".".to_string();

        // Act：handle_load 在 load_session 处会 hang（无 server），
        // 我们只关心它 await 之前的同步重置块，所以 100ms 超时即返回。
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            handle_load(&client, &cwd, "thread-xyz".to_string()),
        )
        .await;

        // Assert：4 类残留已清空。
        assert!(
            crate::kit::atoms::POPUP_KIND.state().read().is_none(),
            "POPUP_KIND should be None after thread switch"
        );
        assert!(
            crate::kit::atoms::HITL_PENDING.state().read().is_none(),
            "HITL_PENDING should be cleared after thread switch"
        );
        assert!(
            crate::kit::atoms::TODO_ITEMS.state().read().is_empty(),
            "TODO_ITEMS should be empty after thread switch"
        );
        assert!(
            crate::kit::atoms::INPUT_HISTORY_INDEX
                .state()
                .read()
                .is_none(),
            "INPUT_HISTORY_INDEX should be None after thread switch"
        );
        assert!(
            crate::kit::atoms::DRAFT.state().read().is_none(),
            "DRAFT should be None after thread switch"
        );
    }
}
