//! Fork a session: create a new thread and copy messages from source.
//!
//! 存储访问经 [`Controller::sessions`]（ARC-BOUNDARY-001 方向）。

use anyhow::{Context, Result};
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_controller::Controller;

/// Fork a session by creating a new thread and copying source messages.
///
/// Returns `Ok((new_thread_id, copied_messages))` on success.
/// The caller is responsible for inserting the new session into its session map.
pub async fn fork_session(
    controller: &Controller,
    source_thread_id: &str,
    source_messages: &[BaseMessage],
    cwd: &str,
) -> Result<(String, Vec<BaseMessage>)> {
    let meta = ThreadMeta::new(cwd);
    let store = controller.sessions();
    let new_thread_id = store
        .create_thread(meta)
        .await
        .context("Thread creation failed")?;

    if !source_messages.is_empty() {
        if let Err(e) = store
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
