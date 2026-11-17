use peri_agent::tools::BaseTool;
use peri_mcp_common::failure::ToolFailure;
use serde_json::Value;
use std::sync::{Arc, Mutex};

use super::draft;
use super::draft::DraftAccessError;
use super::draft::DraftStore;

use super::transaction;
use super::transaction::CommitError;
use super::transaction::SENTINEL_REJECTION;

#[cfg(test)]
#[path = "write_test.rs"]
mod tests;

#[cfg(test)]
#[path = "write_draft_test.rs"]
mod draft_tests;

#[path = "write/write_file_tool.rs"]
mod write_file_tool;
pub use write_file_tool::WriteFileTool;
