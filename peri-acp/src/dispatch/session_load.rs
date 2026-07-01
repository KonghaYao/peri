//! Load session context from ThreadStore (includes ancestor chain snapshots).

use peri_agent::{
    messages::BaseMessage,
    thread::{ThreadId, ThreadStore},
};
use serde_json::json;

use crate::event::router::ViewMapper;

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

/// Build the `peri/unstable-event` payload for a "view-commit" event
/// from the loaded session history.
///
/// Converts `history` to `Vec<ViewModel>` via a fresh `ViewMapperImpl`
/// and returns a `{ sessionId, event, data }` JSON payload suitable for
/// sending through the transport's `send_notification()` method.
///
/// Returns `None` if the history is empty.
pub fn build_session_view_commit_payload(
    session_id: &str,
    history: &[BaseMessage],
) -> Option<serde_json::Value> {
    if history.is_empty() {
        return None;
    }
    let mut vm = crate::event::ViewMapperImpl::new();
    let vms = vm.convert(history);
    Some(json!({
        "sessionId": session_id,
        "event": "view-commit",
        "data": { "view_models": vms },
    }))
}
