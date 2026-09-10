//! Ordered transcript persistence worker: batching, barriers and terminal failure.
//! The task owns its receiver and all pending payloads; no transcript guard is held
//! while the store is awaited. Transcript memory/compaction ownership stays above.

use std::sync::Arc;

use anyhow::anyhow;
use peri_acp_types::store::PersistedPayload;

use super::{PersistOp, TranscriptEntry};
use crate::thread::{ThreadId, ThreadStore};

/// 将积压的 Append 批量落库（单次 `append_messages` 调用 → SQLite 单事务）。
///
/// 成功后清空积压；失败时保留 payload 与首个错误，不重试可能部分成功的批次。
async fn flush_appends(
    store: &dyn ThreadStore,
    tid: &ThreadId,
    pending: &mut Vec<PersistedPayload>,
    barrier_error: &mut Option<String>,
    processed: &mut u64,
) {
    if pending.is_empty() {
        return;
    }
    if barrier_error.is_some() {
        return;
    }
    if let Err(e) = store.append_payloads(tid, pending).await {
        tracing::warn!(
            pending = pending.len(),
            "transcript persist entered terminal failure: {e}"
        );
        *barrier_error = Some(e.to_string());
        return;
    }
    *processed = processed.saturating_add(pending.len() as u64);
    pending.clear();
}

pub(super) async fn run_writer(
    store: Arc<dyn ThreadStore>,
    tid: ThreadId,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<PersistOp>,
) {
    let mut processed: u64 = 0;
    let mut last_warn_at: u64 = 0;
    let mut barrier_error = None;
    // 短窗口 Append 合并：把 ≤100ms 窗口（或 ≥APPEND_BATCH_MAX 条）内的
    // Append 积压为一次 `append_messages` 批量调用（SQLite 单事务 = 一次
    // WAL fsync），消除工具消息风暴下每消息一次 fsync。
    //
    // 可见性语义不变：
    // - Barrier 到达时先 flush 积压再 ack（flush_persistence 确认 = 已落库）
    // - 其他 op 到达时先 flush 积压，保持 FIFO 顺序
    // - 通道关闭时 flush 剩余
    const FAILED_PENDING_MAX: usize = 256;
    let mut dropped_after_failure = 0usize;
    let mut pending_appends: Vec<PersistedPayload> = Vec::new();
    let mut window_start: std::time::Instant = std::time::Instant::now();
    const APPEND_BATCH_MAX: usize = 64;
    const APPEND_BATCH_WINDOW: std::time::Duration = std::time::Duration::from_millis(100);

    loop {
        // 失败后的积压仅用于 barrier 诊断，不能继续驱动批处理定时器。
        // 等待新 op 仍允许 sticky Barrier / Shutdown 以及有界失败缓冲处理。
        let op = if pending_appends.is_empty() || barrier_error.is_some() {
            rx.recv().await
        } else {
            let remaining = APPEND_BATCH_WINDOW.saturating_sub(window_start.elapsed());
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(op) => op,
                Err(_) => {
                    // 窗口到期：批量落库后继续等待
                    flush_appends(
                        store.as_ref(),
                        &tid,
                        &mut pending_appends,
                        &mut barrier_error,
                        &mut processed,
                    )
                    .await;
                    continue;
                }
            }
        };

        match op {
            Some(PersistOp::Append(entry)) => {
                if pending_appends.is_empty() {
                    window_start = std::time::Instant::now();
                }
                let payload = match entry {
                    TranscriptEntry::Message(message) => PersistedPayload::Message(message),
                    TranscriptEntry::Reminder { id, reminder } => {
                        PersistedPayload::SystemReminder { id, reminder }
                    }
                };
                if barrier_error.is_some() && pending_appends.len() >= FAILED_PENDING_MAX {
                    dropped_after_failure = dropped_after_failure.saturating_add(1);
                    tracing::warn!(
                        dropped_after_failure,
                        "terminal transcript persistence failure dropped payload"
                    );
                } else {
                    pending_appends.push(payload);
                }
                if pending_appends.len() >= APPEND_BATCH_MAX {
                    flush_appends(
                        store.as_ref(),
                        &tid,
                        &mut pending_appends,
                        &mut barrier_error,
                        &mut processed,
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
                    &mut barrier_error,
                    &mut processed,
                )
                .await;
                let result = barrier_error.as_ref().map_or(Ok(()), |error| {
                    Err(anyhow!(
                        "{error}; {} payload(s) remain unpersisted, {} dropped after terminal failure",
                        pending_appends.len(),
                        dropped_after_failure
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
                    &mut barrier_error,
                    &mut processed,
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
                    &mut barrier_error,
                    &mut processed,
                )
                .await;
                let result = if let Some(error) = barrier_error.as_ref() {
                    Err(anyhow!(error.clone()))
                } else {
                    match other {
                        PersistOp::RewindTo(id) => store.delete_messages_since(&tid, &id).await,
                        PersistOp::UpdateFlags(id, flags) => {
                            store.update_message_flags(&id, &flags).await
                        }
                        PersistOp::ApplyCompactionBatch { updates } => {
                            let mut first_err = None;
                            for (id, flags) in &updates {
                                if let Err(err) = store.update_message_flags(id, flags).await {
                                    if first_err.is_none() {
                                        first_err = Some(err);
                                    }
                                }
                            }
                            // 无论标记更新是否部分失败，均需使缓存失效。
                            if let Err(err) = store.invalidate_context_cache(&tid).await {
                                if first_err.is_none() {
                                    first_err = Some(err);
                                }
                            }
                            first_err.map_or(Ok(()), Err)
                        }
                        PersistOp::Append(_) | PersistOp::Barrier(_) | PersistOp::Shutdown => {
                            unreachable!("handled in dedicated branches above")
                        }
                    }
                };
                if let Err(e) = result {
                    tracing::warn!("transcript persist failed: {e}");
                    if barrier_error.is_none() {
                        barrier_error = Some(e.to_string());
                    }
                }
                processed = processed.saturating_add(1);
            }
        }

        let bucket = processed / 1000;
        if bucket > last_warn_at {
            last_warn_at = bucket;
            tracing::trace!(
                thread_id = %tid,
                processed,
                "transcript persist writer: 已处理 {processed} 条操作"
            );
        }
    }
}
