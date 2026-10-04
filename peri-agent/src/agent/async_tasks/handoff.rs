//! Bounded wait and observable handoff for unsettled session tasks.
//!
//! A session that initiated background work waits for its terminal reminders so
//! the results are not lost. That wait must be bounded: after [`HANDOFF_MAX_WAIT`]
//! the session stops waiting and records the unsettled task identities (with the
//! scope owner that will reconcile them) into its canonical transcript, so the
//! handoff survives resume instead of becoming an unbounded hang.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource as CanonicalReminderSource, SystemReminder, TrustedSystemReminderFactory,
    SYSTEM_REMINDER_VERSION,
};
use peri_acp_types::tasks::BgTaskKind;

use crate::agent::stages::SessionHandle;
use crate::session::transcript::MessageTranscript;

/// Upper bound on how long one session waits for its unsettled tasks before the
/// session ends with an observable handoff record (120s, same order as the
/// bounded external MCP call deadline).
pub(crate) const HANDOFF_MAX_WAIT: Duration = Duration::from_secs(120);

/// One unsettled task as recorded in the handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingHandoffTask {
    pub task_id: String,
    pub kind: BgTaskKind,
    /// Session that owns the task projection, scope and reconciliation.
    pub owner_session_id: Option<String>,
    /// Owner identity (MCP instance identity, …) when known.
    pub owner_identity: Option<String>,
}

/// Snapshot recorded when the bounded wait expires.
#[derive(Debug, Clone)]
pub struct PendingHandoff {
    pub tasks: Vec<PendingHandoffTask>,
    pub waited: Duration,
}

/// Deadline state for one session's idle wait.
///
/// `should_wait` is consulted by the ReAct loop's exit decision and returns
/// `false` once the bound expires while work is still pending; `take_due` then
/// reports (exactly once) that the handoff record must be written.
pub(crate) struct BoundedWait {
    bound: Duration,
    since: parking_lot::Mutex<Option<Instant>>,
    due: AtomicBool,
}

impl BoundedWait {
    pub(crate) fn new(bound: Duration) -> Arc<Self> {
        Arc::new(Self {
            bound,
            since: parking_lot::Mutex::new(None),
            due: AtomicBool::new(false),
        })
    }

    /// Whether the session should keep waiting for pending work.
    pub(crate) fn should_wait(&self, busy: bool) -> bool {
        if !busy {
            let mut since = self.since.lock();
            *since = None;
            return false;
        }
        let mut since = self.since.lock();
        let started = *since.get_or_insert_with(Instant::now);
        if started.elapsed() < self.bound {
            true
        } else {
            self.due.store(true, Ordering::Release);
            false
        }
    }

    /// The wait expired with work still pending; true only for the first caller.
    pub(crate) fn take_due(&self) -> bool {
        self.due.swap(false, Ordering::AcqRel)
    }

    /// Absolute deadline of the active wait window, or `None` when the session
    /// is not waiting. The ReAct loop arms a timer with it, so the bound fires
    /// without depending on an unrelated wake-up.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.since.lock().map(|started| started + self.bound)
    }

    pub(crate) fn waited(&self) -> Duration {
        self.since
            .lock()
            .map(|started| started.elapsed())
            .unwrap_or_default()
    }
}

/// Write the bounded-wait handoff into the session's canonical transcript and
/// mirror it into the in-memory view. Returns how many pending tasks were
/// recorded (0 when the session has no persisted transcript).
///
/// The record is idempotent: the delivery ID is derived from the pending task
/// set, so a repeated handoff for the same set converges on one entry. The queue
/// is not woken on purpose — this record belongs to the next turn / resume, not
/// to a new iteration of the turn that is ending.
pub(crate) async fn write_pending_handoff(
    session: &SessionHandle,
    handoff: &PendingHandoff,
) -> usize {
    if handoff.tasks.is_empty() {
        return 0;
    }
    let Some((resources, thread_id, writer)) = session.transcript.read().idempotent_reminder_port()
    else {
        return 0;
    };
    let delivery_id = handoff_delivery_id(&handoff.tasks);
    let reminder = match handoff_reminder(handoff) {
        Ok(reminder) => reminder,
        Err(error) => {
            tracing::warn!(%error, "bounded-wait handoff reminder rejected");
            return 0;
        }
    };
    let committed = async {
        if let Some(writer) = &writer {
            MessageTranscript::flush_via_tx(writer).await?;
        }
        resources
            .append_reminder_if_absent(&thread_id, delivery_id, &reminder)
            .await
            .map_err(anyhow::Error::from)
    }
    .await;
    match committed {
        Ok(_) => {
            session
                .transcript
                .write()
                .mirror_committed_reminder(delivery_id, reminder);
            tracing::warn!(
                pending = handoff.tasks.len(),
                waited_ms = handoff.waited.as_millis() as u64,
                "bounded wait expired; pending task handoff recorded"
            );
            handoff.tasks.len()
        }
        Err(error) => {
            tracing::warn!(%error, "bounded-wait handoff persistence failed");
            0
        }
    }
}

/// Build the handoff reminder (pure: content is the observable contract).
pub(crate) fn handoff_reminder(
    handoff: &PendingHandoff,
) -> Result<peri_acp_types::system_reminder::TrustedSystemReminder, String> {
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: CanonicalReminderSource("handoff".into()),
            kind: "pending_handoff".into(),
            severity: ReminderSeverity::Error,
            delivery: ReminderDelivery::Required,
            audiences: ReminderAudiences(vec![ReminderAudience::Model, ReminderAudience::Tui]),
            body: format!(
                "pending: {}. This session stopped waiting for its unsettled background tasks after \
                 the bounded wait. Unsettled tasks: {}. Scope owner(s): {}. Their results are \
                 reconciled by the scope owner and delivered on resume; a missing notification here \
                 is not a failure of the task.",
                handoff.tasks.len(),
                handoff
                    .tasks
                    .iter()
                    .map(|task| format!("{} ({:?})", task.task_id, task.kind))
                    .collect::<Vec<_>>()
                    .join(", "),
                handoff
                    .tasks
                    .iter()
                    .map(|task| task
                        .owner_session_id
                        .clone()
                        .or_else(|| task.owner_identity.clone())
                        .unwrap_or_else(|| "unknown".into()))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            summary: Some(format!("bounded wait expired: pending {}", handoff.tasks.len())),
            metadata: serde_json::json!({
                "pending": handoff.tasks.len(),
                "waited_ms": handoff.waited.as_millis() as u64,
                "tasks": handoff.tasks.iter().map(|task| serde_json::json!({
                    "task_id": task.task_id,
                    "kind": format!("{:?}", task.kind),
                    "owner_session_id": task.owner_session_id,
                    "owner_identity": task.owner_identity,
                })).collect::<Vec<_>>(),
            }),
        })
        .map_err(|error| error.to_string())
}

/// Stable ID for one pending set: a repeated handoff of the same tasks converges.
fn handoff_delivery_id(tasks: &[PendingHandoffTask]) -> peri_acp_types::messages::MessageId {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"bounded-wait-handoff");
    for task in tasks {
        hasher.update((task.task_id.len() as u64).to_be_bytes());
        hasher.update(task.task_id.as_bytes());
        let owner = task.owner_session_id.as_deref().unwrap_or_default();
        hasher.update((owner.len() as u64).to_be_bytes());
        hasher.update(owner.as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    peri_acp_types::messages::MessageId::from(uuid::Uuid::from_bytes(bytes))
}

#[cfg(test)]
#[path = "handoff_test.rs"]
mod tests;
