use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use serde::Deserialize;
use serde_json::Value;
use std::fmt;
use std::fmt::{Debug, Formatter};

use super::web_common;
use super::web_common::WEB_CREDIBILITY_WARNING;
use peri_agent::agent::async_tasks;
use peri_mcp_common::shell;

#[cfg(test)]
#[path = "web_fetch_test.rs"]
mod web_fetch_test;

#[path = "web_fetch/web_fetch_tool.rs"]
mod web_fetch_tool;
#[cfg(test)]
use web_fetch_tool::truncate_content;
pub use web_fetch_tool::WebFetchTool;
#[cfg(test)]
use web_fetch_tool::MAX_CONTENT_CHARS;
#[cfg(test)]
use web_fetch_tool::MAX_CONTENT_LINES;
