//! Load session context from ThreadStore (includes ancestor chain snapshots).

use peri_agent::{
    messages::BaseMessage,
    thread::{ThreadId, ThreadStore},
};

/// Load complete context for a session thread including ancestor snapshots.
///
/// Uses [`ThreadStore::load_context`] which assembles the full message chain
/// (ancestor snapshots + own messages) with materialized caching.
/// Returns an empty `Vec` if the thread does not exist (with a warning log).
pub async fn load_session_messages(
    thread_store: &dyn ThreadStore,
    thread_id: &str,
) -> Vec<BaseMessage> {
    match thread_store
        .load_context(&ThreadId::from(thread_id.to_string()))
        .await
    {
        Ok(msgs) => msgs,
        Err(e) => {
            tracing::warn!(thread_id = %thread_id, error = %e, "session/load: thread not found, returning empty history");
            Vec::new()
        }
    }
}
