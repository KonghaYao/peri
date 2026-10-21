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

use peri_acp_types::store::PERSISTED_PAYLOAD_VERSION;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource as CanonicalReminderSource, SystemReminder, TrustedSystemReminder,
    TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use peri_acp_types::tasks::BgTaskKind;
use peri_acp_types::thread::ThreadId;

use crate::agent::stages::SessionHandle;
use crate::session::transcript::MessageTranscript;

/// Upper bound on how long one session waits for its unsettled tasks before the
/// session ends with an observable handoff record (120s, same order as the
/// bounded external MCP call deadline).
pub(crate) const HANDOFF_MAX_WAIT: Duration = Duration::from_secs(120);

/// Canonical reminder kind for the bounded-wait handoff.
const HANDOFF_REMINDER_KIND: &str = "pending_handoff";

/// Identity domain of the canonical pending-handoff record.
///
/// v1 derived the delivery ID from the pending tasks while the canonical payload
/// carried the attempt's `waited_ms`. The same pending set then produced a new
/// payload under an old ID, and the canonical store (correctly) rejected the
/// duplicate ID with different content, failing the whole turn. v2 keeps the
/// payload contract free of per-attempt observations and separates itself from
/// rows already written under the old contract. Bump this string whenever the
/// persisted handoff contract changes: existing rows stay untouched, and a
/// pending set gets at most one new-version record.
const HANDOFF_DELIVERY_DOMAIN: &[u8] = b"bounded-wait-handoff-v2";

/// 单次 child 执行的收尾窗口；启动后永不重置，不限制已准入迭代的 I/O。
#[derive(Debug, Default)]
pub struct ChildSettlementWindow {
    deadline: parking_lot::Mutex<Option<Instant>>,
}

impl ChildSettlementWindow {
    pub fn start(&self) -> Instant {
        *self
            .deadline
            .lock()
            .get_or_insert_with(|| Instant::now() + HANDOFF_MAX_WAIT)
    }

    pub fn deadline(&self) -> Option<Instant> {
        *self.deadline.lock()
    }

    pub fn is_started(&self) -> bool {
        self.deadline.lock().is_some()
    }

    pub fn expired(&self) -> bool {
        self.deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
}

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

/// 将有界等待交接写入 canonical 并镜像热态；无任务、缺能力、写失败分别返回。
/// 同一 pending 集合沿用稳定 delivery ID，不唤醒正在结束的执行。
///
/// 幂等契约：ID 与载荷都只由归一化后的任务快照、目标会话与契约版本决定。
/// `handoff.waited` 只用于诊断日志，不进入 canonical 载荷——同一集合的第二次
/// 交接必须复现完全相同的持久化字节，否则 store 的「同 ID 异内容」保护会让整轮
/// 执行失败。
#[derive(Debug, thiserror::Error)]
pub enum HandoffCommitError {
    #[error("handoff_capability_unavailable")]
    MissingCapability,
    #[error("handoff_persistence_failed: {0}")]
    Persistence(#[from] anyhow::Error),
}

#[derive(Debug, PartialEq, Eq)]
pub enum HandoffCommit {
    NoObligations,
    Committed(peri_acp_types::messages::MessageId),
}

pub(crate) async fn write_pending_handoff(
    session: &SessionHandle,
    handoff: &PendingHandoff,
) -> Result<HandoffCommit, HandoffCommitError> {
    // 归一化一次，随后 ID 与载荷都从这份快照派生，调用方迭代顺序不能造成漂移。
    let tasks = normalized_pending_tasks(&handoff.tasks);
    if tasks.is_empty() {
        return Ok(HandoffCommit::NoObligations);
    }
    let Some((resources, thread_id, writer)) = session.transcript.read().idempotent_reminder_port()
    else {
        tracing::error!("handoff canonical capability unavailable");
        return Err(HandoffCommitError::MissingCapability);
    };
    let reminder = match handoff_reminder(&tasks) {
        Ok(reminder) => reminder,
        Err(error) => {
            tracing::error!(%error, "bounded-wait handoff reminder rejected");
            return Err(HandoffCommitError::Persistence(anyhow::anyhow!(error)));
        }
    };
    let canonical = match canonical_handoff_payload(&reminder) {
        Ok(canonical) => canonical,
        Err(error) => {
            tracing::error!(
                %thread_id,
                kind = HANDOFF_REMINDER_KIND,
                %error,
                "bounded-wait handoff canonical payload rejected"
            );
            return Err(HandoffCommitError::Persistence(anyhow::anyhow!(error)));
        }
    };
    let delivery_id = handoff_delivery_id(&thread_id, &canonical);
    let payload_digest = sha256_hex(&canonical);
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
        // (a) 本次提交成功：安装到热态视图。
        Ok(true) => {
            session
                .transcript
                .write()
                .mirror_committed_reminder(delivery_id, reminder);
        }
        // 持久化事实已存在但热态没有（如 Full Compact 收缩释放了它）：
        // (b) 仍有效 → 补安装；(c) 已排除 → 不安装（归档原文不得回灌热态）。
        // 两种状态都不发射事件：本函数只写账本，交接由下一轮 Receive 呈现。
        Ok(false) => match resources
            .load_message_flags(&thread_id, &[delivery_id])
            .await
        {
            Ok(flags) => {
                if !flags.get(&delivery_id).is_some_and(|flags| flags.excluded) {
                    session
                        .transcript
                        .write()
                        .mirror_committed_reminder(delivery_id, reminder);
                }
            }
            Err(error) => {
                tracing::error!(
                    %thread_id,
                    delivery_id = %delivery_id.as_uuid(),
                    kind = HANDOFF_REMINDER_KIND,
                    payload_digest = %payload_digest,
                    %error,
                    "bounded-wait handoff flag lookup failed"
                );
                return Err(HandoffCommitError::Persistence(error.into()));
            }
        },
        Err(error) => {
            tracing::error!(
                %thread_id,
                delivery_id = %delivery_id.as_uuid(),
                kind = HANDOFF_REMINDER_KIND,
                payload_digest = %payload_digest,
                %error,
                "bounded-wait handoff persistence failed"
            );
            return Err(HandoffCommitError::Persistence(error));
        }
    }
    tracing::warn!(
        %thread_id,
        delivery_id = %delivery_id.as_uuid(),
        pending = tasks.len(),
        waited_ms = handoff.waited.as_millis() as u64,
        "bounded wait expired; pending task handoff recorded"
    );
    Ok(HandoffCommit::Committed(delivery_id))
}

pub async fn commit_child_handoff(
    session: &SessionHandle,
    owner: &crate::session::subagent::ParkedChildOwner,
    reason: crate::session::subagent::ChildHandoffReason,
    received_messages: Vec<peri_acp_types::messages::MessageId>,
    unprocessed_tool_calls: Vec<String>,
) -> Result<crate::session::subagent::ChildHandoffReceipt, HandoffCommitError> {
    use crate::session::subagent::ChildHandoffReceipt;
    let execution = session.turn.execution_binding();
    let queued = owner.queue.snapshot();
    let mut message_ids = received_messages;
    message_ids.extend(queued.iter().filter_map(|message| {
        message
            .delivery_id
            .or_else(|| message.message().map(crate::messages::BaseMessage::id))
    }));
    let receipt = owner
        .freeze_receipt(ChildHandoffReceipt {
            child_thread_id: owner.child_thread_id.clone(),
            execution,
            owner_session_id: owner.child_thread_id.clone(),
            reason,
            tasks: owner.manager.pending_handoff_tasks(),
            message_ids,
            message_sequences: queued
                .iter()
                .filter_map(|message| message.admission_sequence)
                .collect(),
            unprocessed_tool_calls,
            canonical_message_id: peri_acp_types::messages::MessageId::new(),
        })
        .map_err(|error| HandoffCommitError::Persistence(anyhow::anyhow!(error)))?;
    let reminder = crate::session::producer_reminders::try_trusted_reminder(
        ReminderCategory::Task, "subagent_task_lifecycle", "child_tasks_handed_off",
        ReminderSeverity::Error, ReminderDelivery::Required,
        format!("子 Agent 本次委派未完成，计算已结束。child_thread_id: {}；资源 owner: {}；未结任务: {}；未推理 tool calls: {}。Shell 状态未改变。结果留在原 child 队列，不自动恢复旧 run；需要显式 Agent(resume_thread_id, prompt) 继续分析。",
            receipt.child_thread_id, receipt.owner_session_id,
            receipt.tasks.iter().map(|task| task.task_id.as_str()).collect::<Vec<_>>().join(", "),
            receipt.unprocessed_tool_calls.join(", ")),
        Some("本次委派未完成，资源已保留".into()),
        serde_json::json!({
            "child_thread_id": receipt.child_thread_id,
            "execution": receipt.execution,
            "owner_session_id": receipt.owner_session_id,
            "reason": format!("{:?}", receipt.reason),
            "task_ids": receipt.tasks.iter().map(|task| &task.task_id).collect::<Vec<_>>(),
            "message_ids": receipt.message_ids,
            "message_sequences": receipt.message_sequences,
            "unprocessed_tool_calls": receipt.unprocessed_tool_calls,
        }),
    ).map_err(|error| HandoffCommitError::Persistence(anyhow::anyhow!(error)))?;
    let port = session.transcript.read().idempotent_reminder_port();
    let (resources, thread_id, writer) = port.ok_or(HandoffCommitError::MissingCapability)?;
    let result = async {
        if let Some(writer) = writer {
            MessageTranscript::flush_via_tx(&writer).await?;
        }
        resources
            .append_reminder_if_absent(&thread_id, receipt.canonical_message_id, &reminder)
            .await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        tracing::error!(child_thread_id = %receipt.child_thread_id, %error, "handoff_persistence_failed");
        return Err(HandoffCommitError::Persistence(error));
    }
    session
        .transcript
        .write()
        .mirror_committed_reminder(receipt.canonical_message_id, reminder);
    owner
        .retain_receipt(receipt.clone())
        .map_err(|error| HandoffCommitError::Persistence(anyhow::anyhow!(error)))?;
    Ok(receipt)
}

/// Build the handoff reminder (pure: content is the observable contract).
///
/// Takes the already-normalized task snapshot. The attempt's elapsed time is
/// deliberately absent: it varies between two handoffs of the same pending set,
/// while the canonical payload must stay byte-identical for the same delivery ID.
pub(crate) fn handoff_reminder(
    tasks: &[PendingHandoffTask],
) -> Result<peri_acp_types::system_reminder::TrustedSystemReminder, String> {
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: CanonicalReminderSource("handoff".into()),
            kind: HANDOFF_REMINDER_KIND.into(),
            severity: ReminderSeverity::Error,
            delivery: ReminderDelivery::Required,
            audiences: ReminderAudiences(vec![ReminderAudience::Model, ReminderAudience::Tui]),
            body: format!(
                "pending: {}. This session stopped waiting for its unsettled background tasks after \
                 the bounded wait. Unsettled tasks: {}. Scope owner(s): {}. Their results are \
                 reconciled by the scope owner and delivered on resume; a missing notification here \
                 is not a failure of the task.",
                tasks.len(),
                tasks
                    .iter()
                    .map(|task| format!("{} ({:?})", task.task_id, task.kind))
                    .collect::<Vec<_>>()
                    .join(", "),
                tasks
                    .iter()
                    .map(|task| task
                        .owner_session_id
                        .clone()
                        .or_else(|| task.owner_identity.clone())
                        .unwrap_or_else(|| "unknown".into()))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            summary: Some(format!("bounded wait expired: pending {}", tasks.len())),
            metadata: serde_json::json!({
                "pending": tasks.len(),
                "tasks": tasks.iter().map(|task| serde_json::json!({
                    "task_id": task.task_id,
                    "kind": format!("{:?}", task.kind),
                    "owner_session_id": task.owner_session_id,
                    "owner_identity": task.owner_identity,
                })).collect::<Vec<_>>(),
            }),
        })
        .map_err(|error| error.to_string())
}

/// Deterministic order for one pending set.
///
/// The registry already sorts by task ID, but callers are not part of the
/// contract: both the identity and the canonical payload are derived from this
/// snapshot, so an unsorted or duplicated input cannot make the same set hash
/// differently from the bytes it persists.
fn normalized_pending_tasks(tasks: &[PendingHandoffTask]) -> Vec<PendingHandoffTask> {
    let mut normalized = tasks.to_vec();
    normalized.sort_by(|a, b| {
        a.task_id
            .cmp(&b.task_id)
            .then_with(|| kind_rank(a.kind).cmp(&kind_rank(b.kind)))
            .then_with(|| a.owner_session_id.cmp(&b.owner_session_id))
            .then_with(|| a.owner_identity.cmp(&b.owner_identity))
    });
    normalized.dedup();
    normalized
}

/// Total order over task kinds for normalization (declaration order of the contract enum).
fn kind_rank(kind: BgTaskKind) -> u8 {
    match kind {
        BgTaskKind::Shell => 0,
        BgTaskKind::Agent => 1,
        BgTaskKind::Workflow => 2,
        BgTaskKind::Mcp => 3,
    }
}

/// Canonical bytes of the reminder for identity derivation.
///
/// The store envelope embeds the ID, so hashing it here would be circular; the
/// reminder itself is everything the envelope adds around the ID. `serde_json`
/// serializes struct fields in declaration order and object keys in sorted
/// order, so equal reminders produce equal bytes in every process.
fn canonical_handoff_payload(reminder: &TrustedSystemReminder) -> Result<Vec<u8>, String> {
    serde_json::to_vec(reminder.as_reminder())
        .map_err(|error| format!("canonical handoff payload is not serializable: {error}"))
}

/// Stable ID for one pending set: a repeated handoff of the same tasks in the
/// same thread converges on the same ID and the same canonical payload.
///
/// Hashes the domain/format, the owning thread, the reminder kind and the
/// canonical payload. Every fact that changes what is persisted therefore
/// changes the ID, so an equal ID can only ever carry equal content.
fn handoff_delivery_id(
    thread_id: &ThreadId,
    canonical_payload: &[u8],
) -> peri_acp_types::messages::MessageId {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(HANDOFF_DELIVERY_DOMAIN);
    hasher.update(PERSISTED_PAYLOAD_VERSION.to_be_bytes());
    write_len_prefixed(&mut hasher, thread_id.as_bytes());
    write_len_prefixed(&mut hasher, HANDOFF_REMINDER_KIND.as_bytes());
    write_len_prefixed(&mut hasher, canonical_payload);
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    peri_acp_types::messages::MessageId::from(uuid::Uuid::from_bytes(bytes))
}

/// Length-prefix each field so concatenation cannot collide two different inputs.
fn write_len_prefixed(hasher: &mut impl sha2::Digest, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

/// Short digest for diagnostics; payload content itself stays out of the logs.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
#[path = "handoff_test.rs"]
mod tests;
