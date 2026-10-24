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
pub mod rewind;
pub mod rewind_candidates;
pub mod session_fork;
pub mod session_load;
pub mod session_replay;

pub use init::build_initialize_response;
pub use list_sessions::list_sessions_as_info;
pub use prompt::{extract_prompt_params, handle_prompt};
pub use rewind::{rewind_execute, rewind_preview};
pub use rewind_candidates::{rewind_candidate_response, rewind_candidates, REWIND_CANDIDATE_LIMIT};
pub(crate) use session_fork::{fork_bound_session, load_fork_source};
pub use session_load::{
    history_page_error, initial_history_cursor, load_active_session_payloads,
    load_canonical_session_payloads, load_history_page, InitialHistoryCursor,
};
pub use session_replay::{
    entry_id, project_payload_updates, reminder_visible_to_client,
    replay_canonical_history_streamed, replay_persisted_session_history, replay_session_history,
    write_entry_marks, EntryMarks, LoadGeneration, ProjectedUpdate, ReplayError, ReplayMarks,
    ReplaySender,
};

#[cfg(test)]
mod commands_test;
#[cfg(test)]
mod session_fork_test;
