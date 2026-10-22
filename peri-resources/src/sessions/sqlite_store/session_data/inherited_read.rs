//! inherited 读取的公共结果类型与有界回退门禁。
//!
//! 派生索引是**派生对象**：缺失或与列事实不一致时允许回退全列扫描，但回退本身必须是
//! 有界的。超过 [`INHERITED_FALLBACK_MAX_BYTES`] 或
//! [`INHERITED_FALLBACK_MAX_PAYLOADS`] 的快照必须显式返回 `index_not_ready`，不得在
//! 读请求里隐式扫描整列，也不得伪装成 `NotFound`/空页。

use std::sync::Arc;

use peri_acp_types::history_window::{HistoryInheritedSnapshot, HistoryWindowError};

use super::blob_reader::BlobReadError;
use super::inherited_facts::{InheritedLayout, InheritedSummary};
use super::*;

/// 无派生索引时允许回退扫描的列总字节上限。
pub(super) const INHERITED_FALLBACK_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// 无派生索引时允许回退扫描的 payload 条数上限。
pub(super) const INHERITED_FALLBACK_MAX_PAYLOADS: usize = 4096;

/// summary 读取结果：`Unsupported` 表示信封不是 canonical 形状，调用方回退 SQL。
pub(super) enum InheritedSummaryRead {
    Facts {
        snapshot: Option<HistoryInheritedSnapshot>,
    },
    Unsupported,
}

/// 全量布局读取结果：`Unsupported` 表示信封不是 canonical 形状，调用方回退 SQL。
pub(super) enum InheritedFactsRead {
    Facts {
        snapshot: Option<HistoryInheritedSnapshot>,
        layout: Option<Arc<InheritedLayout>>,
    },
    Unsupported,
}

/// 定位与打开 blob 之间只做一次缓存判定/扫描，结果交给外层分类。
pub(super) enum SummaryProbe {
    Absent,
    Cached(Arc<InheritedSummary>),
    Parsed(Arc<InheritedSummary>),
    Unsupported,
    /// 索引缺失/不完整，且回退扫描会超出有界预算。
    IndexNotReady {
        total_bytes: u64,
        entry_count: Option<usize>,
    },
}

pub(super) enum LayoutProbe {
    Absent,
    Cached(Arc<InheritedLayout>),
    Parsed(Arc<InheritedLayout>),
    Unsupported,
    /// 索引缺失/不完整，且回退扫描会超出有界预算。
    IndexNotReady {
        total_bytes: u64,
        entry_count: Option<usize>,
    },
}

/// 回退扫描是否超出有界预算。`entry_count` 来自索引头；索引完全缺失时只能按字节判断，
/// 但此时扫描本身已被字节上限约束。
pub(super) fn fallback_exceeds_bound(total_bytes: u64, entry_count: Option<usize>) -> bool {
    total_bytes > INHERITED_FALLBACK_MAX_BYTES
        || entry_count.is_some_and(|count| count > INHERITED_FALLBACK_MAX_PAYLOADS)
}

/// 明确的 `index_not_ready`：调用方应提示准备索引/稍后重试，不能改成空页。
pub(super) fn inherited_index_not_ready(
    total_bytes: u64,
    entry_count: Option<usize>,
) -> SessionResourceError {
    let detail = match entry_count {
        Some(count) => format!(
            "inherited derived index is missing or inconsistent for a {total_bytes}-byte \
             snapshot with {count} payloads; bounded fallback is {INHERITED_FALLBACK_MAX_BYTES} \
             bytes / {INHERITED_FALLBACK_MAX_PAYLOADS} payloads"
        ),
        None => format!(
            "inherited derived index is missing for a {total_bytes}-byte snapshot; bounded \
             fallback is {INHERITED_FALLBACK_MAX_BYTES} bytes / \
             {INHERITED_FALLBACK_MAX_PAYLOADS} payloads"
        ),
    };
    SessionResourceError::new(SessionResourceErrorKind::IndexNotReady { detail })
}

pub(super) fn map_inherited_blob_error(error: BlobReadError) -> SessionResourceError {
    match error {
        BlobReadError::OffsetBeyondLength => {
            HistoryWindowError::invalid_request("body chunk offset is beyond the entry length")
                .into()
        }
        BlobReadError::Corrupt(detail) => corrupt(detail),
        BlobReadError::Sqlite(code) => HistoryWindowError::ReadFailed {
            detail: format!("sqlite blob read failed (code {code})"),
        }
        .into(),
        BlobReadError::Transient(detail) => HistoryWindowError::ReadFailed {
            detail: detail.to_owned(),
        }
        .into(),
        BlobReadError::Unsupported => corrupt("inherited context is not incrementally readable"),
    }
}
