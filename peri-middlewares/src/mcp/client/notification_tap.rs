//! MCP 非订阅通知的宿主可见性汇合点。
//!
//! rmcp 把未路由到 `Subscription` 的通知交给 `ClientHandler`；本模块只做同步入缓冲，
//! 不 await、不唤醒模型。`McpMiddleware::before_model` 与连接状态变化同点位 drain，
//! 产出 `Diagnostic` / `custom_notification` 的 Info 提醒（仅 TUI / 诊断受众）。
//!
//! `subscriptions/listen` 流不承载自定义 / 日志通知：rmcp 的 `SubscriptionSink` 会在
//! server 侧拒绝，client 侧收到即判越界并中断订阅（见 `subscription.rs` 的兜底注释），
//! 因此本模块只接 `tools/call` 等非订阅响应流上的通知。

use std::sync::{Arc, Weak};

use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};
// Logging 类型按 SEP-2577 已废弃，但门控入口仍需保留。
#[allow(deprecated)]
use rmcp::model::{CustomNotification, LoggingLevel, LoggingMessageNotificationParam};
use serde_json::{json, Value};

use super::McpClientPool;

/// 单次 drain 前最多累积的通知条目（按 `(server, method)` 合并后的上限）。
pub(crate) const MAX_PENDING_NOTIFICATIONS: usize = 32;
/// server / method 展示名上限（防止 server 自由文本撑爆 summary）。
const MAX_NOTIFICATION_NAME_BYTES: usize = 256;
/// 单条通知 payload 上限；渲染后的 reminder body 仍远低于 64 KiB 上限。
const MAX_NOTIFICATION_PAYLOAD_BYTES: usize = 2 * 1024;
const TRUNCATION_MARKER: &str = "…（已截断）";
const LOGGING_MESSAGE_METHOD: &str = "notifications/message";

/// 一条待投递的 MCP 自定义通知（同类通知合并为最新 payload + 次数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingMcpNotification {
    pub(crate) server: String,
    pub(crate) method: String,
    pub(crate) payload: String,
    pub(crate) occurrences: u32,
}

/// `ClientHandler` 侧的轻量适配器：持 pool 弱引用，回调内只同步写缓冲。
pub(crate) struct NotificationTap {
    pool: Weak<McpClientPool>,
    server: String,
}

impl NotificationTap {
    pub(crate) fn new(pool: &Arc<McpClientPool>, server: &str) -> Self {
        Self {
            pool: Arc::downgrade(pool),
            server: server.to_string(),
        }
    }

    pub(crate) fn on_custom_notification(&self, notification: CustomNotification) {
        tracing::debug!(server = %self.server, method = %notification.method, "MCP 自定义通知到达");
        let Some(pool) = self.pool.upgrade() else {
            tracing::debug!(
                server = %self.server,
                method = %notification.method,
                "MCP 自定义通知丢弃：client pool 已释放"
            );
            return;
        };
        pool.record_custom_notification(
            &self.server,
            &notification.method,
            notification.params.as_ref(),
        );
    }

    #[allow(deprecated)]
    pub(crate) fn on_logging_message(&self, params: LoggingMessageNotificationParam) {
        tracing::debug!(
            server = %self.server,
            level = ?params.level,
            logger = ?params.logger,
            "MCP 日志通知到达"
        );
        let Some(pool) = self.pool.upgrade() else {
            tracing::debug!(
                server = %self.server,
                level = ?params.level,
                "MCP 日志通知丢弃：client pool 已释放"
            );
            return;
        };
        pool.record_logging_notification(&self.server, &params);
    }
}

impl McpClientPool {
    /// 记录一条请求 `_meta` 声明的日志级别（逐 server）。
    ///
    /// Peri 不主动在请求 `_meta` 写 `io.modelcontextprotocol/logLevel`；本入口只观察
    /// 调用方已声明的值，供 `notifications/message` 门控使用。
    #[allow(deprecated)]
    pub(crate) fn declare_log_level(&self, server: &str, level: LoggingLevel) {
        self.declared_log_levels
            .write()
            .insert(server.to_string(), level);
    }

    /// 自定义通知入口（`ClientHandler` 回调，同步、不得 await）。
    pub(crate) fn record_custom_notification(
        &self,
        server: &str,
        method: &str,
        params: Option<&Value>,
    ) {
        let payload = params.map(Value::to_string).unwrap_or_default();
        self.push_pending_notification(server, method, payload);
    }

    /// `notifications/message` 入口：仅在请求 `_meta` 已声明 logLevel 时进入缓冲。
    #[allow(deprecated)]
    pub(crate) fn record_logging_notification(
        &self,
        server: &str,
        params: &LoggingMessageNotificationParam,
    ) {
        let Some(declared) = self.declared_log_levels.read().get(server).copied() else {
            tracing::debug!(
                server,
                level = ?params.level,
                logger = ?params.logger,
                "MCP notifications/message 忽略：请求 _meta 未声明 logLevel"
            );
            return;
        };
        tracing::debug!(
            server,
            declared = ?declared,
            level = ?params.level,
            "MCP notifications/message 接收"
        );
        let payload = json!({
            "level": params.level,
            "logger": params.logger,
            "data": params.data,
        });
        self.push_pending_notification(server, LOGGING_MESSAGE_METHOD, payload.to_string());
    }

    /// 同步写入部署级缓冲；ACP 会话级连接只留 debug，不进入共享面。
    fn push_pending_notification(&self, server: &str, method: &str, payload: String) {
        if self.acp_owners.read().contains_key(server) {
            tracing::debug!(
                server,
                method,
                "MCP 通知跳过：会话级 ACP 连接不进入部署级缓冲"
            );
            return;
        }
        let server = truncate_utf8(server.to_string(), MAX_NOTIFICATION_NAME_BYTES);
        let method = truncate_utf8(method.to_string(), MAX_NOTIFICATION_NAME_BYTES);
        let payload = truncate_utf8(payload, MAX_NOTIFICATION_PAYLOAD_BYTES);
        let mut pending = self.pending_notifications.lock();
        if let Some(existing) = pending
            .iter_mut()
            .find(|entry| entry.server == server && entry.method == method)
        {
            existing.payload = payload;
            existing.occurrences = existing.occurrences.saturating_add(1);
            return;
        }
        if pending.len() >= MAX_PENDING_NOTIFICATIONS {
            drop(pending);
            tracing::warn!(
                server = %server,
                method = %method,
                capacity = MAX_PENDING_NOTIFICATIONS,
                "MCP 通知缓冲已满，丢弃新通知"
            );
            return;
        }
        pending.push(PendingMcpNotification {
            server,
            method,
            payload,
            occurrences: 1,
        });
    }

    /// 取出并清空待投递通知（`before_model` 与连接状态变化同点位调用）。
    pub(crate) fn drain_pending_notifications(&self) -> Vec<PendingMcpNotification> {
        std::mem::take(&mut *self.pending_notifications.lock())
    }
}

/// 通知 → canonical reminder（Info、不唤醒模型、仅 TUI / 诊断受众）。
pub(crate) fn pending_notification_reminder(
    notification: &PendingMcpNotification,
) -> TrustedSystemReminder {
    let mut body = format!(
        "MCP 通知：server={}, method={}, count={}",
        notification.server, notification.method, notification.occurrences
    );
    if !notification.payload.is_empty() {
        body.push('\n');
        body.push_str(&notification.payload);
    }
    let summary = format!("MCP 通知: {}/{}", notification.server, notification.method);
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Diagnostic,
            source: ReminderSource("mcp".into()),
            kind: "custom_notification".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![
                ReminderAudience::Tui,
                ReminderAudience::Diagnostics,
            ]),
            body,
            summary: Some(summary),
            metadata: json!({
                "server": notification.server,
                "method": notification.method,
                "count": notification.occurrences,
            }),
        })
        .expect("MCP custom notification reminder mapping must be valid")
}

/// 按 UTF-8 边界截断，并追加固定标记；总长不超过 `max_bytes`。
fn truncate_utf8(mut text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let budget = max_bytes.saturating_sub(TRUNCATION_MARKER.len());
    let mut cut = budget;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push_str(TRUNCATION_MARKER);
    text
}

#[cfg(test)]
#[path = "notification_tap_test.rs"]
mod tests;
