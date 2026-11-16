use peri_acp_types::tool_outcome::RawToolResult;
use peri_agent::tools::{BaseTool, ToolOutput};
use peri_mcp_common::failure::ToolFailure;
use serde_json::Value;

use super::folder;

use peri_agent::agent::async_tasks;

#[cfg(test)]
#[path = "read_test.rs"]
mod tests;

#[path = "read/read_file_tool.rs"]
mod read_file_tool;
pub use read_file_tool::ReadFileTool;
#[cfg(test)]
use read_file_tool::MAX_CHARS_PER_LINE;
#[cfg(test)]
use read_file_tool::MAX_FILE_SIZE;
#[cfg(test)]
use read_file_tool::READ_IMAGE_MAX_BYTES;
