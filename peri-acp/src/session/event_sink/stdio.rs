//! SDK stdio sink：标准 SessionUpdate 与来源 metadata。

use super::{Client, ConnectionTo, EventSink, SdkSessionId, SessionNotification, SessionUpdate};
use crate::event::map_event;
use async_trait::async_trait;
use peri_acp_types::{event::ExecutorEvent, PeriCaps};
use serde_json::json;
use tracing::{debug, error};

/// Peri 结构化 reminder 通知（`peri/systemReminder`）。
///
/// ACP v1 的 `SessionUpdate` 没有 reminder 变体，因此这一出口在客户端显式声明
/// `peri.systemReminder` 能力时使用 Peri 扩展通知承载 canonical DTO；未声明的
/// 客户端明确不承载（可诊断），不继承 [`EventSink`] 的默认 no-op 冒充成功。
#[derive(
    Debug, Clone, serde::Serialize, serde::Deserialize, agent_client_protocol::JsonRpcNotification,
)]
#[notification(method = "peri/systemReminder")]
struct PeriSystemReminderNotification {
    #[serde(rename = "sessionId")]
    session_id: SdkSessionId,
    reminder: peri_acp_types::system_reminder::SystemReminder,
    replay: bool,
}

/// Build ACP-standard metadata for routing output to its originating SubAgent.
fn source_agent_meta(source_agent_id: &str) -> agent_client_protocol::schema::v1::Meta {
    serde_json::Map::from_iter([(
        "peri".to_string(),
        json!({ "sourceAgentId": source_agent_id }),
    )])
}

/// Attach source identity to the typed notification field preserved by ACP SDKs.
pub(super) fn session_notification(
    session_id: SdkSessionId,
    update: SessionUpdate,
    source_agent_id: Option<&str>,
) -> SessionNotification {
    let notification = SessionNotification::new(session_id, update);
    match source_agent_id {
        Some(source_agent_id) => notification.meta(source_agent_meta(source_agent_id)),
        None => notification,
    }
}

// ── SDK-backed EventSink for stdio path ─────────────────────────────────────

/// [`EventSink`] backed by the SDK's [`ConnectionTo<Client>`].
///
/// Sends standard ACP `session/update` notifications only (no `peri/*` custom
/// notifications — those are TUI-specific). Used by the stdio `peri acp` mode
/// which communicates with external IDE clients via the agent-client-protocol SDK.
pub struct StdioEventSink {
    cx: ConnectionTo<Client>,
    session_id: SdkSessionId,
    caps: PeriCaps,
}

impl StdioEventSink {
    /// 本 sink 承载 reminder 的显式声明（M13）：结构化提醒经
    /// `peri/systemReminder` 扩展通知下发，且只在客户端声明
    /// `peri.systemReminder` 时发送；其余情况明确不承载并留下诊断。
    pub const CARRIES_SYSTEM_REMINDERS: bool = true;

    pub fn new(cx: ConnectionTo<Client>, session_id: SdkSessionId, caps: PeriCaps) -> Self {
        Self {
            cx,
            session_id,
            caps,
        }
    }

    /// Send an arbitrary `SessionUpdate` notification through the SDK connection.
    pub fn send_update(&self, update: SessionUpdate) {
        let notif = SessionNotification::new(self.session_id.clone(), update);
        if let Err(e) = self.cx.send_notification(notif) {
            error!(error = %e, "StdioEventSink: failed to send SessionUpdate");
        }
    }
}

/// stdio 出口对一条 reminder 的承载决策（纯函数，便于定向测试）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StdioReminderCarriage {
    /// 客户端声明 `peri.systemReminder`：发送结构化 `peri/systemReminder` 通知。
    Structured,
    /// 未声明结构化能力：明确不承载（可诊断），不静默冒充成功。
    NotCarried,
    /// 未声明客户端受众（如 Model-only recall）：任何形态都不下发。
    FilteredOut,
}

/// 先按 H8 受众规则过滤，再按会话 caps 选择承载形态。
pub(super) fn reminder_carriage(
    caps: &PeriCaps,
    reminder: &peri_acp_types::system_reminder::SystemReminder,
) -> StdioReminderCarriage {
    if !peri_acp_types::system_reminder::reminder_egress_allowed(
        reminder,
        peri_acp_types::system_reminder::ReminderAudience::Tui,
    ) {
        return StdioReminderCarriage::FilteredOut;
    }
    if caps.system_reminder {
        StdioReminderCarriage::Structured
    } else {
        StdioReminderCarriage::NotCarried
    }
}

#[async_trait]
impl EventSink for StdioEventSink {
    /// 显式 reminder 映射：先按 H8 受众规则过滤，再按 session caps 决定形态。
    ///
    /// - 未声明客户端受众（例如 Model-only recall）→ 一律不下发；
    /// - 声明 `peri.systemReminder` → 结构化 `peri/systemReminder` 通知；
    /// - 未声明 → 该类客户端不承载结构化提醒，留下显式诊断而不是静默 no-op。
    ///
    /// 标准 `session/update` 通道不投影 reminder（`map_event` 对
    /// `ExecutorEvent::SystemReminder` 返回空），因此不存在标准事件与专用 push
    /// 双发。
    async fn push_system_reminder(
        &self,
        _session_id: &str,
        reminder: &peri_acp_types::system_reminder::SystemReminder,
        replay: bool,
    ) {
        match reminder_carriage(&self.caps, reminder) {
            StdioReminderCarriage::FilteredOut => {
                debug!(
                    source = %reminder.source,
                    kind = %reminder.kind,
                    delivery = ?reminder.delivery,
                    replay,
                    "stdio sink: system reminder is not addressed to the client audience"
                );
                return;
            }
            StdioReminderCarriage::NotCarried => {
                debug!(
                    source = %reminder.source,
                    kind = %reminder.kind,
                    delivery = ?reminder.delivery,
                    replay,
                    "stdio sink: client did not negotiate peri.systemReminder; the structured reminder is not carried"
                );
                return;
            }
            StdioReminderCarriage::Structured => {}
        }
        let notification = PeriSystemReminderNotification {
            session_id: self.session_id.clone(),
            reminder: reminder.clone(),
            replay,
        };
        if let Err(e) = self.cx.send_notification(notification) {
            error!(error = %e, "StdioEventSink: failed to send system reminder");
        }
    }

    async fn push_event(&self, _session_id: &str, event: &ExecutorEvent, context_window: u32) {
        if let ExecutorEvent::SystemReminder(reminder) = event {
            self.push_system_reminder(_session_id, reminder, false)
                .await;
            return;
        }
        let mapped = map_event(event, context_window, &self.caps);
        for m in mapped {
            for update in m.updates {
                let notif = session_notification(
                    self.session_id.clone(),
                    update,
                    m.source_agent_id.as_deref(),
                );
                if let Err(e) = self.cx.send_notification(notif) {
                    error!(error = %e, "StdioEventSink: failed to send SessionNotification");
                    break;
                }
            }
        }
    }

    async fn push_done(&self, _session_id: &str, _stop_reason: &str, _request_id: Option<&str>) {
        // No explicit done signal in standard ACP protocol.
    }
}

#[cfg(test)]
#[path = "stdio_test.rs"]
mod tests;
