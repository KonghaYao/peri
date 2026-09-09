//! Fork a session: create a new thread and copy messages from source.
//!
//! 存储访问经 [`Controller::sessions`]（ARC-BOUNDARY-001 方向）。

use anyhow::{Context, Result};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_controller::Controller;

/// Fork a session by creating a new thread and copying source messages.
///
/// Returns `Ok((new_thread_id, copied_messages))` on success.
/// The caller is responsible for inserting the new session into its session map.
pub async fn fork_session(
    controller: &Controller,
    source_thread_id: &str,
    source_payloads: &[PersistedPayload],
    cwd: &str,
) -> Result<(String, Vec<PersistedPayload>)> {
    let meta = ThreadMeta::new(cwd);
    let store = controller.sessions();
    let new_thread_id = store
        .create_thread(meta)
        .await
        .context("Thread creation failed")?;

    if !source_payloads.is_empty() {
        if let Err(copy_error) = store
            .append_payloads(&ThreadId::from(new_thread_id.clone()), source_payloads)
            .await
        {
            if store.delete_thread(&new_thread_id).await.is_err() {
                tracing::error!(
                    event = "session_fork_persistence_inconsistency",
                    source_thread_id,
                    new_thread_id = %new_thread_id,
                    copy_failed = true,
                    compensation_failed = true,
                    classification = "persistence_inconsistency",
                    "session fork persistence inconsistency"
                );
                anyhow::bail!(
                    "Session fork failed due to a persistence inconsistency; manual recovery may be required"
                );
            }

            return Err(copy_error).context("Failed to copy session payloads");
        }
    }

    tracing::info!(
        source = %source_thread_id,
        new = %new_thread_id,
        msg_count = source_payloads.len(),
        "Session forked"
    );

    Ok((new_thread_id, source_payloads.to_vec()))
}
