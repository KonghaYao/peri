//! inherited 读取的公共结果类型与错误映射。
//!
//! `Unsupported` 表示信封不是 canonical 形状，调用方回退 SQL 语义；blob 读取错误在
//! 这里统一转换为 `SessionResourceError`，避免各调用点重复解释 SQLite 结果码。

use std::sync::Arc;

use peri_acp_types::history_window::{HistoryInheritedSnapshot, HistoryWindowError};

use super::blob_reader::BlobReadError;
use super::inherited_facts::{InheritedLayout, InheritedSummary};
use super::*;

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
}

pub(super) enum LayoutProbe {
    Absent,
    Cached(Arc<InheritedLayout>),
    Parsed(Arc<InheritedLayout>),
    Unsupported,
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
