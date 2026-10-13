//! 启动扫描与不可发送对象的清理登记（契约 §5.2）：固定页游标 + 逐对象校验，只续发同 route、未超期的
//! 完整 `Export`（版本/期限/隔离域逐条校验，长度与摘要由端口 `claim` 校验）；其余交清理协议幂等删除，
//! 绝不发送不完整正文。由 [`super`] 重导出给 worker 使用。

use std::sync::Arc;

use crate::error::LangfuseError;
use crate::spool::cleanup::CleanupRegistry;
use crate::spool::{
    ObjectClaim, ObjectId, ObjectKind, ObjectMeta, RouteFingerprint, SpoolError, SpoolState,
    SpoolStore,
};

/// 恢复页大小（契约 §4）。descriptor 上限 4 KiB；页 buffer 上界已由预算的恢复页账单覆盖。
pub(in crate::batcher) const RECOVERY_PAGE_ENTRIES: usize = 64;
const DESCRIPTOR_MAX_BYTES: u64 = 4 * 1024;
/// 描述符版本（契约 §5.2）。
const DESCRIPTOR_VERSION: u16 = 1;

/// 对单个对象的处置：`Resume` 已取回独占 claim 可续发；`Discard` 就地登记清理，其事件数只进统计与诊断。
pub(in crate::batcher) enum Recovered {
    Resume(Box<dyn ObjectClaim>, ObjectMeta),
    Discard {
        meta: ObjectMeta,
        claim: Option<Box<dyn ObjectClaim>>,
        reason: DiscardReason,
    },
}

/// 不可发送对象的清理分类（只进统计与日志）：损坏先隔离诊断再清理，`Snapshot` 是无活跃 owner 的冻结来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::batcher) enum DiscardReason {
    Expired,
    Corrupt,
    Snapshot,
    Terminal,
}

impl DiscardReason {
    pub(in crate::batcher) fn label(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::Corrupt => "corrupt",
            Self::Snapshot => "input-snapshot",
            Self::Terminal => "terminal-residue",
        }
    }
}

/// 一轮启动扫描的结果；`foreign_route` 是隔离域不符而被跳过的对象数（不跨发、不越权删除）。
#[derive(Default)]
pub(in crate::batcher) struct ScanReport {
    pub entries: Vec<Recovered>,
    pub scanned: u64,
    pub foreign_route: u64,
}

/// 单对象占用与 descriptor 上限之和；恢复登记与清理槽绑定共用同一份配额口径。
pub(in crate::batcher) fn discard_quota(meta: &ObjectMeta) -> u64 {
    meta.stored_len.saturating_add(DESCRIPTOR_MAX_BYTES)
}

/// 启动扫描：固定页游标 + 逐对象校验，blocking；端口级错误中止扫描并保留磁盘状态。
pub(in crate::batcher) fn scan(
    store: &Arc<dyn SpoolStore>,
    route: RouteFingerprint,
    now_ms: i64,
) -> Result<ScanReport, SpoolError> {
    let (mut entries, mut cursor) = (Vec::new(), None);
    let mut report = ScanReport::default();
    loop {
        let page = store.list(cursor.clone(), RECOVERY_PAGE_ENTRIES)?;
        for meta in &page.entries {
            report.scanned = report.scanned.saturating_add(1);
            if meta.route != route {
                report.foreign_route = report.foreign_route.saturating_add(1);
                continue;
            }
            let discard = |claim, reason| Recovered::Discard {
                meta: meta.clone(),
                claim,
                reason,
            };
            // 描述符自洽性先看元数据再取锁复核：任一不成立都不发送、不合成成功。
            let refuse = if meta.version != DESCRIPTOR_VERSION
                || meta.accepted_at_ms <= 0
                || meta.expires_at_ms <= meta.accepted_at_ms
                || meta.stored_len == 0
            {
                Some(DiscardReason::Corrupt)
            } else if now_ms >= meta.expires_at_ms {
                Some(DiscardReason::Expired)
            } else if meta.kind == ObjectKind::Input {
                Some(DiscardReason::Snapshot)
            } else if !matches!(
                meta.state,
                SpoolState::Ready | SpoolState::Sending | SpoolState::RetryWait
            ) {
                Some(DiscardReason::Terminal)
            } else {
                None
            };
            if let Some(reason) = refuse {
                entries.push(discard(None, reason));
                continue;
            }
            match store.claim(meta.id) {
                Ok(Some(claim)) => {
                    let claimed = claim.meta().clone();
                    if claimed.version != DESCRIPTOR_VERSION
                        || claimed.id != meta.id
                        || claimed.kind != ObjectKind::Export
                    {
                        report
                            .entries
                            .push(discard(Some(claim), DiscardReason::Corrupt));
                    } else if now_ms >= claimed.expires_at_ms {
                        report
                            .entries
                            .push(discard(Some(claim), DiscardReason::Expired));
                    } else {
                        entries.push(Recovered::Resume(claim, claimed));
                    }
                }
                // 别的 owner 持锁：不是本地候选，磁盘状态保持不动。
                Ok(None) => {}
                Err(SpoolError::Corrupt { .. }) | Err(SpoolError::InvalidMetadata) => {
                    entries.push(discard(None, DiscardReason::Corrupt));
                }
                Err(error) => return Err(error),
            }
        }
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(ScanReport { entries, ..report })
}

/// 不可发送对象交本地清理协议：先占槽再绑定 id，失败不留半登记；槽耗尽则对象保持磁盘原样，绝不丢正文。
pub(in crate::batcher) fn register_discard(
    registry: &CleanupRegistry,
    id: ObjectId,
    quota_bytes: u64,
    claim: Option<Box<dyn ObjectClaim>>,
) -> Result<(), LangfuseError> {
    let mut registration = registry.try_register()?;
    registration.slot.bind(id, quota_bytes)?;
    registration.abandon(claim);
    Ok(())
}
