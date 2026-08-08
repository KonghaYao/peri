//! 权限请求 CAS 原语（pending → resolved 原子一次；§7.4 规则 4 / §5.4）。

use yrs::{Map, WriteTxn};

use acp_hub_proto::action::PermissionDecision;
use acp_hub_proto::schema::{PermissionOptions, PermissionStatus};

use crate::state::doc_pair::DocPair;
use crate::state::factory::ROOT;
use crate::state::view_store::TransactionCtx;

/// 权限请求 CAS 原语（pending → resolved 原子一次；§7.4 规则 4）。
///
/// 供两条路径共用（同一单写者通道内执行，无并发窗口）：
///  - 聚合器处理 `PermissionResolved`/`PermissionExpired` 事件（ACP 流）；
///  - 控制路径 `DocCommand::ResolvePermission`/`ExpirePermission`（客户端应答 /
///    定时器）。
///
/// 判定性时间戳由 server 权威时钟（§4.7）：`expires_at` 生成与「是否过期」判定
/// 都在定时器/命令路径（F7），本原语只做状态迁移，不读时钟（保持纯函数可测）。
///
/// CAS 迁移结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CasOutcome {
    /// pending → resolved/expired 原子迁移成功（唯一一次；调用方此刻才向 ACP
    /// 发 permission.resolve）。
    Migrated,
    /// 已 resolved/已 expired：幂等返回（`duplicate` ack 语义，§4.4）。
    Duplicate,
    /// 已过期（expires_at < now 判定在定时器路径，§4.7；CAS 不重复判定时间）。
    Expired,
    /// 无此 permission_id。
    Unknown,
}

/// resolve：仅 `pending → resolved` 原子迁移一次，迁移成功后写 decision。
/// 已 resolved → `Duplicate`；已 expired → `Expired`；未知 → `Unknown`。
pub fn resolve(
    pair: &mut DocPair,
    permission_id: &str,
    decision: PermissionDecision,
) -> CasOutcome {
    let mut txn = pair.session_txn();
    let root = txn.get_or_insert_map(ROOT);
    cas_migrate(&mut txn, &root, permission_id, Some(decision))
}

/// expire：仅 `pending → expired`，decision 保持 null（§5.4）。
/// 已 resolved → `Duplicate`（不覆盖已裁决）；已 expired → `Duplicate`（幂等）。
pub fn expire(pair: &mut DocPair, permission_id: &str) -> CasOutcome {
    let mut txn = pair.session_txn();
    let root = txn.get_or_insert_map(ROOT);
    cas_migrate(&mut txn, &root, permission_id, None)
}

fn cas_migrate(
    txn: &mut TransactionCtx<'_>,
    root: &yrs::MapRef,
    permission_id: &str,
    decision: Option<PermissionDecision>,
) -> CasOutcome {
    let perms = root.get_or_init::<_, yrs::MapRef>(txn, "pending_permissions");
    let Some(pm) = perms
        .get(txn, permission_id)
        .and_then(|v| v.cast::<yrs::MapRef>().ok())
    else {
        return CasOutcome::Unknown;
    };
    let status = pm
        .get(txn, "status")
        .and_then(|v| v.cast::<String>().ok())
        .unwrap_or_default();
    match status.as_str() {
        "pending" => {
            match decision {
                Some(d) => {
                    pm.insert(txn, "status", permission_status_str(PermissionStatus::Resolved));
                    pm.insert(txn, "decision", decision_str(d));
                }
                None => {
                    pm.insert(txn, "status", permission_status_str(PermissionStatus::Expired));
                    // decision 保持 null（§5.4）。
                }
            }
            CasOutcome::Migrated
        }
        "resolved" => CasOutcome::Duplicate,
        "expired" => match decision {
            Some(_) => CasOutcome::Expired,
            None => CasOutcome::Duplicate,
        },
        _ => CasOutcome::Unknown,
    }
}

pub(crate) fn permission_status_str(s: PermissionStatus) -> &'static str {
    match s {
        PermissionStatus::Pending => "pending",
        PermissionStatus::Resolved => "resolved",
        PermissionStatus::Expired => "expired",
    }
}

pub(crate) fn decision_str(d: PermissionDecision) -> &'static str {
    match d {
        PermissionDecision::Allow => "allow",
        PermissionDecision::Deny => "deny",
    }
}

pub(crate) fn option_str(o: PermissionOptions) -> &'static str {
    match o {
        PermissionOptions::AllowOnce => "allowOnce",
        PermissionOptions::AllowSession => "allowSession",
        PermissionOptions::Deny => "deny",
    }
}
