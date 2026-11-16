use std::path::Path;

use peri_agent::tools::BaseTool;
use serde_json::{json, Value};

use super::client::ArtifactClient;

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;

#[path = "tool/artifact_tool.rs"]
mod artifact_tool;
pub use artifact_tool::ArtifactTool;
