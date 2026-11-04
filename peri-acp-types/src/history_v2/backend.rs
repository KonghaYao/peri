//! History V2 存储读取端口（设计 §9.3、§10.2）。
//!
//! 这是协议服务层与持久化后端之间唯一的有界读取契约：open 创建快照，page/seek 在
//! 固定快照内读取一页，body 读取已交付描述符的可见表示范围。后端**不**接触
//! windowId/readTicket/客户端 cursor；窗口、票据、幂等与预算由 `peri-acp` 的服务层
//! 负责，后端只看到规范化后的原始 cursor 与快照身份。
//!
//! 端口刻意不做隐式重试或降级：每个失败都必须映射为 [`HistoryBackendError`]，由服务层
//! 转成带 `reason`/`recovery` 的领域错误。后端也不得在返回页时自动扩大预算。
//!
//! ## `historyRevision` 的语义
//!
//! 本端口把 `historyRevision` 定义为**存储序 tail position**（[`HistoryCommittedTailRef::position`]；
//! 空历史为 0），不是持久结构版本，也不是跨实例失效凭证。它只回答「这次读取是否仍在
//! open 时固定的 tail 上」：append 不改变它，tail 行被删除/改写则复核失败。跨实例
//! 的一致性与结构失效不在本后端承诺内；当前只有本地 SQLite 开启该能力。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::body::{HistoryEncoding, HistoryRepresentation};
use super::error::{HistoryErrorReason, HistoryRecovery};
use super::integer::DecimalU64;
use super::page::{HistoryCoverage, HistoryEntry, HistoryProgress};
use super::request::{HistoryAnchor, HistoryDirection, HistoryEntryAnchor};

/// 存储可复核的提交尾巴：稳定条目身份 + 存储序位置。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCommittedTailRef {
    /// 稳定条目身份；adapter 负责把它解析回存储条目。
    pub entry_id: String,
    /// 存储序 tail position；空历史没有 tail。
    pub position: u64,
}

/// 一次读取所绑定的不可变快照身份。
///
/// 服务层在 open 成功时固定该值；后续 page/seek/body 只把它原样交给后端，并在返回后
/// 用它复核 tail/revision/projection，避免把不同快照的页拼接起来。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistorySnapshotRef {
    pub session_id: String,
    /// open 时可见的持久提交尾巴；`None` = 空历史。
    pub committed_tail: Option<HistoryCommittedTailRef>,
    /// 存储序 tail position；空历史为 0。不是持久结构版本。
    pub history_revision: DecimalU64,
    /// 展示投影版本。
    pub projection_version: u32,
    /// open 时固定的页面字节预算；page/seek 必须沿用，不得临时扩大。
    pub target_bytes: u64,
}

/// 有界页面数据；cursor 是后端原始值，服务层负责包装为 opaque cursor。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryPageData {
    pub entries: Vec<HistoryEntry>,
    pub coverage: HistoryCoverage,
    pub progress: HistoryProgress,
    /// `None` 表示已证明该方向的快照逻辑范围耗尽。
    pub before_cursor: Option<String>,
    pub after_cursor: Option<String>,
    /// 本次页仍然固定到的提交尾巴；必须与快照一致。
    pub committed_tail: Option<HistoryCommittedTailRef>,
    pub history_revision: DecimalU64,
    pub projection_version: u32,
}

/// open 的结果：快照身份 + 首屏。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryOpenedSnapshot {
    pub history_revision: DecimalU64,
    pub committed_tail: Option<HistoryCommittedTailRef>,
    pub projection_version: u32,
    pub page: HistoryPageData,
}

/// 一段已获准展示的正文；`data` 是 `encoding` 指定的 wire 编码。
///
/// 后端必须保证 `offset` 与请求一致，且 `complete`/`next_offset`/`total_bytes`
/// 满足设计 §7.4 的推进规则；服务层会再次校验形状。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryBodySlice {
    pub body_ref: String,
    pub content_version: String,
    pub representation: HistoryRepresentation,
    pub encoding: HistoryEncoding,
    pub media_type: String,
    pub offset: DecimalU64,
    pub data: String,
    pub next_offset: Option<DecimalU64>,
    pub complete: bool,
    pub total_bytes: DecimalU64,
}

/// 存储后端的类型化失败。
///
/// 区分「快照失效」（必须重开窗口）与「暂时不可用」（同 cursor 退避重试）。服务层
/// 原样采用这里给出的 reason/recovery，不自行猜测更乐观的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryBackendError {
    PermissionDenied,
    NotFound,
    IndexNotReady,
    SourceUnavailable,
    SnapshotInvalidated,
    InvalidCursor,
    AnchorUnavailable,
    BudgetExceeded,
    BackendUnavailable,
    Timeout,
    Busy,
}

impl HistoryBackendError {
    /// 映射到 wire 的领域 `reason`。
    pub fn reason(self) -> HistoryErrorReason {
        match self {
            Self::PermissionDenied => HistoryErrorReason::PermissionDenied,
            Self::NotFound => HistoryErrorReason::NotFound,
            Self::IndexNotReady => HistoryErrorReason::IndexNotReady,
            Self::SourceUnavailable => HistoryErrorReason::SourceUnavailable,
            Self::SnapshotInvalidated => HistoryErrorReason::SnapshotInvalidated,
            Self::InvalidCursor => HistoryErrorReason::InvalidCursor,
            Self::AnchorUnavailable => HistoryErrorReason::AnchorUnavailable,
            Self::BudgetExceeded => HistoryErrorReason::BudgetExceeded,
            Self::BackendUnavailable => HistoryErrorReason::BackendUnavailable,
            Self::Timeout => HistoryErrorReason::Timeout,
            Self::Busy => HistoryErrorReason::Busy,
        }
    }

    /// 客户端恢复建议。
    pub fn recovery(self) -> HistoryRecovery {
        match self {
            Self::PermissionDenied
            | Self::NotFound
            | Self::InvalidCursor
            | Self::AnchorUnavailable => HistoryRecovery::None,
            Self::SnapshotInvalidated => HistoryRecovery::Reopen,
            Self::BudgetExceeded => HistoryRecovery::ReduceScope,
            Self::IndexNotReady
            | Self::SourceUnavailable
            | Self::BackendUnavailable
            | Self::Timeout
            | Self::Busy => HistoryRecovery::Retry,
        }
    }

    /// 安全的固定诊断文本；不包含 cursor、正文或会话内容。
    pub fn message(self) -> &'static str {
        match self {
            Self::PermissionDenied => "History read is not permitted",
            Self::NotFound => "History session was not found",
            Self::IndexNotReady => "History index is not ready",
            Self::SourceUnavailable => "History source is unavailable",
            Self::SnapshotInvalidated => "History snapshot is no longer valid",
            Self::InvalidCursor => "History cursor is invalid",
            Self::AnchorUnavailable => "History anchor is unavailable",
            Self::BudgetExceeded => "History read budget was exceeded",
            Self::BackendUnavailable => "History backend is unavailable",
            Self::Timeout => "History backend timed out",
            Self::Busy => "History backend is busy",
        }
    }
}

/// History V2 的有界存储读取端口。
///
/// 约束：
/// - 所有 `&self` 调用可并发；实现不得依赖调用方持有的全局锁；
/// - `page` 的 `raw_cursor` 只由本服务签发，方向已由服务校验；
/// - 返回的 `HistoryPageData` 必须属于传入快照；tail/revision 不一致时服务层会丢弃并
///   失效窗口；
/// - 后端失败必须显式返回，不得返回空页冒充成功。
#[async_trait]
pub trait HistoryV2Backend: Send + Sync {
    /// 创建浏览快照并返回首屏。`anchor` 是客户端锚点，不包含任何服务层票据。
    async fn open(
        &self,
        session_id: &str,
        anchor: &HistoryAnchor,
        before: u32,
        after: u32,
        target_bytes: u64,
    ) -> Result<HistoryOpenedSnapshot, HistoryBackendError>;

    /// 沿后端原始 cursor 读取一页；不包含边界条目。
    async fn page(
        &self,
        snapshot: &HistorySnapshotRef,
        raw_cursor: &str,
        direction: HistoryDirection,
        limit: u32,
    ) -> Result<HistoryPageData, HistoryBackendError>;

    /// 在固定快照内按稳定条目身份定位。
    async fn seek(
        &self,
        snapshot: &HistorySnapshotRef,
        anchor: &HistoryEntryAnchor,
        before: u32,
        after: u32,
    ) -> Result<HistoryPageData, HistoryBackendError>;

    /// 读取已交付描述符的一段可见正文。
    async fn body(
        &self,
        snapshot: &HistorySnapshotRef,
        body_ref: &str,
        offset: u64,
        max_bytes: u64,
    ) -> Result<HistoryBodySlice, HistoryBackendError>;
}
