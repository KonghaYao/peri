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
//! ACP host 端 `session/load` 会通过 `peri/unstable_event` 推送 ViewCommit
//! 通知（见 peri-acp/src/host/requests.rs:241-260），由 kit_notifier → acp_bridge
//! 转化为 `AcpEventData::ViewCommit`，自动覆写 `VIEW_MODELS` atom——面板会
//! 自然关闭（Enter 后由 panel_overlay 处理），消息流自动刷新。

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use fluent_bundle::FluentValue;

use crate::acp_client::AcpTuiClient;
use crate::i18n;
use crate::kit::atoms;

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
    // 3. BRIDGE_RESET_COUNTER 递增后 bridge 会在下一个事件到达时自动清空
    //    committed。session/load replay 事件即为下一个事件——旧内容在 replay
    //    到达前保持可见，replay 到达后 bridge 清空并重建，实现平滑过渡，
    //    避免 push_view_models_for_reset 造成的空白闪烁。
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
    // REWIND_PREVIEW 跟随会话生命周期：切换 thread 后旧 thread 的消息 id 已
    // 失效，必须清空，否则双击 Esc 会看到旧 thread 的候选（服务端 rewind 报
    // not found）。新 thread 的首个 turn 完成后服务端会推送新的 preview。
    *crate::kit::atoms::REWIND_PREVIEW.state().write() = None;
    *crate::kit::atoms::REWIND_TARGET_TEXT.state().write() = None;
    *crate::kit::atoms::REWIND_BUDGET_STATE.state().write() =
        crate::kit::atoms::RewindBudgetState::Idle;
    *crate::kit::atoms::REWIND_QUERY_ERROR.state().write() = None;
    *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();
    crate::kit::input_history::reset_history_cursor();
    // 4. 最后加载 session。replay notification 到达时 active session 已就绪，不会被误丢。
    tracing::info!(
        target: "msg_scroll_diag",
        thread_id = %thread_id,
        "thread_load_consumer: about to call load_session() for history replay",
    );
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
#[path = "thread_load_consumer_test.rs"]
mod tests;
