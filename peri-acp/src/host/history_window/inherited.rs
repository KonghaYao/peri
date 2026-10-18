//! inherited source 的 Host 侧身份规范化与返回结果校验。

use super::*;

/// 将请求里的 inherited source 与 bounds 的冻结快照对齐；own 原样返回。
pub(super) fn canonical_source(
    source: &HistorySource,
    snapshot: Option<&HistoryInheritedSnapshot>,
) -> Result<HistorySource, AcpError> {
    match source {
        HistorySource::Own => Ok(HistorySource::Own),
        HistorySource::Inherited {
            frozen_snapshot_id,
            ancestor_thread_id,
        } => {
            let Some(snapshot) = snapshot else {
                return Err(anchor_unavailable(
                    "inherited prefix is absent in this frozen session",
                ));
            };
            if snapshot.snapshot_id != *frozen_snapshot_id {
                return Err(snapshot_mismatch(
                    "inherited snapshot id does not match the frozen prefix",
                ));
            }
            if let Some(expected) = ancestor_thread_id {
                if snapshot.ancestor_thread_id.as_ref() != Some(expected) {
                    return Err(snapshot_mismatch(
                        "inherited ancestor identity does not match",
                    ));
                }
            }
            Ok(HistorySource::inherited_with_ancestor(
                snapshot.snapshot_id.clone(),
                snapshot.ancestor_thread_id.clone(),
            ))
        }
    }
}

/// 存储返回的 inherited snapshot 必须与 ticket/source 完全一致。
pub(super) fn validate_rows(
    source: &HistorySource,
    rows: &HistoryWindowRows,
) -> Result<(), AcpError> {
    match source {
        HistorySource::Inherited {
            frozen_snapshot_id, ..
        } => {
            if rows.inherited_snapshot_id.as_deref() != Some(frozen_snapshot_id) {
                return Err(snapshot_mismatch(
                    "inherited snapshot changed during the window read",
                ));
            }
        }
        HistorySource::Own => {
            if rows.inherited_snapshot_id.is_some() || !rows.inherited_entries.is_empty() {
                return Err(snapshot_mismatch("own window returned inherited entries"));
            }
        }
    }
    Ok(())
}
