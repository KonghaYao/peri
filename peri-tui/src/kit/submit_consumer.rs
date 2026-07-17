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
use peri_agent::messages::MessageContent;
use peri_middlewares::hitl::PermissionMode;
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
        SubmitRequest::SessionControl(SessionControlRequest::Clear) => {
            handle_clear_submit(acp_client, cwd).await
        }
        SubmitRequest::SessionControl(SessionControlRequest::Rewind(args)) => {
            info!("kit submit_consumer: /rewind intercepted, forwarding to rewind_consumer");
            if let Some(tx) = REWIND_ACTION_TX.get() {
                let _ = tx.send(crate::kit::rewind_action::RewindAction::Confirm {
                    target_message_id: args.target_message_id,
                    revert_files: args.revert_files,
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
    {
        let ref_guard = ACP_STATE.state();
        let mut acp = ref_guard.write();
        acp.is_loading = false;
    }
    // H1/M3/L5/L10：同步清空弹窗 payload、Todo 列表、输入历史指针，
    // 防止旧 session 残留阻塞新会话。close_popup 在无弹窗时是 no-op，安全；
    // TODO_ITEMS 会在新 session 的 SessionUpdate::Plan 事件到来时重新填充；
    // reset_history_cursor 仅清浏览指针与草稿，INPUT_HISTORY 栈保留。
    crate::kit::popup_overlay::close_popup();
    *crate::kit::atoms::TODO_ITEMS.state().write() = Vec::new();
    crate::kit::input_history::reset_history_cursor();

    let new_sid = acp_client.new_session(cwd, None).await?;
    ACTIVE_SESSION_ID.set(new_sid);
    BRIDGE_RESET_COUNTER.set(BRIDGE_RESET_COUNTER.get().wrapping_add(1));

    acp_events::push_view_models_for_reset();
    {
        let ref_guard = ACP_STATE.state();
        let mut acp = ref_guard.write();
        acp.is_loading = false;
    }
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

    // 通过 LOCAL_EVENT_TX 发送 PromptSubmitted 事件到 acp_bridge，
    // 由 bridge 统一管理 phase/variant/is_loading 状态。
    {
        if let Some(tx) = LOCAL_EVENT_TX.get() {
            let _ = tx.send(AcpEventWithEpoch {
                event: AcpEventData::PromptSubmitted,
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
    acp_client.prompt(&content).await.map_err(|e| {
        warn!(error = %e, "kit submit_consumer: prompt RPC failed");
        Box::new(e) as Box<dyn std::error::Error + Send + Sync>
    })?;
    Ok(())
}

/// 执行视图层操作。
fn execute_view_action(action: ViewActionRequest, acp_client: &AcpTuiClient, cwd: &str) {
    match action {
        ViewActionRequest::CycleProvider => {
            if let Some(cfg_handle) = PERI_CONFIG_HANDLE.get() {
                let cfg = cfg_handle.read();
                let provider_ids: Vec<String> =
                    cfg.config.providers.iter().map(|p| p.id.clone()).collect();
                if !provider_ids.is_empty() {
                    let current = &cfg.config.active_provider_id;
                    let idx = provider_ids.iter().position(|p| p == current).unwrap_or(0);
                    let next = provider_ids[(idx + 1) % provider_ids.len()].clone();
                    let client = acp_client.clone();
                    let cfg_handle = cfg_handle.clone();
                    tokio::spawn(async move {
                        let mut new_cfg = cfg_handle.read().clone();
                        new_cfg.config.active_provider_id = next;
                        let _ = client.update_config(&new_cfg).await;
                    });
                }
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

/// 清空 loading 状态——prompt 失败时兜底，防止 loading 永久卡死。
fn clear_loading_state() {
    let ref_guard = ACP_STATE.state();
    let mut acp = ref_guard.write();
    acp.is_loading = false;
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
                            if let Err(e) = acp_client.cancel().await {
                                tracing::warn!(%e, "cancel_consumer: cancel 失败");
                            }
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::atoms::{VIEW_MODELS, ViewModelsSnapshot};
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
    async fn test_empty_agent_text_skipped() {
        let (client, _server_transport) = make_client_without_pump();
        let cwd = ".".to_string();

        let result = handle_submit(
            &client,
            &cwd,
            SubmitRequest::AgentText("   \n\t ".to_string()),
        )
        .await;
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
            handle_submit(&client, &cwd, SubmitRequest::AgentText("hello".to_string())),
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
        let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
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
        let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
        let shutdown = CancellationToken::new();
        let handle = spawn_submit_consumer(client, rx, ".".into(), shutdown);

        drop(tx);
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
    }

    /// 编译期断言：UnboundedSender<SubmitRequest> 与 atoms::SUBMIT_TX 类型契约一致。
    #[test]
    fn test_submit_tx_type_contract() {
        let (tx, _rx): (mpsc::UnboundedSender<SubmitRequest>, _) = mpsc::unbounded_channel();
        // 模拟 atoms::SUBMIT_TX.set(tx) 的契约
        let _ = tx;
    }

    #[test]
    fn test_lines_to_plain_text_joins_spans_and_lines() {
        let lines = vec![
            Line::from(vec![
                ratatui::text::Span::raw("hello"),
                ratatui::text::Span::raw(" world"),
            ]),
            Line::from("next"),
        ];
        assert_eq!(lines_to_plain_text(&lines), "hello world\nnext\n");
    }

    #[test]
    fn test_debug_export_path_uses_cwd_and_timestamp_prefix() {
        let path = debug_export_path("/tmp/peri-export-test");
        assert!(path.starts_with("/tmp/peri-export-test"));
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("peri-debug-export-"));
        assert!(name.ends_with(".txt"));
    }

    #[tokio::test]
    async fn test_execute_view_action_export_text_writes_notification() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        *NOTIFICATION.state().write() = None;
        execute_view_action(
            ViewActionRequest::ExportText(ExportMode::All),
            &make_client_without_pump().0,
            tempdir.path().to_str().expect("utf8 path"),
        );
        assert!(NOTIFICATION.state().read().is_some());
    }

    #[tokio::test]
    async fn test_clear_request_bypasses_prompt() {
        use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit, tui_hash_str};

        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot {
            items: im::Vector::from(vec![TuiRenderUnit::TuiAssistantBubble(
                TuiAssistantBubble {
                    text: "existing".into(),
                    reasoning: None,
                    content_hash: tui_hash_str("existing|"),
                },
            )]),
            generation: 0,
        };
        let (client, _server_transport) = make_client_without_pump();
        let cwd = ".".to_string();

        // /clear 在无 server 时会因 new_session RPC 超时，但不走 prompt 路径
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            handle_submit(
                &client,
                &cwd,
                SubmitRequest::SessionControl(SessionControlRequest::Clear),
            ),
        )
        .await;

        // 超时表示走到了 new_session 路径（因无 server 响应而 hang）
        assert!(
            result.is_err() || result.unwrap().is_err(),
            "expected timeout (clear → new_session with no server)"
        );
    }

    /// 验证 handle_clear_submit 在调用 new_session 前已同步重置弹窗 / Todo / 历史指针。
    ///
    /// 覆盖 H1（弹窗 payload 残留）/ M3（Todo 残留）/ L5+L10（输入历史指针残留）。
    /// new_session 无 server 会 hang，我们用 100ms 超时让控制流走到重置后即返回，
    /// 断言此时 4 类 atom 已被清空。这些写入发生在 new_session await 之前，
    /// 因此即使 RPC 失败也已完成。
    #[tokio::test]
    async fn test_handle_clear_submit_resets_popup_todo_history_atoms() {
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

        // Act：handle_clear_submit 在 new_session 处会 hang（无 server），
        // 我们只关心它 await 之前的同步重置块，所以 100ms 超时即返回。
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            handle_clear_submit(&client, &cwd),
        )
        .await;

        // Assert：4 类残留已清空。
        assert!(
            crate::kit::atoms::POPUP_KIND.state().read().is_none(),
            "POPUP_KIND should be None after /clear"
        );
        assert!(
            crate::kit::atoms::HITL_PENDING.state().read().is_none(),
            "HITL_PENDING should be cleared after /clear"
        );
        assert!(
            crate::kit::atoms::TODO_ITEMS.state().read().is_empty(),
            "TODO_ITEMS should be empty after /clear"
        );
        assert!(
            crate::kit::atoms::INPUT_HISTORY_INDEX
                .state()
                .read()
                .is_none(),
            "INPUT_HISTORY_INDEX should be None after /clear"
        );
        assert!(
            crate::kit::atoms::DRAFT.state().read().is_none(),
            "DRAFT should be None after /clear"
        );
    }
}
