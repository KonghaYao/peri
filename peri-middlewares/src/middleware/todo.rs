use peri_agent::middleware::capabilities as hook_state;
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, SYSTEM_REMINDER_VERSION,
};
use peri_agent::error::AgentError;
use peri_agent::error::AgentResult;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::session::MessageSource;
use peri_agent::tools::BaseTool;
use serde_json::json;
use tokio::sync::{mpsc, Mutex};

use crate::completion_reminder::CompletionReminder;
use crate::tools::todo::{render_todo_status, TodoItem, TodoState, TodoStatus, TodoWriteTool};

#[cfg(test)]
#[path = "todo_test.rs"]
mod tests;

#[path = "todo/todo_middleware.rs"]
mod todo_middleware;
pub use todo_middleware::TodoMiddleware;
