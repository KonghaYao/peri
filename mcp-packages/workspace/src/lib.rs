//! Workspace tools and their builtin MCP handler.

pub mod filesystem;
mod fuzzy;
mod input;
mod shell_hints;
pub mod terminal;
mod workspace;

pub use input::WorkspaceInstanceInput;
pub use workspace::WorkspaceMcpServer;
