//! Session-local Dynamic MCP 通知投影与 weak sink 绑定。

use std::sync::Arc;

use peri_acp_types::dynamic_mcp::{DynamicMcpInstanceKey, DynamicMcpNotification};
use peri_acp_types::ports::DynamicMcpNotificationSinkPort;
use peri_acp_types::session::{MessageKind, MessageSource};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};

use super::SessionManager;

pub struct SessionDynamicMcpNotificationSink {
    session_id: String,
    inbox: std::sync::Weak<peri_acp_types::session::SessionInbox>,
}

impl SessionDynamicMcpNotificationSink {
    fn reminder(
        kind: &str,
        severity: ReminderSeverity,
        body: String,
        summary: String,
        metadata: serde_json::Value,
    ) -> TrustedSystemReminder {
        TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Lifecycle,
                source: ReminderSource("dynamic_mcp".into()),
                kind: kind.into(),
                severity,
                delivery: ReminderDelivery::Configurable,
                audiences: ReminderAudiences(vec![
                    ReminderAudience::Model,
                    ReminderAudience::Tui,
                    ReminderAudience::Diagnostics,
                ]),
                body,
                summary: Some(summary),
                metadata,
            })
            .expect("dynamic MCP reminder mapping must be valid")
    }
}

impl DynamicMcpNotificationSinkPort for SessionDynamicMcpNotificationSink {
    fn notify(&self, notification: DynamicMcpNotification) -> bool {
        if !self.accepts(&notification.instance_key) {
            return false;
        }
        let Some(inbox) = self.inbox.upgrade() else {
            return false;
        };
        let reminder = Self::reminder(
            "lifecycle_changed",
            ReminderSeverity::Info,
            notification.safe_summary.clone(),
            notification.safe_summary,
            serde_json::json!({ "instance_key": notification.instance_key }),
        );
        inbox.handle().push_system_reminder(
            MessageKind::Info,
            MessageSource::DynamicMcpNotification,
            reminder,
        );
        true
    }

    fn notify_authorization_needed(
        &self,
        instance: &DynamicMcpInstanceKey,
        flow_id: &str,
        authorization_url: &str,
    ) -> bool {
        if !self.accepts(instance) {
            return false;
        }
        let Some(inbox) = self.inbox.upgrade() else {
            return false;
        };
        let body = format!(
            "Dynamic MCP {} requires OAuth authorization for flow {}: {}",
            instance.logical.server_name, flow_id, authorization_url
        );
        let reminder = Self::reminder(
            "oauth_authorization_required",
            ReminderSeverity::Warning,
            body,
            format!(
                "Dynamic MCP {} requires OAuth authorization",
                instance.logical.server_name
            ),
            serde_json::json!({
                "server_name": instance.logical.server_name,
                "flow_id": flow_id,
            }),
        );
        inbox.handle().push_system_reminder(
            MessageKind::Info,
            MessageSource::DynamicMcpNotification,
            reminder,
        );
        true
    }

    fn accepts(&self, instance: &DynamicMcpInstanceKey) -> bool {
        instance.logical.session_id == self.session_id && self.inbox.upgrade().is_some()
    }
}

impl SessionManager {
    pub fn dynamic_mcp_instance_is_current(&self, instance: &DynamicMcpInstanceKey) -> bool {
        self.inner
            .dynamic_mcp
            .as_ref()
            .is_some_and(|deployment| deployment.accepts_instance(instance))
    }

    pub fn dynamic_mcp_notifications_for(&self, session_id: &str) -> bool {
        let Some(deployment) = self.inner.dynamic_mcp.as_ref() else {
            return false;
        };
        let Some(inbox) = self.session_inbox_for(session_id) else {
            return false;
        };
        let sink = Arc::new(SessionDynamicMcpNotificationSink {
            session_id: session_id.to_string(),
            inbox: Arc::downgrade(&inbox),
        });
        let erased: Arc<dyn DynamicMcpNotificationSinkPort> = sink.clone();
        if !deployment.bind_notification_sink(session_id, Arc::downgrade(&erased)) {
            return false;
        }
        let Some(mut session) = self.inner.sessions.get_mut(session_id) else {
            return false;
        };
        session.dynamic_mcp_notifications = Some(sink);
        true
    }
}

#[cfg(test)]
#[path = "dynamic_mcp_test.rs"]
mod tests;
