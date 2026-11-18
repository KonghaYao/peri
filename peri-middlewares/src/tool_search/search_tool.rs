//! ToolSearch 元工具 — 搜索并发现延迟加载的工具

use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use serde_json::{json, Value};

use super::core_tools::direct_tools_description;
use super::core_tools::{SEARCH_EXTRA_TOOLS_NAME, TOOL_SEARCH_NAME};
use super::tool_index::ToolSearchIndex;

#[cfg(test)]
#[path = "search_tool_test.rs"]
mod tests;

#[path = "search_tool/search_extra_tools.rs"]
mod search_extra_tools;
pub use search_extra_tools::SearchExtraTools;
