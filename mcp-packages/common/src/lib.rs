//! Shared behavior used by the independently packaged builtin MCP instances.

pub mod agent_definition;
pub mod failure;
mod helpers;
mod numeric;
pub mod process_env;
pub mod result_mapping;

pub use helpers::{invoke_tool_call, list_tools_of, rmcp_tool_from_base, server_info};
pub use numeric::parse_optional_u64;
