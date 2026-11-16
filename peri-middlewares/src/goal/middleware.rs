//! GoalMiddleware — after_agent 钩子注入递增紧迫感 steering。
//!
//! goal active 且无后台任务或既有 stop block 时注入提示并设 block_continue，executor 自动续跑。
//! agent 必须调 goal(complete) 或 goal(block) 才能终止循环。
//!
//! 注入路径：通过 v2 MessageQueue push canonical Defer reminder（下一次 Receive 消费并唤醒续跑）。
//! 结构化 reminder 在模型投影边界编码为 Human role，不破坏 frozen_system_prompt。

use peri_agent::middleware::capabilities as hook_state;
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, SYSTEM_REMINDER_VERSION,
};
use peri_agent::error::AgentResult;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::session::MessageSource;
use serde_json::json;

use crate::completion_reminder::CompletionReminder;
use crate::goal::GoalTool;

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;

#[path = "middleware/goal_middleware.rs"]
mod goal_middleware;
pub use goal_middleware::GoalMiddleware;
