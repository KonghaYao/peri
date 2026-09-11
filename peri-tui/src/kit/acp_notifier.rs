//! ACP notifier——AcpNotification → AcpEventData 转换器。
//!
//! 直接在 notifier 内完成 DTO 转换，产出的 `AcpEventData` 立即送入 `spawn_acp_bridge`。
//! - **以 session/update 为流式主通道**：ACP 服务端的高频流式事件
//!   （agent_message_chunk / agent_thought_chunk / tool_call / tool_call_update）
//!   通过标准 `session/update` 携带，在 `handle_session_update` 中转换为
//!   `AcpEventData` 变体推入双 bridge channel。
//! - **usage_update**：token 消耗通过标准 session/update 的 `usage_update` tag
//!   携带；root usage 更新 spinner，并把本次请求的 cache observation（包括显式
//!   零命中）转为 session-enveloped `CacheUsageUpdated`；bridge 在每次 root
//!   usage_update 上计算覆盖率并在开启配置时注入警告。
//!   auxiliary usage 不产生父 turn 的 cache 提示。
//! - **AgentEvent DTO 已接入**：`peri/agent_event` 携带的 AcpEvent 变体
//!   （SubagentStarted/SubagentStopped/TurnSuspended/RewindCompleted/...）
//!   通过 `convert_agent_event` 转换为 AcpEventData 推入双 bridge channel。
//!   未映射变体（StateSnapshot/BgToolStep/LspDiagnostics/ContextWarning/...）
//!   保持静默丢弃，S5+ 迭代扩展。
//!
//! 私有模块解码 DTO；本任务顺序发布 commands/plan/spinner 状态后推入 bridge，
//! 并在 transport 关闭时复位连接相关 UI 状态。

mod agent_event;
mod interaction;
mod session_update;

use crate::acp_client::{AcpNotification, AcpTuiClient, InteractionOwner};
use crate::i18n;
use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch};
use crate::kit::atoms::{
    ACP_STATE, AVAILABLE_SLASH_COMMANDS, INPUT_BUFFER, NOTIFICATION, RENDER_HEARTBEAT,
    SPINNER_TOKEN_COUNT,
};
use crate::kit::input_area::refresh_slash_items;
use interaction::{handle_elicitation, handle_request_permission};
use peri_acp::event::AcpEvent;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// 启动 kit ACP notifier 后台任务。
///
/// 从 `notification_rx` 读取 `AcpNotification`，把可识别的流式事件转换为
/// `AcpEventData` 推入 `bridge_tx`，由 `spawn_acp_bridge` 消费并写入 Atom。
///
/// 通道关闭（transport 断开）或 shutdown 触发时干净退出。
pub fn spawn_kit_notifier(
    notification_rx: mpsc::UnboundedReceiver<AcpNotification>,
    bridge_tx: mpsc::UnboundedSender<AcpEventWithEpoch>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    spawn_kit_notifier_inner(notification_rx, bridge_tx, shutdown, None)
}

/// Interactive notifier variant. A registered reverse interaction that cannot
/// enter the bridge is claimed and cancelled instead of remaining orphaned.
pub fn spawn_kit_notifier_with_client(
    notification_rx: mpsc::UnboundedReceiver<AcpNotification>,
    bridge_tx: mpsc::UnboundedSender<AcpEventWithEpoch>,
    shutdown: CancellationToken,
    client: AcpTuiClient,
) -> tokio::task::JoinHandle<()> {
    spawn_kit_notifier_inner(notification_rx, bridge_tx, shutdown, Some(client))
}

fn spawn_kit_notifier_inner(
    mut notification_rx: mpsc::UnboundedReceiver<AcpNotification>,
    bridge_tx: mpsc::UnboundedSender<AcpEventWithEpoch>,
    shutdown: CancellationToken,
    client: Option<AcpTuiClient>,
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
                        Some(notif) => {
                            if let Some(owner) = forward_notification(&bridge_tx, notif)
                                && let Some(client) = &client
                            {
                                client.reject_interaction(&owner).await;
                            }
                        }
                        None => {
                            debug!("kit ACP notifier: notification channel closed (transport disconnected)");
                            // Issue 2026-08-05: 事件流中断兜底复位。
                            // transport 死亡后不再有任何事件到达，is_loading 的所有
                            // 复位路径都依赖事件，会永久卡 true 锁死 TUI（Ctrl+C 退出、
                            // /exit、/clear 全被 loading 门禁拦截）。此处直接复位 atom：
                            // 清 loading + 排队输入 + 提示断连（app-agent-disconnected
                            // 文案此前在 FTL 中存在但零引用，此处接上）。
                            ACP_STATE.state().write().is_loading = false;
                            INPUT_BUFFER.state().write().clear();
                            *NOTIFICATION.state().write() = Some(crate::kit::atoms::Notification {
                                message: i18n::tr("app-agent-disconnected"),
                                until: std::time::Instant::now() + std::time::Duration::from_secs(5),
                            });
                            RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
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
/// 设计决策：session/update 是流式主通道（agent_message_chunk / tool_call 等），
/// AgentDone 通过 TurnDone 转换，AgentEvent 通过 `convert_agent_event` 转换。
fn forward_notification(
    bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
    n: AcpNotification,
) -> Option<InteractionOwner> {
    /// 将 AcpEventData 包装为 AcpEventWithEpoch（注入 session_id）。
    fn wrap_with_session(event: AcpEventData, session_id: String) -> AcpEventWithEpoch {
        AcpEventWithEpoch {
            event,
            active_session_id: session_id,
        }
    }

    match n {
        AcpNotification::UnstableEvent {
            session_id,
            event,
            data,
        } => {
            let decoded = AcpEventData::decode(&event, data);
            if matches!(decoded, AcpEventData::Unknown { .. }) {
                debug!(event = %event, "kit ACP notifier: unknown unstable_event, dropping");
                return None;
            }
            let wrapped = wrap_with_session(decoded, session_id);
            if let Err(e) = bridge_tx.send(wrapped) {
                warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping event");
            }
        }
        // kit notifier: extract AvailableCommandsUpdate / plan / streaming
        // from SessionUpdate.
        AcpNotification::SessionUpdate { session_id, params } => {
            if let Some(decoded) = handle_session_update(params, bridge_tx, &session_id) {
                let wrapped = wrap_with_session(decoded, session_id);
                if let Err(e) = bridge_tx.send(wrapped) {
                    warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping session/update streaming event");
                }
            }
        }
        AcpNotification::AgentDone {
            session_id,
            stop_reason,
            request_id,
        } => {
            let decoded = if stop_reason == "cancelled" {
                AcpEventData::TurnInterrupted {
                    reason: "user cancelled".into(),
                    // 透传被中断 turn 的 requestId——bridge 据此识别事件所属 turn，
                    // 丢弃早于当前 turn 的 stale 取消事件（Issue 2026-08-05）。
                    request_id,
                }
            } else {
                AcpEventData::TurnDone
            };
            let wrapped = wrap_with_session(decoded, session_id);
            if let Err(e) = bridge_tx.send(wrapped) {
                warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping agent done");
            }
        }
        AcpNotification::Elicitation {
            owner,
            request_id_json,
            params,
        } => {
            let rejected_owner = owner.clone();
            if !handle_elicitation(owner, request_id_json, &params, bridge_tx) {
                return Some(rejected_owner);
            }
        }
        // peri/agent_event → AcpEvent → AcpEventData 转换
        // SubagentStarted/SubagentStopped 首先映射至此通道；通过 convert_agent_event
        // 转换为 kit 层 DTO 后推送（与 UnstableEvent 路径形成双通道冗余）。
        AcpNotification::AgentEvent { session_id, event } => {
            if let Some(decoded) = convert_agent_event(event) {
                let wrapped = wrap_with_session(decoded, session_id);
                if let Err(e) = bridge_tx.send(wrapped) {
                    warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping AgentEvent");
                }
            }
        }
        AcpNotification::PredictionReady {
            session_id,
            text,
            actions,
        } => {
            // M4: PredictionReady 不再被丢弃，转换为 AcpEventData::Prediction 推入 bridge channel。
            // dispatch_and_notify 仅写入 PREDICTION atom（input_area 订阅显示），不调
            // push_view_models。
            use peri_acp_types::event_data::Prediction;
            let decoded = AcpEventData::Prediction(Prediction { text, actions });
            let wrapped = wrap_with_session(decoded, session_id);
            if let Err(e) = bridge_tx.send(wrapped) {
                warn!(error = %e, "kit ACP notifier: bridge_tx closed, dropping prediction");
            }
        }
        AcpNotification::RequestPermission {
            owner,
            request_id_json,
            params,
        } => {
            let rejected_owner = owner.clone();
            if !handle_request_permission(owner, request_id_json, &params, bridge_tx) {
                return Some(rejected_owner);
            }
        }
        AcpNotification::InteractionTerminal { owner, outcome } => {
            let wrapped = wrap_with_session(
                AcpEventData::InteractionTerminal { owner, outcome },
                String::new(),
            );
            if let Err(error) = bridge_tx.send(wrapped) {
                warn!(error = %error, "kit ACP notifier: bridge closed, dropping interaction terminal");
            }
        }
        AcpNotification::Peri { .. } | AcpNotification::Other { .. } => {
            debug!("kit ACP notifier: notification variant not yet handled, dropping");
        }
    }
    None
}

/// Preserve legacy context publication at the same synchronous dispatch boundary.
fn convert_agent_event(event: AcpEvent) -> Option<AcpEventData> {
    match event {
        // StateSnapshotMeta：从 budget_pct 写入 CONTEXT_USAGE atom（供 StatusBarRow1 显示）
        AcpEvent::StateSnapshotMeta {
            context_total_tokens,
            budget_pct,
            ..
        } => {
            if let Some(total) = context_total_tokens {
                // budget_pct 可能为 None（首轮/token_tracker 无 last_usage），此时仅存总量
                let pct = budget_pct.unwrap_or(0.0);
                *crate::kit::atoms::CONTEXT_USAGE.state().write() = Some((pct, total));
                crate::kit::atoms::RENDER_HEARTBEAT
                    .set(crate::kit::atoms::RENDER_HEARTBEAT.get().wrapping_add(1));
            }
            None
        }
        event => agent_event::decode_agent_event(event),
    }
}

/// Apply status updates before returning the event to the bridge sender.
/// Decoding is pure; commands, plan, and spinner publication stay synchronous here.
fn handle_session_update(
    params: Value,
    _bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
    session_id: &str,
) -> Option<AcpEventData> {
    let update = params.get("update")?;
    match update.get("sessionUpdate").and_then(Value::as_str) {
        Some("available_commands_update") => {
            let entries = session_update::decode_commands(update)?;
            let len = entries.len();
            *AVAILABLE_SLASH_COMMANDS.state().write() = entries;
            refresh_slash_items();
            debug!(
                "kit ACP notifier: updated AVAILABLE_SLASH_COMMANDS ({})",
                len
            );
            None
        }
        Some("plan") => {
            debug!(update = %update, "handle_session_update: plan tag matched");
            crate::kit::acp_events::handle_plan_update(update);
            None
        }
        _ => {
            let decoded = session_update::decode_stream_update(&params, session_id);
            if let Some(token_count) = decoded.token_count {
                *SPINNER_TOKEN_COUNT.state().write() = token_count;
            }
            decoded.event
        }
    }
}

#[cfg(test)]
#[path = "acp_notifier_test.rs"]
mod tests;
