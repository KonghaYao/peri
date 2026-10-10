//! Ordered transcript persistence worker: batching, barriers, bounded backlog and terminal failure.
//! The task owns its receiver and all pending payloads; no transcript guard is held
//! while the store is awaited. Transcript memory/compaction ownership stays above.
//!
//! 有界性由**待持久化预算**保证，而不是把等待搬进持锁路径：追加方在持有 transcript
//! 写锁时同步预留条数/字节（通道本身 unbound，预留失败即拒收），writer 只在行为效果
//! 确定后归还。预算覆盖三处积压——channel 队列、writer 待批量、in-flight 批次。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use peri_acp_types::session_resources::{RewindBoundary, SessionResources};
use peri_acp_types::store::PersistedPayload;

use super::PersistOp;
use crate::thread::ThreadId;

/// 一次持久化操作的预算预留（条数 + 估算字节）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reservation {
    pub items: usize,
    pub bytes: usize,
}

impl Reservation {
    pub(super) fn for_payload(payload: &PersistedPayload) -> Self {
        Self {
            items: 1,
            // 与落库编码同源（`serialize_persisted_payload` 的 envelope），不另立估算格式；
            // 编码失败时按 0 计（该 payload 稍后必然在写路径上失败并置终态）。
            bytes: peri_acp_types::store::serialize_persisted_payload(payload)
                .map(|encoded| encoded.len())
                .unwrap_or(0),
        }
    }

    pub(super) fn marker(items: usize) -> Self {
        Self { items, bytes: 0 }
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.items = self.items.saturating_add(other.items);
        self.bytes = self.bytes.saturating_add(other.bytes);
    }
}

/// 待持久化预算：条数与字节的共同上限。
///
/// 语义：
/// - 预留同步完成（调用方持锁时不得 await）；
/// - 归还只在**效果确定或缓冲真实释放**之后发生——取消调用方不等于释放 adapter
///   仍持有的 payload；
/// - 预留失败与 writer 终态失败都写成 sticky 失败：热态不再可信，调用方必须停止
///   后续写入并重载会话。
#[derive(Debug)]
pub struct PersistenceBudget {
    max_items: usize,
    max_bytes: usize,
    items: AtomicUsize,
    bytes: AtomicUsize,
    /// 被拒收的条数（已进内存、未获准持久化）：报错时必须如实计入。
    refused: AtomicUsize,
    failure: Mutex<Option<String>>,
}

impl PersistenceBudget {
    pub fn new(max_items: usize, max_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            max_items,
            max_bytes,
            items: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
            refused: AtomicUsize::new(0),
            failure: Mutex::new(None),
        })
    }

    /// 预留一次写入额度；失败即 sticky 失败（预算耗尽本身就是失败事实）。
    pub(super) fn try_reserve(&self, reservation: Reservation) -> Result<(), String> {
        if self.is_failed() {
            self.refused
                .fetch_add(reservation.items.max(1), Ordering::AcqRel);
            return Err(self.failure().unwrap_or_default());
        }
        // 单次预留超过上限时明确拒绝，不把「超限」静默截断成「已允许」。
        if reservation.items > self.max_items || reservation.bytes > self.max_bytes {
            let reason = format!(
                "transcript persistence budget exhausted: single write needs {} item(s)/{} byte(s), budget is {} item(s)/{} byte(s)",
                reservation.items, reservation.bytes, self.max_items, self.max_bytes
            );
            self.refused
                .fetch_add(reservation.items.max(1), Ordering::AcqRel);
            self.mark_failed(&reason);
            return Err(reason);
        }
        let previous_items = self.items.fetch_add(reservation.items, Ordering::AcqRel);
        let previous_bytes = self.bytes.fetch_add(reservation.bytes, Ordering::AcqRel);
        if previous_items + reservation.items > self.max_items
            || previous_bytes + reservation.bytes > self.max_bytes
        {
            self.items.fetch_sub(reservation.items, Ordering::AcqRel);
            self.bytes.fetch_sub(reservation.bytes, Ordering::AcqRel);
            let reason = format!(
                "transcript persistence backlog is full ({} item(s)/{} byte(s) outstanding); {} payload(s) entered memory without a durable slot",
                self.max_items,
                self.max_bytes,
                self.refused.load(Ordering::Acquire) + reservation.items
            );
            self.refused
                .fetch_add(reservation.items.max(1), Ordering::AcqRel);
            self.mark_failed(&reason);
            return Err(reason);
        }
        Ok(())
    }

    pub(super) fn release(&self, reservation: Reservation) {
        self.items.fetch_sub(reservation.items, Ordering::AcqRel);
        self.bytes.fetch_sub(reservation.bytes, Ordering::AcqRel);
    }

    /// 首次失败即定格（后续失败不覆盖首个原因）。
    pub(super) fn mark_failed(&self, reason: &str) {
        if let Ok(mut slot) = self.failure.lock() {
            if slot.is_none() {
                *slot = Some(reason.to_owned());
            }
        }
    }

    pub fn failure(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|slot| slot.clone())
    }

    pub fn is_failed(&self) -> bool {
        self.failure().is_some()
    }

    /// 当前未归还的积压（条数, 字节）——测试用可观察量。
    pub fn outstanding(&self) -> (usize, usize) {
        (
            self.items.load(Ordering::Acquire),
            self.bytes.load(Ordering::Acquire),
        )
    }
}

/// 将积压的 Append 批量落库（单次 `append_history` 调用 → 一个数据行为/一个事务）。
///
/// 成功后清空积压并归还预留；失败时保留 payload 与首个错误，不重试可能部分成功的批次，
/// 也不归还预留（adapter 可能仍持有这些 payload）。
async fn flush_appends(
    store: &dyn SessionResources,
    tid: &ThreadId,
    pending: &mut Vec<PersistedPayload>,
    reserved: &mut Reservation,
    barrier_error: &mut Option<String>,
    processed: &mut u64,
    budget: &PersistenceBudget,
) {
    if pending.is_empty() {
        return;
    }
    if let Some(sticky) = budget.failure() {
        // 预算已定格失败：不再把 payload 交给 adapter，也不归还预留（它们确实未落盘）。
        *barrier_error = Some(sticky);
        return;
    }
    if barrier_error.is_some() {
        return;
    }
    if let Err(e) = store.append_history(tid, pending).await {
        tracing::warn!(
            pending = pending.len(),
            "transcript persist entered terminal failure: {e}"
        );
        let reason = format!("append_history failed: {e}");
        budget.mark_failed(&reason);
        *barrier_error = Some(reason);
        return;
    }
    *processed = processed.saturating_add(pending.len() as u64);
    pending.clear();
    budget.release(*reserved);
    *reserved = Reservation::default();
}

pub(super) async fn run_writer(
    store: Arc<dyn SessionResources>,
    tid: ThreadId,
    budget: Arc<PersistenceBudget>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<PersistOp>,
) {
    let mut processed: u64 = 0;
    let mut last_warn_at: u64 = 0;
    let mut barrier_error: Option<String> = None;

    // 短窗口 Append 合并：把 ≤100ms 窗口（或 ≥APPEND_BATCH_MAX 条）内的
    /// Append 积压为一次 `append_history` 批量调用（单事务 = 一次 WAL fsync），
    /// 消除工具消息风暴下每消息一次 fsync。
    //
    // 可见性语义不变：
    // - Barrier 到达时先 flush 积压再 ack（flush_persistence 确认 = 已落库）
    // - 其他 op 到达时先 flush 积压，保持 FIFO 顺序
    // - 通道关闭时 flush 剩余
    const APPEND_BATCH_MAX: usize = 64;
    const APPEND_BATCH_WINDOW: std::time::Duration = std::time::Duration::from_millis(100);

    let mut pending_appends: Vec<PersistedPayload> = Vec::new();
    let mut pending_reserved = Reservation::default();
    let mut window_start: std::time::Instant = peri_time::monotonic_now();

    loop {
        // 失败后的积压仅用于 barrier 诊断，不能继续驱动批处理定时器。
        // 等待新 op 仍允许 sticky Barrier / Shutdown 以及有界失败缓冲处理。
        let op = if pending_appends.is_empty() || barrier_error.is_some() {
            rx.recv().await
        } else {
            let remaining = APPEND_BATCH_WINDOW.saturating_sub(window_start.elapsed());
            match peri_time::timeout(remaining, rx.recv()).await {
                Ok(op) => op,
                Err(_) => {
                    // 窗口到期：批量落库后继续等待
                    flush_appends(
                        store.as_ref(),
                        &tid,
                        &mut pending_appends,
                        &mut pending_reserved,
                        &mut barrier_error,
                        &mut processed,
                        &budget,
                    )
                    .await;
                    continue;
                }
            }
        };

        match op {
            Some(PersistOp::Append { payload, reserved }) => {
                if pending_appends.is_empty() {
                    window_start = peri_time::monotonic_now();
                }
                pending_reserved.merge(reserved);
                pending_appends.push(payload);
                if pending_appends.len() >= APPEND_BATCH_MAX {
                    flush_appends(
                        store.as_ref(),
                        &tid,
                        &mut pending_appends,
                        &mut pending_reserved,
                        &mut barrier_error,
                        &mut processed,
                        &budget,
                    )
                    .await;
                }
            }
            Some(PersistOp::Barrier(ack)) => {
                // Barrier 语义：确认此前所有 op 均已实际调用 store
                flush_appends(
                    store.as_ref(),
                    &tid,
                    &mut pending_appends,
                    &mut pending_reserved,
                    &mut barrier_error,
                    &mut processed,
                    &budget,
                )
                .await;
                let sticky = budget.failure();
                let result = barrier_error
                    .as_ref()
                    .or(sticky.as_ref())
                    .map_or(Ok(()), |error| {
                        Err(anyhow!(
                            "{error}; {} item(s) remain unpersisted",
                            pending_reserved.items
                        ))
                    });
                let _ = ack.send(result);
            }
            Some(PersistOp::Shutdown) | None => {
                // 优雅关闭：flush 剩余积压后退出。
                // - Shutdown：Drop / shutdown_persistence 显式请求（参照
                //   langfuse-client/src/batcher.rs 的 Shutdown 模式——不 abort，
                //   abort 会立即取消任务导致 pending_appends 和通道中未处理的
                //   消息被直接丢弃）
                // - None：通道关闭（所有发送端已 drop），等效于 Shutdown
                // 注意：必须放在 `Some(other)` 通配分支之前，否则 Shutdown
                // 会被当作普通 op 落入 unreachable!。
                flush_appends(
                    store.as_ref(),
                    &tid,
                    &mut pending_appends,
                    &mut pending_reserved,
                    &mut barrier_error,
                    &mut processed,
                    &budget,
                )
                .await;
                break;
            }
            Some(other) => {
                // 保序：先 flush 积压 Append，再处理非 Append op
                flush_appends(
                    store.as_ref(),
                    &tid,
                    &mut pending_appends,
                    &mut pending_reserved,
                    &mut barrier_error,
                    &mut processed,
                    &budget,
                )
                .await;
                let (reservation, result) = match other {
                    PersistOp::RewindTo { id, reserved } => (
                        reserved,
                        store
                            .rewind_history(&tid, RewindBoundary::KeepThrough(id))
                            .await,
                    ),
                    PersistOp::UpdateFlags {
                        id,
                        flags,
                        reserved,
                    } => (
                        reserved,
                        store.apply_message_projections(&tid, &[(id, flags)]).await,
                    ),
                    PersistOp::ApplyCompactionBatch { updates, reserved } => (
                        reserved,
                        store.apply_message_projections(&tid, &updates).await,
                    ),
                    PersistOp::Append { .. } | PersistOp::Barrier(_) | PersistOp::Shutdown => {
                        unreachable!("handled in dedicated branches above")
                    }
                };
                let result = if let Some(sticky) = barrier_error.as_ref() {
                    Err(anyhow!(sticky.clone()))
                } else {
                    match result {
                        Ok(()) => {
                            // 效果确定：release 与持久化效果同步，取消调用方不算释放。
                            budget.release(reservation);
                            Ok(())
                        }
                        Err(error) => {
                            let reason = error.to_string();
                            budget.mark_failed(&reason);
                            Err(anyhow!(reason))
                        }
                    }
                };
                if let Err(e) = result {
                    tracing::warn!("transcript persist failed: {e}");
                    if barrier_error.is_none() {
                        barrier_error = Some(e.to_string());
                    }
                } else {
                    processed = processed.saturating_add(1);
                    if processed >= last_warn_at.saturating_add(1000) {
                        last_warn_at = processed;
                        tracing::debug!(processed, "transcript persist progress");
                    }
                }
            }
        }
    }
}
