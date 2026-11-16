use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::monitor;
use peri_acp_types::tasks::{TaskManager, TaskRegistrationTicket};
use rmcp::model::DetailedTask;
use rmcp::model::GetTaskParams;
use rmcp::model::MetaObject;
use rmcp::model::TaskPayload;
use rmcp::service::Peer;
use rmcp::service::RoleClient;

use super::super::monitor_alert;
use super::super::monitor_alert::MonitorAlertAction;
use super::super::monitor_alert::MonitorAlertState;
use super::super::McpClientPool;

#[path = "subscription_task_binding.rs"]
mod binding;

#[cfg(test)]
#[path = "subscription_tasks_test.rs"]
mod tests;

#[cfg(test)]
#[path = "monitor_subscription_test.rs"]
mod monitor_subscription_tests;

#[path = "subscription_tasks/task_subscription.rs"]
mod task_subscription;
#[cfg(test)]
use task_subscription::lost_owner_retry_due;
#[cfg(test)]
use task_subscription::LOST_ABANDON_ATTEMPTS;
