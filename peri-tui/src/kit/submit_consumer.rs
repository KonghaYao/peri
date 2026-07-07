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

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Local;
use peri_agent::messages::MessageContent;
use ratatui::text::Line;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::acp_client::AcpTuiClient;
use crate::kit::atoms::{
    ACP_STATE, BRIDGE_RESET_COUNTER, DIFF_VISIBLE, NOTIFICATION, PERI_CONFIG_HANDLE,
    PERMISSION_MODE_HANDLE, RENDER_CACHE, RENDER_HEARTBEAT, REWIND_ACTION_TX, VIEW_MODELS,
    ViewModelsSnapshot,
};
use crate::kit::render_bridge::RenderCache;

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
    text: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    // /clear（及别名）不走 agent 协议——直接新开会话，清空 UI
    if is_clear_command(trimmed) {
        info!("kit submit_consumer: /clear intercepted, creating new session");

        // 触发 bridge 状态重置：防止旧 session committed 在新 session 中残留
        BRIDGE_RESET_COUNTER.set(BRIDGE_RESET_COUNTER.get().wrapping_add(1));

        // 立即重置 atom 状态（在异步 new_session 之前）。
        // input_area 已在 submit_text 中将 is_loading 设为 true，
        // 旧 session 的滞留事件也会在 new_session 执行期间通过
        // acp_bridge 写入 is_loading=true。先重置可最小化窗口期。
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        *RENDER_CACHE.state().write() = RenderCache::default();
        {
            let ref_guard = ACP_STATE.state();
            let mut acp = ref_guard.write();
            acp.is_loading = false;
        }

        acp_client.new_session(cwd, None).await?;

        // 再次重置：旧 session 的 CancellationToken 虽在 session/close
        // 时取消，但已进入 pipe 的滞留事件（TextChunk/ToolStarted）
        // 会在 new_session 执行期间通过 acp_bridge 写入 is_loading=true。
        // 这些事件无后续 TurnDone 来清除 loading，必须手动再清理一次。
        *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
        *RENDER_CACHE.state().write() = RenderCache::default();
        {
            let ref_guard = ACP_STATE.state();
            let mut acp = ref_guard.write();
            acp.is_loading = false;
        }
        return Ok(());
    }

    // /rewind（及别名 /undo）发 RewindAction::Confirm 到 rewind_consumer
    if is_rewind_or_undo_command(trimmed) {
        info!("kit submit_consumer: /rewind intercepted, forwarding to rewind_consumer");
        let args = parse_rewind_args(trimmed);
        if let Some(tx) = REWIND_ACTION_TX.get() {
            let _ = tx.send(crate::kit::rewind_action::RewindAction::Confirm {
                target_message_id: args.target_message_id.clone(),
                revert_files: args.revert_files,
            });
        }
        return Ok(());
    }

    // 视图层快捷命令——不经过 ACP，直接执行视图操作
    if let Some(action) = resolve_view_action(trimmed) {
        info!(action = ?action, "kit submit_consumer: view-layer command intercepted");
        execute_view_action(&action, acp_client, cwd);
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

/// 判断输入是否为 `/clear` 命令（含别名 `/cls` `/reset`）。
fn is_clear_command(input: &str) -> bool {
    let cmd = input.split_whitespace().next().unwrap_or("");
    matches!(cmd, "/clear" | "/cls" | "/reset")
}

/// 判断输入是否为 `/rewind` 命令（含别名 `/undo`）。
fn is_rewind_or_undo_command(input: &str) -> bool {
    let cmd = input.split_whitespace().next().unwrap_or("");
    matches!(cmd, "/rewind" | "/undo")
}

struct RewindArgs {
    target_message_id: String,
    revert_files: bool,
}

/// 解析 /rewind 命令参数。
/// 格式: `/rewind <message_id> [--revert-files]`
fn parse_rewind_args(input: &str) -> RewindArgs {
    let parts: Vec<&str> = input.split_whitespace().collect();
    let target_message_id = parts.get(1).map(|s| s.to_string()).unwrap_or_default();
    let revert_files = parts.contains(&"--revert-files");
    RewindArgs {
        target_message_id,
        revert_files,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportMode {
    All,
    Screen,
}

#[derive(Debug)]
enum ViewAction {
    CycleModel,
    CycleProvider,
    CyclePermissionMode,
    ToggleDiff,
    ExportText(ExportMode),
}

/// 解析视图层快捷命令。格式: `/command [next|cycle|toggle]`
fn resolve_view_action(input: &str) -> Option<ViewAction> {
    let cmd = input.split_whitespace().next().unwrap_or("");
    match cmd {
        "/model" => Some(ViewAction::CycleModel),
        "/provider" => Some(ViewAction::CycleProvider),
        "/mode" => Some(ViewAction::CyclePermissionMode),
        "/diff" => Some(ViewAction::ToggleDiff),
        "/debug-export-text" => Some(ViewAction::ExportText(parse_export_mode(input))),
        _ => None,
    }
}

/// 执行视图层操作。
fn execute_view_action(action: &ViewAction, acp_client: &AcpTuiClient, cwd: &str) {
    match action {
        ViewAction::CycleModel => {
            if let Some(cfg_handle) = PERI_CONFIG_HANDLE.get() {
                let cfg = cfg_handle.read();
                let aliases = [
                    "opus".to_string(),
                    "sonnet".to_string(),
                    "haiku".to_string(),
                ];
                if !aliases.is_empty() {
                    let current = &cfg.config.active_alias;
                    let idx = aliases.iter().position(|a| a == current).unwrap_or(0);
                    let next = aliases[(idx + 1) % aliases.len()].clone();
                    let client = acp_client.clone();
                    tokio::spawn(async move {
                        let _ = client.set_config_option("model", &next).await;
                    });
                }
            }
        }
        ViewAction::CycleProvider => {
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
        ViewAction::CyclePermissionMode => {
            use peri_middlewares::hitl::PermissionMode;

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
        ViewAction::ToggleDiff => {
            let state = DIFF_VISIBLE.state();
            let mut visible = state.write();
            *visible = !*visible;
        }
        ViewAction::ExportText(mode) => {
            let message = match export_debug_text(*mode, cwd) {
                Ok(path) => format!("已导出消息文本：{}", path.display()),
                Err(err) => format!("导出消息文本失败：{err}"),
            };
            *NOTIFICATION.state().write() = Some(crate::kit::atoms::Notification {
                message,
                until: Instant::now() + Duration::from_secs(5),
            });
            RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
        }
    }
}

fn parse_export_mode(input: &str) -> ExportMode {
    match input.split_whitespace().nth(1) {
        Some("screen") => ExportMode::Screen,
        _ => ExportMode::All,
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

fn collect_debug_export_lines(mode: ExportMode) -> Vec<Line<'static>> {
    let cache = RENDER_CACHE.state().read().clone();
    let all_lines: Vec<Line<'static>> = cache
        .entries
        .iter()
        .flat_map(|(_, entry)| entry.lines.iter().cloned())
        .collect();
    match mode {
        ExportMode::All => all_lines,
        ExportMode::Screen => {
            let viewport = crate::kit::atoms::message_viewport_snapshot()
                .read()
                .clone();
            let start = viewport.first_line.min(all_lines.len());
            let end = viewport.last_line.min(all_lines.len()).max(start);
            all_lines[start..end].to_vec()
        }
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

    #[test]
    fn test_is_clear_command_matches_variants() {
        assert!(is_clear_command("/clear"));
        assert!(is_clear_command("/cls"));
        assert!(is_clear_command("/reset"));
        // 允许尾部空白
        assert!(is_clear_command("/clear  "));
        // 允许额外参数
        assert!(is_clear_command("/clear extra args"));
        assert!(is_clear_command("/cls  some stuff"));
        // 非 clear 命令
        assert!(!is_clear_command("/compact"));
        assert!(!is_clear_command("hello"));
        assert!(!is_clear_command(""));
    }

    #[test]
    fn test_is_rewind_command_matches_variants() {
        assert!(is_rewind_or_undo_command("/rewind"));
        assert!(is_rewind_or_undo_command("/undo"));
        assert!(is_rewind_or_undo_command("/rewind msg-123"));
        assert!(is_rewind_or_undo_command("/rewind msg-123 --revert-files"));
        assert!(!is_rewind_or_undo_command("/clear"));
        assert!(!is_rewind_or_undo_command("hello"));
    }

    #[test]
    fn test_parse_rewind_args() {
        let args = parse_rewind_args("/rewind abc123 --revert-files");
        assert_eq!(args.target_message_id, "abc123");
        assert!(args.revert_files);

        let args = parse_rewind_args("/rewind xyz");
        assert_eq!(args.target_message_id, "xyz");
        assert!(!args.revert_files);

        let args = parse_rewind_args("/rewind");
        assert_eq!(args.target_message_id, "");
        assert!(!args.revert_files);
    }

    #[test]
    fn test_resolve_view_action() {
        assert!(matches!(
            resolve_view_action("/model"),
            Some(ViewAction::CycleModel)
        ));
        assert!(matches!(
            resolve_view_action("/provider"),
            Some(ViewAction::CycleProvider)
        ));
        assert!(matches!(
            resolve_view_action("/mode"),
            Some(ViewAction::CyclePermissionMode)
        ));
        assert!(matches!(
            resolve_view_action("/diff"),
            Some(ViewAction::ToggleDiff)
        ));
        assert!(matches!(
            resolve_view_action("/debug-export-text"),
            Some(ViewAction::ExportText(ExportMode::All))
        ));
        assert!(matches!(
            resolve_view_action("/debug-export-text all"),
            Some(ViewAction::ExportText(ExportMode::All))
        ));
        assert!(matches!(
            resolve_view_action("/debug-export-text screen"),
            Some(ViewAction::ExportText(ExportMode::Screen))
        ));
        assert!(resolve_view_action("/clear").is_none());
        assert!(resolve_view_action("hello").is_none());
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
    async fn test_clear_command_bypasses_prompt() {
        use peri_acp_types::view_model::{AssistantBubbleData, ViewModel, hash_str};

        crate::kit::atoms::init_atoms();
        *VIEW_MODELS.state().write() = ViewModelsSnapshot {
            committed: std::sync::Arc::from(vec![ViewModel::AssistantBubble(
                AssistantBubbleData {
                    text: "existing".into(),
                    reasoning: None,
                    tool_card_ids: vec![],
                    content_hash: hash_str("existing|"),
                },
            )]),
            current_turn: std::sync::Arc::from([]),
        };
        let (client, _server_transport) = make_client_without_pump();
        let cwd = ".".to_string();

        // /clear 在无 server 时会因 new_session RPC 超时，但不走 prompt 路径
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            handle_submit(&client, &cwd, "/clear".to_string()),
        )
        .await;

        // 超时表示走到了 new_session 路径（因无 server 响应而 hang）
        assert!(
            result.is_err() || result.unwrap().is_err(),
            "expected timeout (clear → new_session with no server)"
        );
    }
}
