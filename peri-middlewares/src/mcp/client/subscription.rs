use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::mcp::{McpNotificationMessageKind, MCP_MESSAGE_KIND_META_KEY};
use peri_acp_types::plugin::McpSubscriptionsConfig;
use peri_acp_types::session::{InboxHandle, MessageKind, MessageSource};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource as CanonicalReminderSource, SystemReminder, TrustedSystemReminder,
    TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use rmcp::model::GetMeta;
use rmcp::model::NotificationMetaObject;
use rmcp::model::ServerNotification;
use rmcp::model::SubscriptionFilter;
use rmcp::service::Subscription;
use rmcp::service::SubscriptionEnd;
use serde_json::json;

use super::{McpClientPool, McpServiceWrapper, SUBSCRIPTION_LISTEN_TIMEOUT};

#[path = "subscription_tasks.rs"]
mod tasks;

#[cfg(test)]
#[path = "subscription_test.rs"]
mod tests;

#[path = "subscription/subscription_events.rs"]
mod subscription_events;
#[cfg(test)]
pub(crate) use subscription_events::build_subscription_filter;
#[cfg(test)]
pub(crate) use subscription_events::git_watch_reminder_from_resource;
#[cfg(test)]
pub(crate) use subscription_events::is_git_watch_resource;
#[cfg(test)]
use subscription_events::notification_message_kind;
pub(crate) use subscription_events::setup_subscription;

impl McpClientPool {
    /// 关闭 builtin 能力时不建立订阅；外部 server 的订阅不受影响。
    pub(crate) fn subscription_allowed(&self, server: &str) -> bool {
        let closed = self
            .builtin_instance_context()
            .map(|context| context.closed.clone())
            .unwrap_or_default();
        let configs = self.configs.read();
        let Some(config) = configs.get(server) else {
            return true;
        };
        if super::super::builtin::is_closed_source(server, config.source.as_ref(), &closed) {
            return false;
        }
        if !matches!(config.source.as_ref(), Some(peri_acp_types::plugin::ConfigSource::Builtin { instance }) if peri_acp_types::builtin_mcp::is_v5_server_name(instance))
        {
            return true;
        }
        config.subscriptions.as_ref().is_none_or(|subscriptions| {
            !super::super::builtin::visible_subscription_resources(
                server,
                &subscriptions.resources,
                &closed,
            )
            .is_empty()
        })
    }

    pub(crate) fn subscription_allowed_for_uri(&self, server: &str, uri: &str) -> bool {
        if !self.subscription_allowed(server) {
            return false;
        }
        let closed = self
            .builtin_instance_context()
            .map(|context| context.closed.clone())
            .unwrap_or_default();
        let is_builtin_v5 = self.configs.read().get(server).is_some_and(|config| {
            matches!(config.source.as_ref(), Some(peri_acp_types::plugin::ConfigSource::Builtin { instance }) if peri_acp_types::builtin_mcp::is_v5_server_name(instance))
        });
        !is_builtin_v5 || !super::super::builtin::is_resource_closed(uri, &closed)
    }

    pub(crate) fn filter_subscriptions(
        &self,
        server: &str,
        subscriptions: &McpSubscriptionsConfig,
    ) -> Option<McpSubscriptionsConfig> {
        let closed = self
            .builtin_instance_context()
            .map(|context| context.closed.clone())
            .unwrap_or_default();
        let is_builtin_v5 = self.configs.read().get(server).is_some_and(|config| {
            matches!(config.source.as_ref(), Some(peri_acp_types::plugin::ConfigSource::Builtin { instance }) if peri_acp_types::builtin_mcp::is_v5_server_name(instance))
        });
        let resources = if is_builtin_v5 {
            super::super::builtin::visible_subscription_resources(
                server,
                &subscriptions.resources,
                &closed,
            )
        } else {
            subscriptions.resources.clone()
        };
        (!resources.is_empty()
            || (!is_builtin_v5
                && (subscriptions.tools_list_changed
                    || subscriptions.prompts_list_changed
                    || subscriptions.resources_list_changed)))
            .then_some(McpSubscriptionsConfig {
                resources,
                tools_list_changed: subscriptions.tools_list_changed,
                prompts_list_changed: subscriptions.prompts_list_changed,
                resources_list_changed: subscriptions.resources_list_changed,
            })
    }
}
