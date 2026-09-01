//! Thread 切换消费者——THREAD_LOAD_TX channel → acp_client.load_session()。
//!
//! H3 修复：ThreadBrowser 面板 Enter 时把 `thread_id` 通过 channel 推送，
//! 本消费者调用 `AcpTuiClient::load_session(thread_id, cwd, model)`。
//!
//! ## 设计
//!
//! - dispatcher 在同步 `send` 边界先取得 reservation，再入队
//! - 单消费者，从 `mpsc::UnboundedReceiver<ThreadLoadRequest>` 顺序读取
//! - 与 submit_consumer / rewind_consumer 同模式，独立 channel 解耦
//! - shutdown 信号触发时干净退出
//!
//! ## 加载成功后 UI 怎么刷新？
//!
//! ACP host 端 `session/load` 会通过 `peri/unstable_event` 推送 ViewCommit
//! 通知（见 peri-acp/src/host/requests.rs:241-260），由 kit_notifier → acp_bridge
//! 转化为 `AcpEventData::ViewCommit`，自动覆写 `VIEW_MODELS` atom——面板会
//! 自然关闭（Enter 后由 panel_overlay 处理），消息流自动刷新。

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use fluent_bundle::FluentValue;

use crate::acp_client::{AcpTuiClient, client::SessionLoadReservation};
use crate::i18n;
use crate::kit::atoms;

/// Queued load plus the reservation that blocks new prompts until consumption
/// finishes (or the request is dropped/cancelled).
pub struct ThreadLoadRequest {
    thread_id: String,
    _reservation: SessionLoadReservation,
}

impl ThreadLoadRequest {
    #[cfg(test)]
    pub(crate) fn thread_id(&self) -> &str {
        &self.thread_id
    }
}

/// Synchronous producer boundary shared by browser, confirm popup, and compact
/// replay. Reserving here avoids relying on async consumer scheduling order.
#[derive(Clone)]
pub struct ThreadLoadDispatcher {
    tx: mpsc::UnboundedSender<ThreadLoadRequest>,
    client: AcpTuiClient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadLoadDispatchError;

impl ThreadLoadDispatcher {
    pub fn new(tx: mpsc::UnboundedSender<ThreadLoadRequest>, client: AcpTuiClient) -> Self {
        Self { tx, client }
    }

    pub fn send(&self, thread_id: String) -> Result<(), ThreadLoadDispatchError> {
        let request = ThreadLoadRequest {
            thread_id,
            _reservation: self.client.reserve_session_load(),
        };
        self.tx.send(request).map_err(|_| ThreadLoadDispatchError)
    }
}

/// 启动 thread 切换消费者后台任务。
pub fn spawn_thread_load_consumer(
    acp_client: AcpTuiClient,
    mut rx: mpsc::UnboundedReceiver<ThreadLoadRequest>,
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
                        Some(request) => {
                            let thread_id = request.thread_id.clone();
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
                                            title: i18n::tr("thread-switch-confirm-title"),
                                            message: i18n::tr_args(
                                                "thread-switch-bg-tasks-message",
                                                &[(
                                                    "count".into(),
                                                    FluentValue::from(bg_tasks.len() as i64),
                                                )],
                                            ),
                                            details: vec![
                                                i18n::tr_args(
                                                    "thread-switch-task-counts",
                                                    &[
                                                        ("shell".into(), FluentValue::from(shell_c as i64)),
                                                        ("agent".into(), FluentValue::from(agent_c as i64)),
                                                        ("workflow".into(), FluentValue::from(wf_c as i64)),
                                                    ],
                                                ),
                                                i18n::tr("thread-switch-bg-note"),
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

                            let result = tokio::select! {
                                _ = shutdown.cancelled() => {
                                    info!("kit thread_load_consumer: shutdown cancelled in-flight session/load");
                                    drop(request);
                                    break;
                                }
                                result = handle_load(&acp_client, &cwd, thread_id) => result,
                            };
                            // Explicitly keep the reservation alive across the load RPC.
                            drop(request);
                            if let Err(e) = result {
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

    // Client-owned session boundary projects the target before the load RPC so
    // pre-response replay is accepted without a split atom/client fact source.
    tracing::info!(
        target: "msg_scroll_diag",
        thread_id = %thread_id,
        "thread_load_consumer: about to call load_session() for history replay",
    );
    acp_client.load_session(&thread_id, cwd, None).await?;
    Ok(())
}

#[cfg(test)]
#[path = "thread_load_consumer_test.rs"]
mod tests;
