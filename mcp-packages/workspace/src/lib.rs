//! Workspace tools and their builtin MCP handler.

mod file_observation;
mod file_rewind;
pub mod filesystem;
mod fuzzy;
mod git_branch;
mod git_watch;
pub mod image;
mod input;
mod output_store;
pub mod resources;
mod shell_hints;
mod shell_tasks;
use peri_mcp_common::task_scope;
pub mod terminal;
mod workspace;

pub use git_watch::{GitWatchState, GIT_REF_RESOURCE_URI};
pub use input::WorkspaceInstanceInput;
pub use peri_acp_types::workspace_resources::ResourceScope;
pub use peri_mcp_common::task_scope::{
    ExecutionGeneration, TaskScopeAuthority, TaskScopeCapability, TASK_SCOPE_META_KEY,
};
pub use resources::{ResourceBudget, ResourceRoot, WorkspaceResourcesInput};
pub use workspace::WorkspaceMcpServer;

#[cfg(test)]
#[path = "workspace_rewind_wire_test.rs"]
mod workspace_rewind_wire_test;
