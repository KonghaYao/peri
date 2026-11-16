//! ToolSearchMiddleware — 注册元工具并注入延迟工具列表到 system prompt

use peri_agent::middleware::capabilities as hook_state;
use std::collections::BTreeMap;
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::error::AgentResult;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::tools::BaseTool;

use super::declaration;
use super::execute_tool::ExecuteExtraTool;
use super::search_tool::SearchExtraTools;
use super::tool_index::ToolSearchIndex;

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;

#[path = "middleware/tool_search_middleware.rs"]
mod tool_search_middleware;
pub use tool_search_middleware::ToolSearchMiddleware;
