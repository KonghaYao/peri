//! 启动恢复：扫描本隔离域的持久对象，可续发的转 [`BatchWorker`] 的统一重试队列，其余交清理协议
//! 幂等删除；同一 worker 执行，不与 live 批次争用第二条发送路径（契约 §5.2、§7 P4a）。

use std::sync::Arc;

use tracing::{debug, error, info, warn};

use super::BatchWorker;
use crate::batcher::recovery::{self, DiscardReason, RetryEntry};
use crate::batcher::shutdown::drain_cleanup;
use crate::batcher::wall_ms;
use crate::payload::{ExportPayload, PayloadBody};
use crate::spool::SpoolObject;

impl BatchWorker {
    pub(super) async fn recover(&mut self) {
        if !self.telemetry.spool_enabled {
            return;
        }
        let (Some(store), Some(registry)) = (self.spool.clone(), self.registry.as_ref()) else {
            return;
        };
        let context = match self.client.export_context(self.telemetry.spool_ttl) {
            Ok(context) => context,
            Err(error) => {
                warn!(error = %error, "telemetry spool recovery skipped: no route context");
                return;
            }
        };
        let now_ms = wall_ms();
        let scan_store = Arc::clone(&store);
        let report = match tokio::task::spawn_blocking(move || {
            recovery::scan(&scan_store, context.route, now_ms)
        })
        .await
        {
            Ok(Ok(report)) => report,
            Ok(Err(error)) => {
                warn!(error = %error, "telemetry spool startup scan failed; disk state left as is");
                return;
            }
            Err(_join) => {
                warn!("telemetry spool startup scan did not complete");
                return;
            }
        };
        let mut resumable = Vec::new();
        for entry in report.entries {
            match entry {
                recovery::Recovered::Resume(claim, meta) => {
                    // 清理槽先于任何本地分配：容量不足时对象保持磁盘原样，下一轮再领。
                    let Ok(registration) = registry.try_register() else {
                        warn!("telemetry spool recovery deferred: no cleanup slot");
                        continue;
                    };
                    if let Err(error) = registration
                        .slot
                        .bind(meta.id, recovery::discard_quota(&meta))
                    {
                        warn!(error = %error, "telemetry spool recovery binding failed");
                        continue;
                    }
                    match SpoolObject::from_claim(Arc::clone(&store), claim, registration) {
                        Ok(object) => resumable.push(object),
                        Err(error) => {
                            warn!(error = %error, "telemetry spool recovery claim rejected")
                        }
                    }
                }
                recovery::Recovered::Discard {
                    meta,
                    claim,
                    reason,
                } => {
                    let quota_bytes = recovery::discard_quota(&meta);
                    let discard = recovery::register_discard(registry, meta.id, quota_bytes, claim);
                    if let Err(error) = discard {
                        warn!(
                            reason = reason.label(),
                            error = %error,
                            "telemetry spool discard deferred to the next recovery"
                        );
                    }
                    match reason {
                        DiscardReason::Corrupt => {
                            self.counters.add_corrupt_object();
                            error!(
                                reason = reason.label(),
                                "telemetry spool object isolated and removed"
                            );
                        }
                        DiscardReason::Expired => {
                            self.counters.add_expired_events(meta.event_count);
                            warn!(
                                reason = reason.label(),
                                "telemetry spool object expired before delivery"
                            );
                        }
                        DiscardReason::Snapshot | DiscardReason::Terminal => {
                            debug!(reason = reason.label(), "telemetry spool object retired")
                        }
                    }
                }
            }
        }
        for object in resumable {
            self.accept_recovered(object);
        }
        if report.foreign_route > 0 {
            warn!(
                objects = report.foreign_route,
                "telemetry spool objects from another route were left untouched"
            );
        }
        drain_cleanup(
            self.registry.as_mut(),
            &self.ledger,
            self.spool.as_ref(),
            &self.counters,
        )
        .await;
        info!(
            scanned = report.scanned,
            "telemetry spool recovery finished"
        );
    }

    /// 恢复对象进入同一重试队列并立即到期：与 live 事件共用同一 worker 与发送单元。
    pub(super) fn accept_recovered(&mut self, object: SpoolObject) {
        let meta = object.meta().clone();
        // 恢复的正文已落盘，不再重新编码；线包/原文字节按本进程接手计一次。
        let payload = ExportPayload {
            body: PayloadBody::File(object),
            encoding: meta.encoding,
            raw_len: meta.raw_len,
            wire_len: meta.stored_len,
            sha256: meta.sha256,
            event_count: meta.event_count,
            budget: Arc::clone(&self.budget),
        };
        self.counters
            .account_payload(payload.raw_len, payload.wire_len);
        self.counters.add_recovered_events(meta.event_count);
        self.ledger.accepted(meta.event_count);
        self.ledger.mark_persisted(meta.event_count);
        info!(
            events = meta.event_count,
            "telemetry spool export recovered for delivery"
        );
        self.retry.push(RetryEntry {
            payload,
            accepted_at_ms: meta.accepted_at_ms,
            expires_at_ms: meta.expires_at_ms,
            events: meta.event_count,
            persisted: true,
            handed_off: false,
            due_at_ms: Some(wall_ms()),
            prefix_seq: None,
        });
    }
}
