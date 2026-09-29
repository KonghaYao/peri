//! Workspace tools and their builtin MCP handler.

pub mod filesystem;
mod fuzzy;
mod git_watch;
mod input;
pub mod resources;
mod shell_hints;
pub mod terminal;
mod workspace;

pub use git_watch::{GitWatchState, GIT_REF_RESOURCE_URI};
pub use input::WorkspaceInstanceInput;
pub use peri_acp_types::workspace_resources::ResourceScope;
pub use resources::{ResourceBudget, ResourceRoot, WorkspaceResourcesInput};
pub use workspace::WorkspaceMcpServer;
