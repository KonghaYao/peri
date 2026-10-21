//! Confirm prefix deletion and archive dependency reconciliation before publishing memory.
use std::sync::atomic::Ordering;

use anyhow::anyhow;
use peri_acp_types::session_resources::{MutationOutcome, RewindBoundary, RewindOutcome};

use super::{history, BaseMessage, MessageId, MessageTranscript};

impl MessageTranscript {
    /// Keep the target and revoke incomplete archive groups in the same durable operation.
    /// Known failures preserve the old view; an unacknowledged commit requires reloading.
    pub async fn rewind_to(&mut self, id: MessageId) -> anyhow::Result<()> {
        if self.compaction_commit_state.is_uncertain() {
            return Err(anyhow!(
                "rewind requires reload after an uncertain history commit"
            ));
        }
        let ids: Vec<_> = self.entries.iter().map(|entry| entry.id()).collect();
        let keep_len = history::rewind_keep_len(&ids, RewindBoundary::KeepThrough(id))?;
        if keep_len <= self.ancestor_len {
            return Err(anyhow!("cannot rewind into ancestor region"));
        }
        let retained: Vec<_> = self.entries[self.ancestor_len..keep_len]
            .iter()
            .map(|entry| {
                (
                    entry.id(),
                    self.flags(entry.id()),
                    matches!(entry.as_message(), Some(BaseMessage::System { .. })),
                )
            })
            .collect();
        let updates = match (&self.session_resources, &self.thread_id) {
            (Some(store), Some(thread_id)) => {
                let store = store.clone();
                let thread_id = thread_id.clone();
                let previous = self.compaction_commit_state.0.load(Ordering::SeqCst);
                self.compaction_commit_state.mark_pending();
                self.flush_persistence().await?;
                match store
                    .rewind_history(&thread_id, RewindBoundary::KeepThrough(id), &[])
                    .await
                {
                    Ok(RewindOutcome::Applied) => {}
                    Ok(RewindOutcome::BoundaryMissing) => {
                        self.compaction_commit_state.restore(previous);
                        return Err(anyhow!("rewind boundary disappeared before commit"));
                    }
                    Err(error) => {
                        if error.effect() == MutationOutcome::NotApplied {
                            self.compaction_commit_state.restore(previous);
                        }
                        tracing::error!(%error, "transcript rewind commit failed");
                        return Err(error.into());
                    }
                }
                // The store reconciles against complete canonical metadata, including catalogs
                // that Full has released from memory. Read their resulting flags, not a hot subset.
                let snapshot = store.load_session_snapshot(&thread_id).await?;
                let updates = retained
                    .iter()
                    .map(|(id, _, _)| (*id, snapshot.flags.get(id).cloned().unwrap_or_default()))
                    .collect();
                self.compaction_commit_state.restore(previous);
                updates
            }
            (None, None) => history::rewind_projection_updates(&retained, &[]),
            _ => return Err(anyhow!("rewind persistence handles are incomplete")),
        };
        self.staged = None;
        self.entries.truncate(keep_len);
        for id in &ids[keep_len..] {
            self.id_index.remove(id);
            self.flags.remove(id);
        }
        history::apply_flag_updates(&mut self.flags, &updates);
        self.bump_view_revision();
        Ok(())
    }
}
