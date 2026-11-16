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
#[cfg(not(target_os = "emscripten"))]
#[cfg(test)]
pub(crate) use subscription_events::is_git_watch_resource;
#[cfg(target_os = "emscripten")]
#[cfg(test)]
pub(crate) use subscription_events::is_git_watch_resource;
#[cfg(test)]
use subscription_events::notification_message_kind;
pub(crate) use subscription_events::setup_subscription;
