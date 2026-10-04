//! Shared behavior used by the independently packaged builtin MCP instances.

pub mod agent_definition;
pub mod failure;
mod helpers;
mod numeric;
pub mod process_env;
pub mod result_mapping;
pub mod task_scope;

pub use helpers::{invoke_tool_call, list_tools_of, rmcp_tool_from_base, server_info};
pub use numeric::parse_optional_u64;

pub mod shell;
mod shell_executor;
pub mod shell_output;

#[cfg(all(test, unix))]
mod shell_session_complete_test;

pub fn create_local_task_manager() -> peri_agent::agent::async_tasks::TaskManager {
    peri_agent::agent::async_tasks::TaskManager::with_shell_executor(std::sync::Arc::new(
        shell_executor::LocalShellExecutor,
    ))
}
