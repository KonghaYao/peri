//! Peri History V2 wire 契约（统一历史加载协议，设计 v2）。
//!
//! 本模块只放**纯类型与纯校验**，不接运行路径、不读数据库、不修改 V1 类型：
//!
//! - 协商：`_meta["peri.history"]` 的 client/server capability 与预算；
//! - 首屏：`session/load` 的 `initial` 请求与结果；
//! - 只读 RPC：`_peri/history` 的 open/page/seek/body/close；
//! - 页面：`WindowPage`、条目、subItem、coverage 与 progress；
//! - 错误：JSON-RPC `-32000` 的 `data` 形状。
//!
//! ## wire 约定
//!
//! 大整数一律使用 [`DecimalU64`]（规范十进制字符串）；小整数与计数使用 JSON number。
//! 所有请求/响应/子结构启用 `deny_unknown_fields`。枚举使用 `kind`/`action` tag；
//! 变体内容用 `deny_unknown_fields` 的独立结构承载，因此未知字段不会静默通过。

use serde::{Deserialize, Deserializer, Serialize};

mod backend;
mod body;
mod error;
mod initial;
mod integer;
mod negotiation;
mod page;
mod request;

pub use backend::{
    HistoryBackendError, HistoryBodySlice, HistoryCommittedTailRef, HistoryOpenedSnapshot,
    HistoryPageData, HistorySnapshotRef, HistoryV2Backend,
};
pub use body::{
    HistoryBodyChunk, HistoryBodyDescriptor, HistoryBodyKind, HistoryClosedKind,
    HistoryClosedResult, HistoryEncoding, HistoryRepresentation,
};
pub use error::{
    ExecutionEffect, HistoryContractError, HistoryEffect, HistoryEffectState, HistoryErrorData,
    HistoryErrorReason, HistoryRecovery,
};
pub use initial::{
    HistoryInitialKind, HistoryInitialRequest, HistoryInitialResult, HistoryLiveBoundary,
};
pub use integer::DecimalU64;
pub use negotiation::{
    HistoryClientCapability, HistoryLimits, HistoryServerCapability,
    HISTORY_DEFAULT_BODY_CHUNK_BYTES, HISTORY_DEFAULT_ITEMS, HISTORY_DEFAULT_MAX_RESPONSE_BYTES,
    HISTORY_DEFAULT_MAX_WINDOWS, HISTORY_DEFAULT_TARGET_PAGE_BYTES,
};
pub use page::{
    HistoryAttachmentSubItem, HistoryBoundary, HistoryBoundaryPosition, HistoryBoundarySide,
    HistoryCoverage, HistoryCoverageInterval, HistoryDurability, HistoryDurabilityState,
    HistoryEntry, HistoryNoticeSubItem, HistoryOpaqueSubItem, HistoryOrder, HistoryOrigin,
    HistoryProgress, HistoryProgressReason, HistorySubItem, HistoryTextSubItem,
    HistoryToolCallSubItem, HistoryToolResultSubItem, HistoryWindowKind, HistoryWindowPage,
};
pub use request::{
    HistoryAnchor, HistoryBodyRequest, HistoryCloseRequest, HistoryDirection, HistoryEntryAnchor,
    HistoryInheritedSource, HistoryOpenRequest, HistoryPageRequest, HistoryRequest,
    HistorySeekRequest, HistorySource,
};

/// 当前唯一支持的协议版本。
pub const HISTORY_V2_VERSION: u32 = 2;
/// 当前服务端展示投影版本；投影规则变化时必须提升。
///
/// 它只标识“同一展示口径”，不是存储结构版本；窗口复核要求前后一致。
pub const HISTORY_V2_PROJECTION_VERSION: u32 = 1;
/// `_meta` 中的能力键。
pub const HISTORY_META_KEY: &str = "peri.history";
/// 只读 RPC 方法名。
pub const HISTORY_METHOD: &str = "_peri/history";
/// 历史变化通知方法名。
pub const HISTORY_CHANGED_NOTIFICATION: &str = "_peri/history/changed";
/// 选中的加载模式。
pub const HISTORY_PROFILE_WINDOWED: &str = "windowed";
/// 错误 `data.domain`。
pub const HISTORY_DOMAIN: &str = "peri.history";

/// `maxItems` 的协议上限。
pub const HISTORY_MAX_ITEMS: u32 = 100;
/// 目标页字节下限。
pub const HISTORY_MIN_TARGET_PAGE_BYTES: u64 = 64 * 1024;
/// 目标页字节上限。
pub const HISTORY_MAX_TARGET_PAGE_BYTES: u64 = 512 * 1024;
/// 单个正文字块上限。
pub const HISTORY_MAX_BODY_CHUNK_BYTES: u32 = 256 * 1024;
/// sessionId/entryId/subItemId/invocationKey 的字节上限。
pub const HISTORY_IDENTITY_MAX_BYTES: usize = 512;
/// cursor/readTicket/bodyRef/snapshotToken 的字节上限。
pub const HISTORY_TOKEN_MAX_BYTES: usize = 4 * 1024;

/// 必填但可为 null 的字段：缺失报错，`null` 正常解码。
pub(crate) fn deserialize_required_option<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer)
}

pub(crate) fn require_version(version: u32) -> Result<(), HistoryContractError> {
    if version == HISTORY_V2_VERSION {
        Ok(())
    } else {
        Err(HistoryContractError::UnsupportedVersion)
    }
}

pub(crate) fn require_request_key(request_key: DecimalU64) -> Result<(), HistoryContractError> {
    if request_key.is_zero() {
        return Err(HistoryContractError::invalid_request(
            "requestKey must be at least 1",
        ));
    }
    Ok(())
}

pub(crate) fn require_identity(
    field: &'static str,
    value: &str,
) -> Result<(), HistoryContractError> {
    require_non_empty(field, value)?;
    require_len(field, value, HISTORY_IDENTITY_MAX_BYTES)
}

pub(crate) fn require_token(field: &'static str, value: &str) -> Result<(), HistoryContractError> {
    require_non_empty(field, value)?;
    require_len(field, value, HISTORY_TOKEN_MAX_BYTES)
}

fn require_non_empty(field: &'static str, value: &str) -> Result<(), HistoryContractError> {
    if value.is_empty() {
        return Err(HistoryContractError::invalid_request(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

fn require_len(field: &'static str, value: &str, max: usize) -> Result<(), HistoryContractError> {
    if value.len() > max {
        return Err(HistoryContractError::IdentityTooLong { field });
    }
    Ok(())
}

pub(crate) fn require_item_limit(
    field: &'static str,
    limit: u32,
    limits: &HistoryLimits,
) -> Result<(), HistoryContractError> {
    let max = limits.max_items.min(HISTORY_MAX_ITEMS);
    if limit == 0 || limit > max {
        return Err(HistoryContractError::limits(format!(
            "{field} must be within 1..={max}"
        )));
    }
    Ok(())
}

pub(crate) fn require_latest_items(
    field: &'static str,
    count: u32,
    limits: &HistoryLimits,
) -> Result<(), HistoryContractError> {
    let max = limits.max_items.min(HISTORY_MAX_ITEMS);
    if count == 0 || count > max {
        return Err(HistoryContractError::limits(format!(
            "{field} must be within 1..={max}"
        )));
    }
    Ok(())
}

pub(crate) fn require_around_items(
    label: &'static str,
    before: u32,
    after: u32,
    limits: &HistoryLimits,
) -> Result<(), HistoryContractError> {
    let max = limits.max_items.min(HISTORY_MAX_ITEMS);
    let total = u64::from(before) + u64::from(after) + 1;
    if before > max || after > max || total > u64::from(max) {
        return Err(HistoryContractError::limits(format!(
            "{label} requires before + after + 1 <= {max}"
        )));
    }
    Ok(())
}

pub(crate) fn require_target_bytes(
    target: u64,
    limits: &HistoryLimits,
) -> Result<(), HistoryContractError> {
    if !(HISTORY_MIN_TARGET_PAGE_BYTES..=HISTORY_MAX_TARGET_PAGE_BYTES).contains(&target) {
        return Err(HistoryContractError::limits(format!(
            "targetBytes must be within {HISTORY_MIN_TARGET_PAGE_BYTES}..={HISTORY_MAX_TARGET_PAGE_BYTES}"
        )));
    }
    if target > limits.max_response_bytes {
        return Err(HistoryContractError::limits(
            "targetBytes must not exceed maxResponseBytes",
        ));
    }
    Ok(())
}

/// `_peri/history/changed` 通知参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HistoryChangedNotification {
    pub version: u32,
    pub session_id: String,
    pub window_id: String,
    pub snapshot_token: String,
    pub change: HistoryChange,
    /// 必填，可 null；committed 时是新持久上界。
    #[serde(deserialize_with = "deserialize_required_option")]
    pub latest_tail: Option<DecimalU64>,
    pub history_revision: DecimalU64,
}

/// 通知的变化类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryChange {
    Committed,
    Invalidated,
    Closed,
}

#[cfg(test)]
#[path = "history_v2_test.rs"]
mod tests;
