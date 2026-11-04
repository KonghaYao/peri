//! A settled tool exchange is one owned, FIFO writer transaction, published after ACK.

use std::sync::{atomic::Ordering, Arc};

use anyhow::anyhow;
use parking_lot::RwLock;

use super::{history, BaseMessage, MessageTranscript, PersistOp, PersistedPayload};

/// Owned transaction passed only by the transcript's checked admission path.
#[derive(Debug)]
pub struct AtomicBatchCommit {
    pub(super) payloads: Vec<PersistedPayload>,
    pub(super) ack: tokio::sync::oneshot::Sender<anyhow::Result<Vec<PersistedPayload>>>,
}

impl MessageTranscript {
    /// Commit a complete exchange without copying it into the bounded append backlog.
    pub(crate) async fn commit_tool_exchange(
        transcript: &Arc<RwLock<Self>>,
        messages: Vec<BaseMessage>,
    ) -> anyhow::Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let payloads: Vec<_> = messages
            .into_iter()
            .map(PersistedPayload::Message)
            .collect();
        let (writer, revision, state, previous) = {
            let mut current = transcript.write();
            if let Some(error) = current.persistence_failure() {
                return Err(anyhow!("transcript persistence failed: {error}"));
            }
            history::ensure_distinct_ids(&payloads, |id| current.id_index.contains_key(&id))?;
            if current.staged.is_some() {
                return Err(anyhow!("cannot commit an exchange over staged messages"));
            }
            let Some(writer) = current.persist_tx_handle() else {
                for payload in payloads {
                    current.push_loaded_payload(payload);
                }
                return Ok(());
            };
            let state = current.compaction_commit_state.clone();
            let previous = state.0.load(Ordering::SeqCst);
            if previous == 1
                || state
                    .0
                    .compare_exchange(previous, 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
            {
                return Err(anyhow!(
                    "a transcript commit is already pending; reload the session"
                ));
            }
            (writer, current.view_revision(), state, previous)
        };

        // The same writer drains earlier appends before the indivisible transaction.
        // Pending survives a dropped caller, including the ACK-to-publication window;
        // the owned writer still finishes, and the executor must cold-reload that view.
        let (ack, receive) = tokio::sync::oneshot::channel();
        writer
            .send(PersistOp::CommitBatch(AtomicBatchCommit { payloads, ack }))
            .map_err(|_| anyhow!("transcript persistence writer closed before exchange commit"))?;
        let payloads = receive.await.map_err(|_| {
            anyhow!("transcript persistence writer closed during exchange commit")
        })??;
        let mut current = transcript.write();
        if current.view_revision() != revision || current.staged.is_some() {
            return Err(anyhow!(
                "transcript changed during exchange commit; reload the session"
            ));
        }
        for payload in payloads {
            current.push_loaded_payload(payload);
        }
        state.restore(previous);
        Ok(())
    }
}

#[cfg(test)]
#[path = "atomic_batch_test.rs"]
mod tests;
