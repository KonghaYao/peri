use peri_mcp_common::failure::ToolFailure;
use std::path::Path;

use chrono::Utc;
use peri_agent::tools::BaseTool;
use serde_json::Value;
use tracing::debug;

use peri_mcp_common::shell;

#[cfg(test)]
#[path = "folder_test.rs"]
mod tests;

#[path = "folder/folder_operations_tool.rs"]
mod folder_operations_tool;
#[cfg(test)]
use folder_operations_tool::deep_scan_folder;
pub use folder_operations_tool::list_folder;
pub use folder_operations_tool::FolderOperationsTool;
