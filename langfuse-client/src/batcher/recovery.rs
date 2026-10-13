//! 恢复与重试：`Retryable` 持久写回 `retry_wait` 由同一 worker 到期续发（首次时间与 TTL 不变）；
//! 只有 `Delivered` 是交付。启动扫描与清理登记见子模块 [`scan`]（契约 §5.2、§7 P4a）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::warn;

use super::stats::Counters;
use crate::client::{export, send, ExportConfig, LangfuseClient};
use crate::config::TelemetryConfig;
use crate::encoding::{BudgetedWriter, ExportContext};
use crate::error::LangfuseError;
use crate::memory::MemoryBudget;
use crate::payload::{ExportPayload, PayloadBody};
use crate::snapshot::TelemetryEvent;
use crate::spool::cleanup::CleanupRegistry;
use crate::spool::{
    CleanupRegistration, ObjectClaim, ObjectMeta, SpoolError, SpoolState, SpoolStore,
};

mod scan;

pub(in crate::batcher) use scan::*;

/// 一条等待重试的 payload：`due_at_ms` 为 `None` 表示持久状态待修复、禁止发送，首次接受时间与 TTL 不因重试重置。
pub(super) struct RetryEntry {
    pub(super) payload: ExportPayload,
    pub(super) accepted_at_ms: i64,
    pub(super) expires_at_ms: i64,
    pub(super) events: u64,
    /// 已在可恢复 Export 中并完成移交登记；计入持久积压。
    pub(super) persisted: bool,
    /// 已登记移交且 claim 已释放（另一进程可领取）；仅当本地清理槽无法归还时才继续保留，
    /// 保留期间既不重发也不重复记账，只为不让槽转 Pending 而被本地删除。
    pub(super) handed_off: bool,
    pub(super) due_at_ms: Option<i64>,
    /// 批次内最小事件 seq（活前缀水位）；`None` 表示恢复条目，不属于任何活前缀。
    pub(super) prefix_seq: Option<u64>,
}

/// 已到处置时刻：TTL 到期或到了下次允许发送时间。
fn actionable(entry: &RetryEntry, now_ms: i64) -> bool {
    entry.expires_at_ms <= now_ms || entry.due_at_ms.is_some_and(|due| due <= now_ms)
}

/// 关闭移交的分类（§5.2：只有正常移交点产生持久积压）：`handed_off_events` 已持久为 `retry_wait`、
/// claim 已释放（另一进程可领取），`local_pending_events` 按 `Pending` 报告。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct HandOff {
    pub handed_off_events: u64,
    pub local_pending_events: u64,
}

/// 移交计划：`entries` 是可移交条目——调用方必须**先记账再**调用 [`RetryQueue::release_hand_off`]；
/// 其余条目留在队列里，未收敛的 payload 与清理槽仍归本 owner（关闭期限到期也不丢）。
#[derive(Default)]
pub(super) struct HandOffPlan {
    pub(super) hand_off_events: u64,
    pub(super) local_pending_events: u64,
    entries: Vec<RetryEntry>,
}

impl HandOffPlan {
    /// 计划中的可移交事件数（登记前读取）。
    pub(super) fn hand_off_events(&self) -> u64 {
        self.hand_off_events
    }
}

/// 重试等待队列；唯一 worker 持有，一次调度一个唤醒，不为每条建 timer/task。
#[derive(Default)]
pub(super) struct RetryQueue {
    entries: Vec<RetryEntry>,
}

impl RetryQueue {
    pub(super) fn push(&mut self, entry: RetryEntry) {
        self.entries.push(entry);
    }

    /// 是否存在需要立刻处置的条目（TTL 到期或已到下次允许时间）。
    pub(super) fn has_actionable(&self, now_ms: i64) -> bool {
        self.entries.iter().any(|entry| actionable(entry, now_ms))
    }

    /// 最近一次到期时间（不含待修复条目）；`None` 表示无需唤醒。
    pub(super) fn next_due_ms(&self) -> Option<i64> {
        self.entries
            .iter()
            .filter_map(|entry| entry.due_at_ms)
            .min()
    }

    /// 前缀水位内是否仍有待交付的条目：恢复条目没有本进程 seq，不拖累任何活前缀的屏障。
    pub(super) fn unsettled_upto(&self, boundary: u64) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.prefix_seq.is_some_and(|seq| seq <= boundary))
    }

    /// 取下一条可处置条目：TTL 先到的超期条目优先，其次到期条目。
    pub(super) fn take_actionable(&mut self, now_ms: i64) -> Option<RetryEntry> {
        let expired = |entry: &RetryEntry| entry.expires_at_ms <= now_ms;
        let index = self.entries.iter().position(expired).or_else(|| {
            self.entries
                .iter()
                .position(|entry| actionable(entry, now_ms))
        })?;
        Some(self.entries.swap_remove(index))
    }

    /// 在队条目的最早接受时刻；空队列为 `None`（年龄不靠猜测）。
    pub(super) fn oldest_accepted_ms(&self) -> Option<i64> {
        self.entries.iter().map(|entry| entry.accepted_at_ms).min()
    }

    /// 队列是否已无可处置条目（保留的未收敛条目仍算在队）。
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 计划移交：分出持久且可恢复（有下次允许时间）的文件正文条目；
    /// 内存 payload、持久状态待修复条目、恢复条目与已移交条目都留在队列里——不丢 payload、不丢清理槽。
    pub(super) fn plan_hand_off(&mut self) -> HandOffPlan {
        let mut plan = HandOffPlan::default();
        let mut retained = Vec::with_capacity(self.entries.len());
        for entry in self.entries.drain(..) {
            let handoffable = !entry.handed_off
                && entry.persisted
                && entry.due_at_ms.is_some()
                && matches!(entry.payload.body, PayloadBody::File(_));
            if handoffable {
                plan.hand_off_events = plan.hand_off_events.saturating_add(entry.events);
                plan.entries.push(entry);
            } else if entry.handed_off {
                // 已登记移交、claim 已释放：不再是本地待交付，也不再登记一次。
                retained.push(entry);
            } else {
                plan.local_pending_events = plan.local_pending_events.saturating_add(entry.events);
                retained.push(entry);
            }
        }
        self.entries = retained;
        plan
    }

    /// 释放已登记的移交条目：解锁 claim（另一进程可领取），并尽量归还本地清理槽。
    /// 清理登记取不出来（`SpoolObject` 的 Drop 必须归还句柄）时用同一注册表的新登记替换原登记，
    /// 再 `disarm_after_handoff` 替换出来的原登记：本地槽归还、磁盘配额与正文留给下一 owner。
    /// 注册表已满时仍解锁，但保留 payload —— 丢弃它会让槽转 Pending，下一次收敛就会删掉刚
    /// 移交的正文；保留的条目不再发送、不再记账。
    /// 调用点必须在最后一次有界清理之后，且登记由调用方在解锁前完成。
    pub(super) fn release_hand_off(
        &mut self,
        plan: HandOffPlan,
        registry: Option<&CleanupRegistry>,
    ) -> HandOff {
        let mut report = HandOff {
            handed_off_events: 0,
            local_pending_events: plan.local_pending_events,
        };
        for mut entry in plan.entries {
            let PayloadBody::File(object) = &mut entry.payload.body else {
                report.local_pending_events =
                    report.local_pending_events.saturating_add(entry.events);
                self.entries.push(entry);
                continue;
            };
            drop(object.take_claim());
            entry.handed_off = true;
            report.handed_off_events = report.handed_off_events.saturating_add(entry.events);
            match registry.and_then(|registry| registry.try_register().ok()) {
                Some(placeholder) => {
                    let registration = std::mem::replace(&mut object.cleanup, placeholder);
                    registration.disarm_after_handoff();
                }
                None => {
                    warn!(
                        events = entry.events,
                        "telemetry handoff could not return its local cleanup slot; payload retained"
                    );
                    self.entries.push(entry);
                }
            }
        }
        report
    }
}

/// 一次提交单元的结果：只有 `Delivered` 是交付。
pub(super) enum Delivery {
    Delivered,
    /// 只落入可恢复 Export（或保留内存 payload）：事件仍是 pending，等待下一次发送。
    Retry {
        payload: ExportPayload,
        due_at_ms: i64,
        error: LangfuseError,
    },
    /// 无法确认/修复持久状态：保留 payload 与 claim，禁止继续发送，也不报告完整。
    Unresolved {
        payload: ExportPayload,
        error: LangfuseError,
    },
    /// 终态失败：永久错误、周期耗尽或 TTL 先到。
    Failed {
        error: LangfuseError,
        expired: bool,
    },
}

impl Delivery {
    /// 非 TTL 超期的终态失败：显式错误，不构造任何成功结果。
    pub(super) fn failed(error: LangfuseError) -> Self {
        Self::Failed {
            error,
            expired: false,
        }
    }
}

/// 在途发送任务的统一结果；`persisted` 表示事件已登记为持久积压，`accepted_at_ms` 取批次内最早接受时刻。
pub(super) struct Submission {
    pub events: u64,
    pub accepted_at_ms: i64,
    pub expires_at_ms: i64,
    pub persisted: bool,
    /// 批次内最小事件 seq；`None` 表示恢复条目，不属于任何活前缀。
    pub prefix_seq: Option<u64>,
    pub outcome: Delivery,
}

/// 周期结果的分类（不含 payload 所有权）：决定持久终态、重试等待与终态失败。
#[derive(Clone, Copy)]
enum Classified {
    Delivered,
    Retry { due_at_ms: i64 },
    Unresolved,
    Failed { expired: bool },
}

/// 批次编码（blocking job）；失败把事件与来源交回调用方按未提交处置。
// 每个参数都是该批次各自的依赖或输入，彼此没有共同身份；打包成结构体只是搬运同一批值。
#[allow(clippy::too_many_arguments)]
pub(super) async fn encode_batch(
    events: Vec<TelemetryEvent>,
    telemetry: TelemetryConfig,
    export: ExportConfig,
    budget: Arc<MemoryBudget>,
    spool: Option<Arc<dyn SpoolStore>>,
    context: ExportContext,
    cleanup: CleanupRegistration,
    counters: Arc<Counters>,
) -> Result<ExportPayload, LangfuseError> {
    let max_wire_bytes = export.max_wire_bytes;
    tokio::task::spawn_blocking(move || {
        let writer =
            BudgetedWriter::new(&telemetry, budget, spool, context, max_wire_bytes, cleanup)?;
        let payload = export::encode(&events, writer, &export)?;
        // 原文/线包累计字节按本进程产生或接手的 payload 计一次，重试不重复累计。
        counters.account_payload(payload.raw_len, payload.wire_len);
        Ok(payload)
    })
    .await
    .map_err(|_| LangfuseError::IngestionApi("OTLP batch encoding job did not complete".into()))?
}

/// 一次发送周期：live 批次与恢复对象共用，只有 `Delivered` 才能被当成交付。文件路径先把终态写入
/// attempt 的同一路径（写不进去即待修复），重试先持久 `retry_wait` 再归回同一句柄，丢句柄即显式失败。
pub(super) async fn send_export(
    client: &LangfuseClient,
    payload: ExportPayload,
    expires_at_ms: i64,
    export: &ExportConfig,
) -> Delivery {
    let PayloadBody::File(object) = &payload.body else {
        let mut checkpoint = send::MemoryCheckpoint::new();
        let outcome = client.send_payload(&payload, &mut checkpoint).await;
        let classified = classify(&outcome, expires_at_ms, export);
        return delivery_of(outcome, payload, classified);
    };
    let Some(claim) = object.take_claim() else {
        // 句柄不在本对象手里（并发误用）：按不可修复的终态失败处理，不发送不完整正文。
        return Delivery::failed(LangfuseError::Spool(SpoolError::Busy));
    };
    let mut checkpoint = send::FileCheckpoint::new(claim);
    let outcome = client.send_payload(&payload, &mut checkpoint).await;
    let classified = classify(&outcome, expires_at_ms, export);
    // 只有交付与终态失败写持久终态（判定见 `SendOutcome::terminal_state`）；重试等待与待修复保持原状。
    let state = match classified {
        Classified::Retry { .. } | Classified::Unresolved => None,
        _ => Some(outcome.terminal_state()),
    };
    let terminal = match state {
        Some(state) => checkpoint.checkpoint_terminal(state).await.err(),
        None => None,
    };
    let Ok(mut claim) = checkpoint.into_claim().await else {
        return Delivery::failed(LangfuseError::Spool(SpoolError::Unavailable));
    };
    if let Some(error) = terminal {
        // 句柄归位，正文与终态意图都留给下一轮：既不删除，也不报告交付。
        let _ = object.return_claim(claim);
        return Delivery::Unresolved { payload, error };
    }
    if let Classified::Retry { due_at_ms } = classified {
        let mut meta = claim.meta().clone();
        // 首次接受时间、TTL 与累计尝试保持原样：只更新状态与下次允许时间。
        meta.state = SpoolState::RetryWait;
        meta.next_attempt_at_ms = Some(due_at_ms);
        match checkpoint_claim(claim, meta).await {
            (Ok(()), Some(returned)) => claim = returned,
            (result, released) => {
                if let Some(released) = released {
                    let _ = object.return_claim(released);
                }
                let error = result.err().unwrap_or_else(lost_claim);
                return Delivery::Unresolved { payload, error };
            }
        }
    }
    if let Err(error) = object.return_claim(claim) {
        // 句柄归还不能作为交付；归位失败即显式终态，事件仍是 pending。
        return Delivery::failed(LangfuseError::Spool(error));
    }
    delivery_of(outcome, payload, classified)
}

/// 周期结果 → 分类；重试等待按 `max(Retry-After, 指数退避)` 计算下次允许时间。
fn classify(outcome: &send::SendOutcome, expires_at_ms: i64, export: &ExportConfig) -> Classified {
    match outcome {
        send::SendOutcome::Delivered => Classified::Delivered,
        send::SendOutcome::Retryable {
            retry_after,
            attempts,
            ..
        } => match retry_due_at_ms(super::wall_ms(), *retry_after, *attempts, export) {
            // TTL 先到：不为一次注定过期的等待建重试。
            Ok(due_at_ms) if due_at_ms < expires_at_ms => Classified::Retry { due_at_ms },
            Ok(_) => Classified::Failed { expired: true },
            Err(_) => Classified::Failed { expired: false },
        },
        send::SendOutcome::Terminal { .. } => Classified::Failed { expired: false },
        send::SendOutcome::CheckpointFailed { .. } => Classified::Unresolved,
    }
}

/// 分类与周期错误合成交付结果；分类决定 payload 是留下等续发还是终结。
fn delivery_of(
    outcome: send::SendOutcome,
    payload: ExportPayload,
    classified: Classified,
) -> Delivery {
    let error = || match outcome.into_error() {
        Some(error) => error,
        None => LangfuseError::IngestionApi("OTLP export cycle reported no delivery error".into()),
    };
    match classified {
        Classified::Delivered => Delivery::Delivered,
        Classified::Retry { due_at_ms } => Delivery::Retry {
            payload,
            due_at_ms,
            error: error(),
        },
        Classified::Unresolved => Delivery::Unresolved {
            payload,
            error: error(),
        },
        Classified::Failed { expired } => Delivery::Failed {
            error: error(),
            expired,
        },
    }
}

/// 下一次允许发送时间：`max(Retry-After, 指数退避)`，封顶 `max_retry_delay`；溢出显式失败。
pub(super) fn retry_due_at_ms(
    now_ms: i64,
    retry_after: Duration,
    attempts: u64,
    export: &ExportConfig,
) -> Result<i64, LangfuseError> {
    let shift = attempts.saturating_sub(1).min(31) as u32;
    let backoff = export
        .initial_retry_delay
        .checked_mul(1u32 << shift)
        .unwrap_or(export.max_retry_delay)
        .min(export.max_retry_delay);
    let wait_ms = i64::try_from(backoff.max(retry_after).as_millis()).unwrap_or(i64::MAX);
    let overflow = || LangfuseError::Config("telemetry retry deadline arithmetic overflows".into());
    now_ms.checked_add(wait_ms).ok_or_else(overflow)
}

/// 用取回的独占句柄持久写一次描述符；无论成败都返回同一句柄（`None` 表示句柄丢失）。
async fn checkpoint_claim(
    claim: Box<dyn ObjectClaim>,
    meta: ObjectMeta,
) -> (Result<(), LangfuseError>, Option<Box<dyn ObjectClaim>>) {
    let slot = Arc::new(Mutex::new(Some(claim)));
    let held = Arc::clone(&slot);
    let job = tokio::task::spawn_blocking(move || {
        let mut guard = held.lock().map_err(|_| lost_claim())?;
        let mut claim = guard.take().ok_or_else(lost_claim)?;
        let result = claim.checkpoint(meta).map_err(LangfuseError::Spool);
        *guard = Some(claim);
        result
    })
    .await;
    let claim = slot.lock().unwrap_or_else(|p| p.into_inner()).take();
    // 任务未完成时句柄仍在槽里：显式失败并把它交回调用方。
    (job.unwrap_or_else(|_join| Err(lost_claim())), claim)
}

/// 句柄丢失（槽被毒化或任务未完成）：显式失败，正文与终态意图留在盘上，不冒充交付。
fn lost_claim() -> LangfuseError {
    LangfuseError::IngestionApi("telemetry spool claim was lost".into())
}

/// 超期条目：先持久 `TerminalFailed`（不是 HTTP 成功）再交清理协议删除；写不进去则退回条目，
/// 对象与 claim 都保留，标记待修复、停止自动重试。
// 退回的条目（含 payload 与 claim）与错误一起交出，Err 因此偏大；这是把条目交回调用方的
// 唯一通道，装箱只会给错误路径加一次分配，不改变语义。
#[allow(clippy::result_large_err)]
pub(super) async fn expire_entry(entry: RetryEntry) -> Result<(), (RetryEntry, LangfuseError)> {
    let PayloadBody::File(object) = &entry.payload.body else {
        return Ok(());
    };
    let Some(claim) = object.take_claim() else {
        return Err((entry, LangfuseError::Spool(SpoolError::Busy)));
    };
    let mut meta = claim.meta().clone();
    meta.state = SpoolState::TerminalFailed;
    meta.next_attempt_at_ms = None;
    let (result, released) = checkpoint_claim(claim, meta).await;
    let Some(returned) = released else {
        return Err((entry, result.err().unwrap_or_else(lost_claim)));
    };
    if let Err(error) = result {
        // 句柄归位后保留对象：不删除正文、不冒充终态。
        let _ = object.return_claim(returned);
        return Err((entry, error));
    }
    object
        .return_claim(returned)
        .map_err(|error| (entry, LangfuseError::Spool(error)))
}
