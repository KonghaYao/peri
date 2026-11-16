use std::fmt;
use std::fmt::{Debug, Formatter};
use std::time::Duration;

use async_trait::async_trait;
use peri_agent::agent::async_tasks;
use peri_agent::tools::BaseTool;
use serde::Deserialize;
use serde_json::Value;

use super::web_common;
use super::web_common::WEB_CREDIBILITY_WARNING;

// WebSearch / WebFetch 两个工具的用例（sub-plan F §6.4 第 2 步：挂载点从
// `middleware/web.rs` 迁到本文件——该文件由 I-03 在 W3 删除；16 个用例，
// 过滤器 `middleware::web_search::tests`）。
#[cfg(test)]
#[path = "web_test.rs"]
mod tests;

#[path = "web_search/web_search_tool.rs"]
mod web_search_tool;
#[cfg(test)]
pub(crate) use web_search_tool::format_search_results;
#[cfg(test)]
pub(crate) use web_search_tool::SearchResult;
pub use web_search_tool::WebSearchTool;
#[cfg(test)]
use web_search_tool::MAX_RESPONSE_BYTES;
#[cfg(test)]
use web_search_tool::REQUEST_TIMEOUT;
