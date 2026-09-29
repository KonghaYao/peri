//! Workspace tools and their builtin MCP handler.

pub mod filesystem;
mod input;
pub mod terminal;
mod workspace;

pub use input::WorkspaceInstanceInput;
pub use workspace::WorkspaceMcpServer;
