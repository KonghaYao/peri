//! kit 路径专用 ACP notifier——AcpNotification → AcpEventData 转换器。
//!
//! 与 legacy `runtime::acp_notifier` 的关键区别：
//!
//! - **不做 TuiEvent 中间态**：legacy 先转 `TuiEvent::AcpEvent {event:"agent-event", ..}`
//!   再由 main_loop 反向解包，效率低且绕路。kit 直接在 notifier 内完成 DTO 转换，
//!   产出的 `AcpEventData` 立即送入 `spawn_acp_bridge`。
//! - **以 UnstableEvent 为流式主通道**：ACP 服务端的高频流式事件
//!   （text-chunk / reasoning-chunk / tool-started / tool-ended / view-commit /
//!   turn-done / ...）通过 `peri/unstable-event` notification 携带，event 字段是
//!   kebab-case 字符串，data 是 JSON——这恰好匹配 `AcpEventData::decode` 的输入。
//! - **AgentEvent DTO 暂时忽略**：`peri/agent_event` 携带的 AcpEvent 变体
//!   （TurnCommitted/StateSnapshotMeta/CompactCompleted/...）属于 v2 低频 DTO，
//!   kit 路径目前只关心 unstable-event 流。S5+ 扩展时再接入。
//!
//! 该任务是**纯转换 + channel push**——不做状态突变。

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::acp_client::AcpNotification;
use crate::kit::acp_types::AcpEventData;
use crate::kit::atoms::AVAILABLE_SLASH_COMMANDS;

/// 启动 kit ACP notifier 后台任务。
///
/// 从 `notification_rx` 读取 `AcpNotification`，把可识别的流式事件转换为
/// `AcpEventData` 推入 `bridge_tx`，由 `spawn_acp_bridge` 消费并写入 Atom。
///
/// 通道关闭（transport 断开）或 shutdown 触发时干净退出。
pub fn spawn_kit_notifier(
    mut notification_rx: mpsc::UnboundedReceiver<AcpNotification>,
    bridge_tx: mpsc::UnboundedSender<AcpEventData>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    debug!("kit ACP notifier: shutdown signal received, exiting");
                    break;
                }
                n = notification_rx.recv() => {
                    match n {
                        Some(notif) => forward_notification(&bridge_tx, notif),
                        None => {
                            debug!("kit ACP notifier: notification channel closed (transport disconnected)");
                            break;
                        }
                    }
                }
            }
        }
    })
}

/// 把单条 `AcpNotification` 转换并推入 bridge channel。
///
/// 设计决策见模块级注释：UnstableEvent 是主通道，其他变体目前 silent drop。
fn forward_notification(bridge_tx: &mpsc::UnboundedSender<AcpEventData>, n: AcpNotification) {
    match n {
        AcpNotification::UnstableEvent { event, data, .. } => {
            let decoded = AcpEventData::decode(&event, data);
            if matches!(decoded, AcpEventData::Unknown { .. }) {
                debug!(event = %event, "kit ACP notifier: unknown unstable-event, dropping");
                return;
            }
            if let Err(e) = bridge_tx.send(decoded) {
                warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping event");
            }
        }
        // kit notifier: extract AvailableCommandsUpdate from SessionUpdate
        // and write to AVAILABLE_SLASH_COMMANDS atom for InputArea slash popup.
        AcpNotification::SessionUpdate { params, .. } => {
            handle_session_update(params);
        }
        // 暂未在 kit 路径处理——S5+ 接入 DTO 事件时再扩展
        AcpNotification::AgentEvent { .. }
        | AcpNotification::AgentDone { .. }
        | AcpNotification::RequestPermission { .. }
        | AcpNotification::Elicitation { .. }
        | AcpNotification::PredictionReady { .. }
        | AcpNotification::Peri { .. }
        | AcpNotification::Other { .. } => {
            debug!("kit ACP notifier: notification variant not yet handled, dropping");
        }
    }
}

/// Extract commands from an AvailableCommandsUpdate SessionUpdate notification
/// and write them to the AVAILABLE_SLASH_COMMANDS atom for InputArea slash completion.
fn handle_session_update(params: serde_json::Value) {
    // params: {"session_id": "...", "update": <SessionUpdate>}
    // SessionUpdate uses #[serde(tag = "sessionUpdate", rename_all = "snake_case")]
    // → AvailableCommandsUpdate serializes as:
    //   {"sessionUpdate": "available_commands_update", "availableCommands": [...]}
    let update = match params.get("update") {
        Some(u) => u,
        None => return,
    };
    // Discriminate: check the tag field, not a container key
    if update.get("sessionUpdate").and_then(|v| v.as_str()) != Some("available_commands_update") {
        return;
    }
    let cmds = match update.get("availableCommands").and_then(|v| v.as_array()) {
        Some(c) => c,
        None => return,
    };
    let entries: Vec<(String, String)> = cmds
        .iter()
        .filter_map(|cmd| {
            let name = cmd.get("name")?.as_str()?;
            let desc = cmd
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Some((name.to_string(), desc.to_string()))
        })
        .collect();
    let len = entries.len();
    *AVAILABLE_SLASH_COMMANDS.state().write() = entries;
    debug!(
        "kit ACP notifier: updated AVAILABLE_SLASH_COMMANDS ({})",
        len
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp::event::AcpEvent;
    use peri_acp_types::event_data::TextChunk;
    use serde_json::json;
    use serial_test::serial;

    fn spawn_test_notifier() -> (
        mpsc::UnboundedSender<AcpNotification>,
        mpsc::UnboundedReceiver<AcpEventData>,
        CancellationToken,
    ) {
        let (notif_tx, notif_rx) = mpsc::unbounded_channel::<AcpNotification>();
        let (bridge_tx, bridge_rx) = mpsc::unbounded_channel::<AcpEventData>();
        let shutdown = CancellationToken::new();
        let _handle = spawn_kit_notifier(notif_rx, bridge_tx, shutdown.clone());
        (notif_tx, bridge_rx, shutdown)
    }

    #[tokio::test]
    async fn test_unstable_event_text_chunk_forwarded() {
        let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::UnstableEvent {
                session_id: "s1".into(),
                event: "text-chunk".into(),
                data: json!({"text": "hi", "agent_id": null}),
            })
            .unwrap();

        let ev = bridge_rx.recv().await.expect("expected one event");
        match ev {
            AcpEventData::TextChunk(tc) => {
                assert_eq!(tc.text, "hi");
                assert!(tc.agent_id.is_none());
            }
            other => panic!("expected TextChunk, got {other:?}"),
        }

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_unstable_event_unknown_dropped() {
        let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::UnstableEvent {
                session_id: "s1".into(),
                event: "future-event".into(),
                data: json!({"x": 1}),
            })
            .unwrap();

        // Unknown 事件被丢弃——bridge_rx 在短时间内应无数据
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(50), bridge_rx.recv()).await;
        assert!(
            matches!(result, Ok(None)) || result.is_err(),
            "expected no event (channel idle or timeout), got {result:?}"
        );

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_agent_event_dropped_for_now() {
        let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::AgentEvent {
                session_id: "s1".into(),
                event: AcpEvent::TurnCommitted {
                    messages_json: "[]".into(),
                    steps: 0,
                },
            })
            .unwrap();

        // AgentEvent DTO 目前 silent drop
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(50), bridge_rx.recv()).await;
        assert!(
            matches!(result, Ok(None)) || result.is_err(),
            "expected AgentEvent to be dropped, got {result:?}"
        );

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_channel_close_exits_cleanly() {
        let (notif_tx, _bridge_rx, shutdown) = spawn_test_notifier();

        // 模拟 transport 断开：drop sender 让 recv() 返回 None
        drop(notif_tx);

        // 给任务一点时间退出
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // shutdown 仍可正常调用（任务已退出，cancel 信号无害）
        shutdown.cancel();
    }

    /// 验证 handle_session_update 能正确解析 ACP SessionUpdate 的 JSON 格式。
    /// SessionUpdate 使用 #[serde(tag = "sessionUpdate")] 内部标签，字段名 camelCase。
    #[test]
    #[serial]
    fn test_handle_session_update_parses_available_commands() {
        crate::kit::atoms::init_atoms();
        let payload = json!({
            "sessionId": "s1",
            "update": {
                "sessionUpdate": "available_commands_update",
                "availableCommands": [
                    {"name": "help", "description": "Show help"},
                    {"name": "clear", "description": "Clear conversation"},
                    {"name": "archify", "description": "Create architecture diagrams"}
                ]
            }
        });
        handle_session_update(payload);
        let entries = AVAILABLE_SLASH_COMMANDS.state().read().clone();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], ("help".to_string(), "Show help".to_string()));
        assert_eq!(
            entries[2],
            (
                "archify".to_string(),
                "Create architecture diagrams".to_string()
            )
        );
    }

    /// 验证非 available_commands_update 的 session/update 不会错误写入 atom。
    #[test]
    #[serial]
    fn test_handle_session_update_skips_non_command_update() {
        crate::kit::atoms::init_atoms();
        // 重置 atom 状态，避免跨测试污染
        *AVAILABLE_SLASH_COMMANDS.state().write() = Vec::new();
        let payload = json!({
            "sessionId": "s1",
            "update": {
                "sessionUpdate": "usage_update",
                "used": 1000,
                "total": 200000
            }
        });
        handle_session_update(payload);
        let entries = AVAILABLE_SLASH_COMMANDS.state().read().clone();
        assert_eq!(entries.len(), 0, "非 commands update 不应写入 atom");
    }

    /// 编译期类型断言：TextChunk 仍可从 peri-acp-types 引用——确保 S3 与 v2 event_data
    /// 类型契约一致。
    #[test]
    fn test_text_chunk_type_contract() {
        let tc = TextChunk {
            text: "x".into(),
            agent_id: None,
        };
        assert_eq!(tc.text, "x");
    }
}
