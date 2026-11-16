use peri_agent::tools::BaseTool;
use peri_mcp_common::failure::ToolFailure;
use serde_json::Value;

use super::transaction;
use super::transaction::CommitError;
use super::transaction::SENTINEL_REJECTION;

#[cfg(test)]
#[path = "edit_test.rs"]
mod tests;

#[path = "edit/edit_file_tool.rs"]
mod edit_file_tool;
pub use edit_file_tool::EditFileTool;
