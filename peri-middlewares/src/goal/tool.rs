//! Goal 工具 — 单一 deferred 工具，通过 action 参数分发。
//!
//! action: create / complete / block / get
//! complete 经 auxiliary_model LLM 二元验证。

use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::goal::GoalController;
use peri_agent::messages::BaseMessage;
use peri_agent::tools::{BaseTool, ToolContext};
use peri_model::{ModelMessage, ModelRequest, StopReason};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;

#[path = "tool/goal_tool.rs"]
mod goal_tool;
pub use goal_tool::GoalTool;
