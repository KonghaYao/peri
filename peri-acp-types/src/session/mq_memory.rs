//! 内存 MQ store：`MessageQueue::new()` 使用的测试 / 嵌入后端。
//!
//! 语义与 SQLite / 远端 adapter 对齐（见 [`ThreadMqStore`] 文档）：入队判定顺序
//! 「历史 → MQ → 插入」，Receive 逐项原子移交，取消物理删除。差别只在执行器：
//! 这里用一把 `Mutex` 代替事务，因此同一个 `MessageQueue` 上的操作天然串行。
//!
//! `enqueued_at` 用固定基准（epoch）+ 微秒递增计数器生成，同一进程内的入队顺序即成
//! 消费顺序，测试不依赖真实时钟。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use super::mq::{
    MqCancelOutcome, MqEnqueueDisposition, MqEnqueueOutcome, MqEntry, MqFilter, MqInput,
    MqReceiveDisposition, MqReceiveItemOutcome, MqReceiveOutcome, MqReceiveSelection,
    MqReceiveStatus, ThreadMqStore,
};
use super::MessageSource;
use crate::messages::MessageId;
use crate::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use crate::store::{serialize_persisted_payload, PersistedPayload};
use crate::thread::ThreadId;

const ENQUEUED_AT_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.6fZ";

#[derive(Debug)]
struct StoredRow {
    thread_id: ThreadId,
    entry: MqEntry,
    payload_json: String,
}

#[derive(Debug)]
struct HistoryRow {
    thread_id: ThreadId,
    payload_json: String,
}

#[derive(Debug, Default)]
struct MemoryState {
    rows: Vec<StoredRow>,
    history: HashMap<MessageId, HistoryRow>,
    clock: u64,
}

/// 进程内 MQ 存储；仅供测试与无持久化的嵌入场景。
#[derive(Debug, Default, Clone)]
pub struct InMemoryMqStore {
    state: Arc<Mutex<MemoryState>>,
    #[cfg(feature = "test-support")]
    receive_gate: Arc<Mutex<Option<super::mq::ReceiveGate>>>,
    #[cfg(feature = "test-support")]
    query_gate: Arc<Mutex<Option<super::mq::ReceiveGate>>>,
}

impl InMemoryMqStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ThreadMqStore for InMemoryMqStore {
    async fn mq_enqueue(
        &self,
        thread_id: &ThreadId,
        input: MqInput,
    ) -> SessionResourceResult<MqEnqueueOutcome> {
        if input.payload.id() != input.id {
            return Err(invalid_input(
                "MQ payload id must match the queued message id",
            ));
        }
        let payload_json = payload_json(&input.payload)?;
        let mut state = self.state.lock();
        // 历史事实优先：已移交的 ID 不再回到 MQ。
        if let Some(history) = state.history.get(&input.id) {
            if history.thread_id != *thread_id {
                return Err(conflict(
                    "message id already belongs to another thread's history",
                ));
            }
            return Ok(MqEnqueueOutcome {
                disposition: MqEnqueueDisposition::AlreadyHandedOff,
                // 历史行不保存入队时间，也没有 MQ 行：与存储 adapter 一致返回请求内容的投影。
                entry: MqEntry {
                    id: input.id,
                    enqueued_at: enqueued_at_now(&mut state.clock),
                    kind: input.kind,
                    source: input.source,
                    payload: input.payload,
                    policy: input.policy,
                    original_draft: input.original_draft,
                },
            });
        }
        if let Some(index) = state.rows.iter().position(|row| row.entry.id == input.id) {
            let row = &state.rows[index];
            if row.thread_id != *thread_id {
                return Err(conflict("message id already belongs to another thread"));
            }
            let same = row.entry.kind == input.kind
                && row.entry.source == input.source
                && row.entry.policy == input.policy
                && row.payload_json == payload_json
                && row.entry.original_draft == input.original_draft;
            if !same {
                return Err(conflict(
                    "a different message already uses this id in the thread MQ",
                ));
            }
            return Ok(MqEnqueueOutcome {
                disposition: MqEnqueueDisposition::Existing,
                entry: row.entry.clone(),
            });
        }
        let entry = MqEntry {
            id: input.id,
            enqueued_at: enqueued_at_now(&mut state.clock),
            kind: input.kind,
            source: input.source,
            payload: input.payload,
            policy: input.policy,
            original_draft: input.original_draft,
        };
        state.rows.push(StoredRow {
            thread_id: thread_id.clone(),
            entry: entry.clone(),
            payload_json,
        });
        Ok(MqEnqueueOutcome {
            disposition: MqEnqueueDisposition::Inserted,
            entry,
        })
    }

    async fn mq_query(
        &self,
        thread_id: &ThreadId,
        filter: MqFilter,
    ) -> SessionResourceResult<Vec<MqEntry>> {
        let mut entries = self
            .state
            .lock()
            .rows
            .iter()
            .filter(|row| row.thread_id == *thread_id)
            .map(|row| row.entry.clone())
            .collect::<Vec<_>>();
        entries.sort_by_key(MqEntry::watermark);
        let entries = filter_entries(entries, &filter);
        #[cfg(feature = "test-support")]
        {
            // 暂停「读取完成、尚未返回」的窗口：调用方的旧快照正是从这里开始过期。
            let gate = self.query_gate.lock().take();
            if let Some((entered, release)) = gate {
                let _ = entered.send(());
                let _ = release.await;
            }
        }
        Ok(entries)
    }

    async fn mq_history_contains(
        &self,
        thread_id: &ThreadId,
        id: MessageId,
    ) -> SessionResourceResult<bool> {
        Ok(self
            .state
            .lock()
            .history
            .get(&id)
            .is_some_and(|history| history.thread_id == *thread_id))
    }

    async fn mq_cancel(
        &self,
        thread_id: &ThreadId,
        id: MessageId,
    ) -> SessionResourceResult<MqCancelOutcome> {
        let mut state = self.state.lock();
        let Some(index) = state.rows.iter().position(|row| row.entry.id == id) else {
            return Ok(match state.history.get(&id) {
                Some(history) if history.thread_id == *thread_id => {
                    MqCancelOutcome::AlreadyHandedOff
                }
                _ => MqCancelOutcome::NotFound,
            });
        };
        if state.rows[index].thread_id != *thread_id {
            return Ok(MqCancelOutcome::NotFound);
        }
        if state.rows[index].entry.source != MessageSource::UserInput {
            return Err(invalid_input(
                "only pending user input can be cancelled from the thread MQ",
            ));
        }
        let row = state.rows.remove(index);
        Ok(MqCancelOutcome::Cancelled(row.entry))
    }

    async fn mq_receive(
        &self,
        thread_id: &ThreadId,
        selection: MqReceiveSelection,
    ) -> SessionResourceResult<MqReceiveOutcome> {
        #[cfg(feature = "test-support")]
        {
            // 暂停下一次真正的规范提交（任意载荷）：测试要在「已选中、尚未写入」
            // 的窗口里观察 mailbox／交接状态。
            let gates_commit = selection
                .items
                .iter()
                .any(|item| item.disposition == MqReceiveDisposition::Commit);
            if gates_commit {
                let gate = self.receive_gate.lock().take();
                if let Some((entered, release)) = gate {
                    let _ = entered.send(());
                    let _ = release.await;
                }
            }
        }
        let mut state = self.state.lock();
        let mut items = Vec::with_capacity(selection.items.len());
        for item in &selection.items {
            let Some(index) = state
                .rows
                .iter()
                .position(|row| row.entry.id == item.entry.id)
            else {
                items.push(item_outcome(item.entry.id, MqReceiveStatus::Missing, None));
                continue;
            };
            let row = &state.rows[index];
            // 选择基于旧快照（行已取消 / 移交，或同 ID 重新入队）：什么都不写。
            if row.thread_id != *thread_id || row.entry.enqueued_at != item.entry.enqueued_at {
                items.push(item_outcome(item.entry.id, MqReceiveStatus::Missing, None));
                continue;
            }
            let payload_json = row.payload_json.clone();
            let status = match item.disposition {
                MqReceiveDisposition::Suppress => MqReceiveStatus::Suppressed,
                MqReceiveDisposition::Commit => match state.history.get(&item.entry.id) {
                    Some(history) if history.thread_id == *thread_id => {
                        if history.payload_json == payload_json {
                            MqReceiveStatus::AlreadyPresent
                        } else {
                            MqReceiveStatus::Conflict
                        }
                    }
                    Some(_) => MqReceiveStatus::Conflict,
                    None => {
                        state.history.insert(
                            item.entry.id,
                            HistoryRow {
                                thread_id: thread_id.clone(),
                                payload_json,
                            },
                        );
                        MqReceiveStatus::Committed
                    }
                },
            };
            let row = state.rows.remove(index);
            items.push(item_outcome(item.entry.id, status, Some(row.entry)));
        }
        Ok(MqReceiveOutcome { items })
    }

    async fn mq_clear(&self, thread_id: &ThreadId) -> SessionResourceResult<()> {
        self.state
            .lock()
            .rows
            .retain(|row| row.thread_id != *thread_id);
        Ok(())
    }

    fn supports_immediate_enqueue(&self) -> bool {
        true
    }

    #[cfg(feature = "test-support")]
    fn install_receive_gate(&self, gate: super::mq::ReceiveGate) {
        *self.receive_gate.lock() = Some(gate);
    }

    #[cfg(feature = "test-support")]
    fn install_query_gate(&self, gate: super::mq::ReceiveGate) {
        *self.query_gate.lock() = Some(gate);
    }
}

fn item_outcome(
    id: MessageId,
    status: MqReceiveStatus,
    entry: Option<MqEntry>,
) -> MqReceiveItemOutcome {
    MqReceiveItemOutcome { id, status, entry }
}

/// 投影过滤：输入必须已按 `(enqueued_at, id)` 升序，这里只做收敛。
fn filter_entries(entries: impl IntoIterator<Item = MqEntry>, filter: &MqFilter) -> Vec<MqEntry> {
    let limit = filter.limit.unwrap_or(usize::MAX);
    let mut selected = Vec::new();
    for entry in entries {
        if filter.user_input_only && entry.source != MessageSource::UserInput {
            continue;
        }
        if !filter.sources.is_empty() && !filter.sources.contains(&entry.source) {
            continue;
        }
        if !filter.kinds.is_empty() && !filter.kinds.contains(&entry.kind) {
            continue;
        }
        if let Some(after) = &filter.after {
            if entry.watermark() <= *after {
                continue;
            }
        }
        selected.push(entry);
        if selected.len() >= limit {
            break;
        }
    }
    selected
}

fn payload_json(payload: &PersistedPayload) -> SessionResourceResult<String> {
    serialize_persisted_payload(payload).map_err(|_| {
        SessionResourceError::new(SessionResourceErrorKind::Internal {
            detail: "MQ payload is not serializable".to_owned(),
        })
    })
}

fn enqueued_at_now(clock: &mut u64) -> String {
    *clock = clock.saturating_add(1);
    let micros = i64::try_from(*clock).unwrap_or(i64::MAX);
    chrono::DateTime::<chrono::Utc>::from_timestamp_micros(micros)
        .unwrap_or_else(|| {
            chrono::DateTime::<chrono::Utc>::from_timestamp_micros(0)
                .expect("epoch is a valid timestamp")
        })
        .format(ENQUEUED_AT_FORMAT)
        .to_string()
}

fn invalid_input(detail: impl Into<String>) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
        detail: detail.into(),
    })
}

fn conflict(detail: impl Into<String>) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Conflict {
        detail: detail.into(),
    })
}
