//! ACP method dispatch — shared business logic.
//!
//! Provides pure functions that implement ACP session lifecycle
//! operations. Both TUI (MpscTransport) and stdio transports call these
//! functions, keeping only JSON-RPC framing and session-state management
//! in their respective transport layers.

pub mod commands;
pub mod config_update;
pub mod execute_command;
pub mod init;
pub mod list_sessions;
pub mod prompt;
pub mod session_fork;
pub mod session_load;
pub mod session_replay;

pub use commands::build_available_commands;
pub use init::build_initialize_response;
pub use list_sessions::list_sessions_as_info;
pub use prompt::{extract_prompt_params, handle_prompt};
pub use session_fork::fork_session;
pub use session_load::{build_session_view_commit_payload, load_session_messages};
pub use session_replay::{replay_session_history, ReplayError, ReplaySender};

#[cfg(test)]
mod commands_test;
