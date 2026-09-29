//! Workspace tools and their builtin MCP handler.

pub mod filesystem;
mod fuzzy;
mod git_watch;
mod input;
mod shell_hints;
pub mod terminal;
mod workspace;

pub use git_watch::{GitWatchState, GIT_REF_RESOURCE_URI};
pub use input::WorkspaceInstanceInput;
pub use workspace::WorkspaceMcpServer;
