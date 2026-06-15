//! Fork a session: create a new thread and copy messages from source.

use anyhow::{Context, Result};
use peri_agent::{
    messages::BaseMessage,
    thread::{ThreadId, ThreadMeta, ThreadStore},
};

/// Fork a session by creating a new thread and copying source messages.
///
/// Returns `Ok((new_thread_id, copied_messages))` on success.
/// The caller is responsible for inserting the new session into its session map.
pub async fn fork_session(
    thread_store: &dyn ThreadStore,
    source_thread_id: &str,
    source_messages: &[BaseMessage],
    cwd: &str,
) -> Result<(String, Vec<BaseMessage>)> {
    let meta = ThreadMeta::new(cwd);
    let new_thread_id = thread_store
        .create_thread(meta)
        .await
        .context("Thread creation failed")?;

    if !source_messages.is_empty() {
        if let Err(e) = thread_store
            .append_messages(&ThreadId::from(new_thread_id.clone()), source_messages)
            .await
        {
            tracing::warn!(error = %e, "session/fork: failed to copy messages to new thread");
        }
    }

    tracing::info!(
        source = %source_thread_id,
        new = %new_thread_id,
        msg_count = source_messages.len(),
        "Session forked"
    );

    Ok((new_thread_id, source_messages.to_vec()))
}
