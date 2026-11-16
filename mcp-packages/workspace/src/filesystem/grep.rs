use peri_mcp_common::failure::ToolFailure;
use std::cell::Cell;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;

use grep::regex::RegexMatcherBuilder;
use grep::searcher::BinaryDetection;
use grep::searcher::SearcherBuilder;
use ignore::WalkBuilder;
use peri_agent::tools::BaseTool;
use peri_time;
use serde_json::Value;
use std::time::Duration;

use super::grep_args::GrepInput;
use super::grep_args::OutputMode;
use super::grep_args::ParsedArgs;
use super::grep_format::SearchSink;
use peri_mcp_common::shell;

#[cfg(test)]
#[path = "grep_test.rs"]
mod tests;

#[cfg(test)]
#[path = "search_reporting_test.rs"]
mod search_reporting_tests;

#[path = "grep/grep_tool.rs"]
mod grep_tool;
#[cfg(test)]
use grep_tool::execute_search;
pub use grep_tool::GrepTool;
pub(crate) use grep_tool::MAX_LINE_BYTES;
pub(crate) use grep_tool::MAX_OUTPUT_BYTES;
#[cfg(test)]
use grep_tool::SEARCH_THREADS_MAX;
#[cfg(test)]
pub(crate) use grep_tool::SEARCH_TIMEOUT;
