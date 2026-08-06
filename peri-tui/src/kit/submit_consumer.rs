//! 提交消费者——SUBMIT_TX channel → acp_client.prompt()。
//!
//! 这是 S4 的核心：InputArea 写入 channel 的用户文本在此被取出，转成
//! `MessageContent::text(...)` 并调用 `AcpTuiClient::prompt()`。同时承担
//! **首次会话懒初始化**：用户第一次提交时如果还没有 session，先创建。
//!
//! 设计：
//! - 单消费者，从 `mpsc::UnboundedReceiver<SubmitRequest>` 顺序读取（保证提交顺序）
//! - 与 notifier / bridge 并行运行，三者通过独立 channel 解耦
//! - shutdown 信号触发时干净退出，不发残留请求

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Local;
use fluent_bundle::FluentValue;
use peri_acp_types::messages::MessageContent;
use peri_acp_types::permission::PermissionMode;
use ratatui::text::Line;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::acp_client::AcpTuiClient;
use crate::i18n;
use crate::kit::acp_events;
use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch};
use crate::kit::atoms::{
    ACP_STATE, ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, EXIT_REQUESTED, LOADING_EPOCH,
    LOCAL_EVENT_TX, NOTIFICATION, PERI_CONFIG_HANDLE, PERMISSION_MODE_HANDLE, RENDER_HEARTBEAT,
    REWIND_ACTION_TX,
};
use crate::kit::submit_request::{
    ExportMode, SessionControlRequest, SubmitRequest, ViewActionRequest,
};

/// cancel_consumer 中 cancel RPC 的超时上限。transport 死亡时 cancel 可能
/// 挂起——超时后仍执行本地复位（兜底路径，Issue 2026-08-05 S4.2）。
const CANCEL_RPC_TIMEOUT_SECS: u64 = 2;

/// 启动提交消费者后台任务。
///
/// 参数：
/// - `acp_client`：克隆自 build_app_and_acp 返回的 AcpTuiClient（Clone + Arc 内部）
/// - `rx`：SUBMIT_TX 的接收端（由 entry::run_kit_fullscreen 创建 channel 时拿到）
/// - `cwd`：用于首次 new_session 时传给 ACP server
/// - `shutdown`：与 notifier / bridge 共享的同一 CancellationToken
pub fn spawn_submit_consumer(
    acp_client: AcpTuiClient,
    mut rx: mpsc::UnboundedReceiver<SubmitRequest>,
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
                        Some(request) => {
                            if let Err(e) = handle_submit(&acp_client, &cwd, request).await {
                                error!(error = %e, "kit submit_consumer: prompt failed");
                                clear_loading_state();
                            }
                        }
                    }
                }
            }
        }
    })
}

/// 处理单条提交：确保 session 存在 → 发送 prompt。
/// `/clear`（及别名 `/cls` `/reset`）不经过 agent，直接创建新会话。
async fn handle_submit(
    acp_client: &AcpTuiClient,
    cwd: &str,
    request: SubmitRequest,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match request {
        SubmitRequest::AgentText(text) => handle_agent_text_submit(acp_client, cwd, text).await,
        SubmitRequest::KeepGoing => handle_keepgoing_submit(acp_client, cwd).await,
        SubmitRequest::SessionControl(SessionControlRequest::Clear) => {
            handle_clear_submit(acp_client, cwd).await
        }
        SubmitRequest::SessionControl(SessionControlRequest::Rewind(args)) => {
            info!("kit submit_consumer: /rewind intercepted, forwarding to rewind_consumer");
            if let Some(tx) = REWIND_ACTION_TX.get() {
                let _ = tx.send(crate::kit::rewind_action::RewindAction::Confirm {
                    target_message_id: args.target_message_id,
                });
            }
            Ok(())
        }
        SubmitRequest::SessionControl(SessionControlRequest::ToggleSetup) => {
            warn!("kit submit_consumer: unexpected ToggleSetup request in consumer");
            Ok(())
        }
        SubmitRequest::ViewAction(action) => {
            info!(action = ?action, "kit submit_consumer: view-layer command intercepted");
            execute_view_action(action, acp_client, cwd);
            Ok(())
        }
        SubmitRequest::OpenPanel(kind) => {
            warn!(
                ?kind,
                "kit submit_consumer: unexpected OpenPanel request in consumer"
            );
            Ok(())
        }
    }
}

async fn handle_clear_submit(
    acp_client: &AcpTuiClient,
    cwd: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    info!("kit submit_consumer: /clear intercepted, creating new session");

    ACTIVE_SESSION_ID.set(String::new());
    BRIDGE_RESET_COUNTER.set(BRIDGE_RESET_COUNTER.get().wrapping_add(1));
    acp_events::push_view_models_for_reset();
    // S4.2: 与 cancel 路径同构的复位（直接写 + LocalLoadingReset 注入）——
    // bridge 检测到 BRIDGE_RESET_COUNTER 前，直接写保证 UI 立即响应；注入
    // 事件保证 bridge phase 同步复位（幂等，Issue 2026-08-05）。
    clear_loading_state();
    // H1/M3/L5/L10：同步清空弹窗 payload、Todo 列表、输入历史指针，
    // 防止旧 session 残留阻塞新会话。close_popup 在无弹窗时是 no-op，安全；
    // TODO_ITEMS 会在新 session 的 SessionUpdate::Plan 事件到来时重新填充；
    // reset_history_cursor 仅清浏览指针与草稿，INPUT_HISTORY 栈保留。
    // REWIND_PREVIEW 跟随会话生命周期：clear 后旧消息 id 已失效，必须清空，
    // 否则双击 Esc 会看到已删除的候选（服务端 rewind 报 not found）。
    crate::kit::popup_overlay::close_popup();
    *crate::kit::atoms::REWIND_PREVIEW.state().write() = None;
    *crate::kit::atoms::REWIND_TARGET_TEXT.state().write() = None;
    *crate::kit::atoms::REWIND_BUDGET_STATE.state().write() =
        crate::kit::atoms::RewindBudgetState::Idle;
    *crate::kit::atoms::REWIND_QUERY_ERROR.state().write() = None;
    *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();
    crate::kit::input_history::reset_history_cursor();

    let new_sid = acp_client.new_session(cwd, None).await?;
    ACTIVE_SESSION_ID.set(new_sid);
    BRIDGE_RESET_COUNTER.set(BRIDGE_RESET_COUNTER.get().wrapping_add(1));

    acp_events::push_view_models_for_reset();
    // S4.2: 同第一处——/clear 后 bridge 侧 phase 复位（幂等）。
    clear_loading_state();
    Ok(())
}

async fn handle_agent_text_submit(
    acp_client: &AcpTuiClient,
    cwd: &str,
    text: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    if !acp_client.has_session() {
        info!(cwd = %cwd, "kit submit_consumer: creating initial session");
        let session_id = acp_client.new_session(cwd, None).await?;
        ACTIVE_SESSION_ID.set(session_id);
        BRIDGE_RESET_COUNTER.set(BRIDGE_RESET_COUNTER.get().wrapping_add(1));
    }

    // Issue 2026-08-05 返工：在发送 PromptSubmitted 之前生成本轮 prompt 的
    // request_id（uuid v7）——PromptSubmitted 事件与 prompt RPC 必须携带同一
    // id，bridge 才能把它记录为"当前 turn 的 id"，供 stale TurnInterrupted 配对。
    let request_id = Some(uuid::Uuid::now_v7().to_string());

    // 通过 LOCAL_EVENT_TX 发送 PromptSubmitted 事件到 acp_bridge，
    // 由 bridge 统一管理 phase/variant/is_loading 状态。
    {
        if let Some(tx) = LOCAL_EVENT_TX.get() {
            let _ = tx.send(AcpEventWithEpoch {
                event: AcpEventData::PromptSubmitted {
                    request_id: request_id.clone(),
                },
                active_session_id: String::new(),
            });
        } else {
            warn!("LOCAL_EVENT_TX not initialized, PromptSubmitted event dropped");
        }
    }
    // 递增 loading epoch：message_area 据此检测新一轮 loading 会话，
    // 避免 rapid toggle（如 TurnDone → drain_input_buffer → 新 prompt）
    // 在同一渲染周期内完成时组件错过 is_loading 的 false 中间态。
    *LOADING_EPOCH.state().write() += 1;
    tracing::info!(
        target: "msg_scroll_diag",
        "submit_consumer: emitted PromptSubmitted event, LOADING_EPOCH incremented, about to call prompt()",
    );

    let content = MessageContent::text(trimmed.to_string());
    acp_client.prompt(&content, request_id).await.map_err(|e| {
        warn!(error = %e, "kit submit_consumer: prompt RPC failed");
        Box::new(e) as Box<dyn std::error::Error + Send + Sync>
    })?;
    Ok(())
}

/// keepgoing 提交：向 ACP 发送空白 user prompt。
///
/// 与 `handle_agent_text_submit` 的差异：
/// - 不 trim/丢弃空文本——空白 prompt 是 keepgoing 的协议语义（服务端不插入
///   user 消息但继续运行 agent loop，见 `run_session_loop` 的 keepgoing 分支）
/// - 不发送 LocalUserBubble——TUI 不应显示空气泡
/// - 不进输入历史
async fn handle_keepgoing_submit(
    acp_client: &AcpTuiClient,
    _cwd: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !acp_client.has_session() {
        // 无会话（或无可继续的轮次）时 no-op——按钮仅在有 summary 时显示，此为防御分支
        info!("kit submit_consumer: keepgoing ignored (no active session)");
        return Ok(());
    }

    // Issue 2026-08-05 返工：keepgoing 同样生成 request_id（每次 keepgoing RPC
    // 都是新 turn）——修复 v1 在 keepgoing 场景（无 LocalUserBubble、代际不变）
    // 下 stale 判定失效的漏洞。
    let request_id = Some(uuid::Uuid::now_v7().to_string());

    // 与 handle_agent_text_submit 相同的 loading 状态切换：PromptSubmitted 事件
    // 由 bridge 统一管理 phase/variant/is_loading 状态。
    if let Some(tx) = LOCAL_EVENT_TX.get() {
        let _ = tx.send(AcpEventWithEpoch {
            event: AcpEventData::PromptSubmitted {
                request_id: request_id.clone(),
            },
            active_session_id: String::new(),
        });
    } else {
        warn!("LOCAL_EVENT_TX not initialized, PromptSubmitted event dropped");
    }
    // 递增 loading epoch：message_area 据此检测新一轮 loading 会话（同 handle_agent_text_submit）
    *LOADING_EPOCH.state().write() += 1;
    tracing::info!(
        target: "msg_scroll_diag",
        "submit_consumer: keepgoing prompt submitted, LOADING_EPOCH incremented",
    );

    acp_client
        .prompt(&MessageContent::text(""), request_id)
        .await
        .map_err(|e| {
            warn!(error = %e, "kit submit_consumer: keepgoing prompt RPC failed");
            Box::new(e) as Box<dyn std::error::Error + Send + Sync>
        })?;
    Ok(())
}

/// 执行视图层操作。
fn execute_view_action(action: ViewActionRequest, acp_client: &AcpTuiClient, cwd: &str) {
    match action {
        ViewActionRequest::CycleProvider => {
            // 语义改为循环 active_alias 四档（fable → opus → sonnet → haiku）
            if let Some(cfg_handle) = PERI_CONFIG_HANDLE.get() {
                let cfg = cfg_handle.read();
                let aliases = ["fable", "opus", "sonnet", "haiku"];
                let current = &cfg.config.active_alias;
                let idx = aliases.iter().position(|a| *a == current).unwrap_or(0);
                let next = aliases[(idx + 1) % aliases.len()].to_string();
                drop(cfg);
                let client = acp_client.clone();
                let cfg_handle = cfg_handle.clone();
                tokio::spawn(async move {
                    let mut new_cfg = cfg_handle.read().clone();
                    new_cfg.config.active_alias = next;
                    let _ = client.update_config(&new_cfg).await;
                });
            }
        }
        ViewActionRequest::CyclePermissionMode => {
            if let Some(mode_handle) = PERMISSION_MODE_HANDLE.get() {
                let current = mode_handle.load();
                let next = match current {
                    PermissionMode::Default => PermissionMode::AcceptEdit,
                    PermissionMode::AcceptEdit => PermissionMode::AutoMode,
                    PermissionMode::AutoMode => PermissionMode::Bypass,
                    PermissionMode::Bypass => PermissionMode::Default,
                };
                mode_handle.store(next);
            }
        }
        ViewActionRequest::ExportText(mode) => {
            let message = match export_debug_text(mode, cwd) {
                Ok(path) => i18n::tr_args(
                    "export-success",
                    &[("path".into(), FluentValue::from(path.display().to_string()))],
                ),
                Err(err) => i18n::tr_args(
                    "export-fail",
                    &[("error".into(), FluentValue::from(err.to_string()))],
                ),
            };
            *NOTIFICATION.state().write() = Some(crate::kit::atoms::Notification {
                message,
                until: Instant::now() + Duration::from_secs(5),
            });
            RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
        }
        ViewActionRequest::Exit => {
            info!("kit submit_consumer: /exit command received, requesting app exit");
            EXIT_REQUESTED.set(true);
        }
    }
}

fn export_debug_text(
    mode: ExportMode,
    cwd: &str,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let lines = collect_debug_export_lines(mode);
    let text = lines_to_plain_text(&lines);
    let path = debug_export_path(cwd);
    std::fs::write(&path, text)?;
    Ok(path)
}

fn collect_debug_export_lines(_mode: ExportMode) -> Vec<Line<'static>> {
    let snapshot = crate::kit::atoms::VIEW_MODELS.state().read().clone();
    let mut all_lines: Vec<Line<'static>> = Vec::new();
    for item in snapshot.items.iter() {
        let text = extract_vm_text(item);
        if !text.is_empty() {
            all_lines.push(Line::from(text));
        }
    }
    all_lines
}

fn extract_vm_text(vm: &crate::kit::tui_render_unit::TuiRenderUnit) -> String {
    use crate::kit::tui_render_unit::TuiRenderUnit;
    match vm {
        TuiRenderUnit::TuiUserBubble(data) => data.text.clone(),
        TuiRenderUnit::TuiAssistantBubble(data) => data.text.clone(),
        TuiRenderUnit::TuiToolCard(data) => format!(
            "[{}] {} -> {}",
            data.tool_name, data.input_summary, data.output_summary
        ),
        TuiRenderUnit::TuiSystemNote(data) => data.text.clone(),
        TuiRenderUnit::TuiSubAgentGroup(data) => {
            let mut text = format!("[SubAgent: {}]", data.agent_name);
            for child in data.view_models.iter() {
                let child_text = extract_vm_text(child);
                if !child_text.is_empty() {
                    text.push_str("\n  ");
                    text.push_str(&child_text);
                }
            }
            text
        }
        TuiRenderUnit::TuiCollapsedGroup(data) => {
            format!("[Collapsed: {} ({} items)]", data.title, data.count)
        }
        TuiRenderUnit::TuiDivider(data) => data.label.as_deref().unwrap_or("---").to_string(),
        TuiRenderUnit::TuiAskUserBlock(data) => data
            .items
            .iter()
            .map(|i| format!("Q: {} A: {}", i.header, i.answer))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn lines_to_plain_text(lines: &[Line<'static>]) -> String {
    let mut output = String::new();
    for line in lines {
        for span in &line.spans {
            output.push_str(span.content.as_ref());
        }
        output.push('\n');
    }
    output
}

fn debug_export_path(cwd: &str) -> PathBuf {
    Path::new(cwd).join(format!(
        "peri-debug-export-{}.txt",
        Local::now().format("%Y%m%d-%H%M%S")
    ))
}

/// 清空 loading 状态——prompt 失败 / cancel / /clear 时兜底，防止 loading 永久卡死。
///
/// S4.2 双保险（Issue 2026-08-05）：
/// 1. 直接写 ACP_STATE.is_loading=false——bridge 已退出（shutdown 路径）时
///    事件无人消费，直接写是唯一生效路径；
/// 2. 注入 LocalLoadingReset 内部事件——bridge 存活时同步复位 phase（幂等），
///    否则后续任意事件触发 push_acp_state 会用 phase 重算 is_loading=true，
///    造成取消后 loading 闪回 + 提交判定竞态（误入 INPUT_BUFFER）。
fn clear_loading_state() {
    {
        let ref_guard = ACP_STATE.state();
        let mut acp = ref_guard.write();
        acp.is_loading = false;
    }
    if let Some(tx) = LOCAL_EVENT_TX.get() {
        let _ = tx.send(AcpEventWithEpoch {
            event: AcpEventData::LocalLoadingReset,
            active_session_id: String::new(),
        });
    } else {
        warn!("LOCAL_EVENT_TX not initialized, LocalLoadingReset event dropped");
    }
}

/// 启动取消消费者后台任务。
///
/// 监听 CANCEL_TX channel，收到信号时调用 `acp_client.cancel()` 中断当前 agent。
/// 与 submit_consumer / notifier / bridge 并行运行，通过独立 channel 解耦。
///
/// 参数：
/// - `acp_client`：克隆自 build_app_and_acp 返回的 AcpTuiClient
/// - `rx`：CANCEL_TX 的接收端（由 entry::run_kit_fullscreen 创建 channel 时拿到）
/// - `shutdown`：与 notifier / bridge 共享的同一 CancellationToken
pub fn spawn_cancel_consumer(
    acp_client: AcpTuiClient,
    mut rx: mpsc::UnboundedReceiver<()>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("kit cancel_consumer: started");
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("kit cancel_consumer: shutdown signal received, exiting");
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        None => {
                            info!("kit cancel_consumer: CANCEL_TX dropped, exiting");
                            break;
                        }
                        Some(()) => {
                            // Issue 2026-08-05 S4.2: 顺序——先 cancel RPC（带
                            // 超时）再复位。cancel 完成前服务端仍在推流（流尾巴），
                            // 若先复位，这些事件会把 bridge phase 拉回 PromptRunning
                            // （loading 闪回）；cancel 完成后流已停止，复位才稳定。
                            // timeout 防止 transport 死亡时 cancel 挂起阻塞复位
                            // （兜底路径：transport 死 / prompt task panic 时
                            // TurnInterrupted 永不到达，复位使 Ctrl+C 双击退出
                            // 路径恢复可用，与服务端 TurnInterrupted 幂等）。
                            let cancel_result = tokio::time::timeout(
                                Duration::from_secs(CANCEL_RPC_TIMEOUT_SECS),
                                acp_client.cancel(),
                            )
                            .await;
                            match cancel_result {
                                Ok(Ok(())) => {}
                                Ok(Err(e)) => tracing::warn!(%e, "cancel_consumer: cancel 失败"),
                                Err(_) => {
                                    tracing::warn!("cancel_consumer: cancel RPC 超时，继续本地复位")
                                }
                            }
                            clear_loading_state();
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
#[path = "submit_consumer_test.rs"]
mod tests;
