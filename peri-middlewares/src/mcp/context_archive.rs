//! Session-local model tool adapter. All recovery data crosses tools/call.
use async_trait::async_trait;
use peri_acp_types::context_archive::ArchiveError;
use peri_agent::context_archive::ContextArchiveReader;
use peri_agent::tools::BaseTool;
use peri_agent::tools::ToolContext;
use peri_mcp_common::context_archive::{self, ContextArchiveMcpServer};
use rmcp;
use rmcp::model::CallToolRequestParams;
use rmcp::model::ProtocolVersion;
use rmcp::ClientLifecycleMode;
use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
#[path = "context_archive_test.rs"]
mod tests;

#[path = "context_archive/read_context_archive_tool.rs"]
mod read_context_archive_tool;
pub use read_context_archive_tool::ReadContextArchiveTool;
