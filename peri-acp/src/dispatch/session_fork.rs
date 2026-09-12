//! Fork a session: create a new thread and copy messages from source.
//!
//! 存储访问经 [`Controller::sessions`]（ARC-BOUNDARY-001 方向）。

use std::collections::HashMap;

use anyhow::{Context, Result};
use peri_acp_types::messages::MessageId;
use peri_acp_types::store::{MessageFlags, PersistedPayload, ThreadStore};
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_controller::Controller;

/// Clone one logical payload into an independently owned fork row.
///
/// SQLite stores `message_id` as a database-wide primary key, so preserving the
/// source ID would make `INSERT OR IGNORE` silently leave the fork without rows.
fn clone_payload_for_fork(payload: &PersistedPayload) -> PersistedPayload {
    let id = MessageId::new();
    match payload {
        PersistedPayload::Message(message) => {
            PersistedPayload::Message(message.clone().with_message_id(id))
        }
        PersistedPayload::SystemReminder { reminder, .. } => PersistedPayload::SystemReminder {
            id,
            reminder: reminder.clone(),
        },
    }
}

fn clone_flags_for_fork(
    mut flags: MessageFlags,
    source_id: MessageId,
    forked_id: MessageId,
) -> MessageFlags {
    if let Some(projection) = flags.projection.as_mut() {
        for entry in &mut projection.entries {
            if entry.message_id == source_id {
                entry.message_id = forked_id;
            }
        }
    }
    flags
}

async fn cleanup_failed_fork(
    store: &dyn ThreadStore,
    source_thread_id: &str,
    new_thread_id: &ThreadId,
) -> Result<()> {
    if store.delete_thread(new_thread_id).await.is_err() {
        tracing::error!(
            event = "session_fork_persistence_inconsistency",
            source_thread_id,
            new_thread_id,
            copy_failed = true,
            compensation_failed = true,
            classification = "persistence_inconsistency",
            "session fork persistence inconsistency"
        );
        anyhow::bail!(
            "Session fork failed due to a persistence inconsistency; manual recovery may be required"
        );
    }
    Ok(())
}

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
    let source_thread_id = ThreadId::from(source_thread_id.to_string());
    let source_flags = store
        .load_message_flags(&source_thread_id)
        .await
        .context("Failed to load source compact flags")?;
    let new_thread_id = store
        .create_thread(meta)
        .await
        .context("Thread creation failed")?;

    let copied_payloads = source_payloads
        .iter()
        .map(clone_payload_for_fork)
        .collect::<Vec<_>>();
    let copied_flags = source_payloads
        .iter()
        .zip(&copied_payloads)
        .filter_map(|(source, copied)| {
            source_flags.get(&source.id()).cloned().map(|flags| {
                (
                    copied.id(),
                    clone_flags_for_fork(flags, source.id(), copied.id()),
                )
            })
        })
        .collect::<HashMap<_, _>>();

    if !copied_payloads.is_empty() {
        if let Err(copy_error) = store
            .append_payloads(&new_thread_id, &copied_payloads)
            .await
        {
            cleanup_failed_fork(store.as_ref(), source_thread_id.as_str(), &new_thread_id).await?;
            return Err(copy_error).context("Failed to copy session payloads");
        }
    }

    for (message_id, flags) in copied_flags {
        if let Err(copy_error) = store.update_message_flags(&message_id, &flags).await {
            cleanup_failed_fork(store.as_ref(), source_thread_id.as_str(), &new_thread_id).await?;
            return Err(copy_error).context("Failed to copy session compact flags");
        }
    }

    tracing::info!(
        source = %source_thread_id,
        new = %new_thread_id,
        msg_count = copied_payloads.len(),
        "Session forked"
    );

    Ok((new_thread_id, copied_payloads))
}

/// Fork a bound, idle source while the caller holds its lifecycle gate and owner.
pub(crate) async fn fork_bound_session(
    controller: &Controller,
    source_thread_id: &str,
    workspace: &peri_acp_types::workspace::ResolvedWorkspace,
) -> Result<(
    String,
    Vec<PersistedPayload>,
    std::sync::Arc<dyn peri_acp_types::workspace::SessionExecutionLease>,
)> {
    let store = controller.sessions();
    let source_id = source_thread_id.to_owned();
    let payloads = store.load_payloads(&source_id).await?;
    // A source snapshot must finish every assistant tool invocation before it can
    // be copied into an independently executable history.
    let mut pending = std::collections::HashSet::new();
    for message in payloads.iter().filter_map(PersistedPayload::as_message) {
        for call in message.tool_calls() {
            pending.insert(call.id.clone());
        }
        if let peri_acp_types::messages::BaseMessage::Tool { tool_call_id, .. } = message {
            pending.remove(tool_call_id);
        }
    }
    anyhow::ensure!(
        pending.is_empty(),
        "Cannot fork history with incomplete tool calls"
    );
    let flags = store.load_message_flags(&source_id).await?;
    let cwd = workspace
        .cwd
        .to_str()
        .context("Execution directory is not UTF-8")?;
    let id = store
        .create_bound_thread(ThreadMeta::new(cwd), workspace)
        .await?;
    let lease = store.acquire_execution_lease(&id).await?;
    let copied: Vec<_> = payloads.iter().map(clone_payload_for_fork).collect();
    let result = async {
        store.append_payloads(&id, &copied).await?;
        for (source, copied) in payloads.iter().zip(&copied) {
            if let Some(flags) = flags.get(&source.id()) {
                store
                    .update_message_flags(
                        &copied.id(),
                        &clone_flags_for_fork(flags.clone(), source.id(), copied.id()),
                    )
                    .await?;
            }
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        cleanup_failed_fork(store.as_ref(), source_thread_id, &id).await?;
        lease.mark_clean().await?;
        return Err(error);
    }
    Ok((id, copied, lease))
}
