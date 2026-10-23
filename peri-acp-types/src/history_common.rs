//! 历史读取的共用基础类型：来源/方向、稳定条目身份、durable tail、checkpoint、
//! 冻结继承快照、读取票据、正文描述符/分块与稳定错误分类。
//!
//! 这些类型同时服务于 History V2 适配器与 `SessionResources` 的存储读取端口；
//! 它们只描述**存储事实与有界读取契约**，不携带 `sessionGeneration`、窗口游标或
//! 连接级票据签名——那些属于已删除的旧窗口协议。
//!
//! ## 身份与失效
//!
//! - 条目的稳定身份是 [`HistoryEntryId`]（`{会话}:{message_id}`）；`position`/`rowid`
//!   只是同一快照内的排序键，不能作为稳定身份。
//! - [`HistoryCommittedTail`] 固定一次读取的持久上界；身份/位置复核失败按
//!   [`HistoryWindowError::SnapshotMismatch`] 处理。
//! - [`HistoryReadTicket`] 是不透明短期票据；本模块只承载值，签名与撤销由 Host 的
//!   连接/会话事实负责。票据内容不进入 `Debug`、日志或错误文本。

use serde::{Deserialize, Serialize};

use crate::messages::MessageId;
use crate::session_resources::{SessionResourceError, SessionResourceErrorKind};
use crate::thread::ThreadId;

/// 窗口投影/预算口径版本。
pub const HISTORY_WINDOW_PROJECTION_VERSION: u16 = 1;
/// 服务端默认单页行数。
pub const HISTORY_WINDOW_DEFAULT_LIMIT: u16 = 50;
/// 单页行数上限。
pub const HISTORY_WINDOW_MAX_LIMIT: u16 = 100;
/// 服务端默认页字节预算。
pub const HISTORY_WINDOW_DEFAULT_BYTE_BUDGET: u32 = 512 * 1024;
/// 客户端可请求的页字节预算上限。
pub const HISTORY_WINDOW_MAX_BYTE_BUDGET: u32 = 4 * 1024 * 1024;
/// 正文分块读取的默认块大小。
pub const HISTORY_WINDOW_DEFAULT_CHUNK_SIZE: u32 = 64 * 1024;
/// 正文分块读取的单次上限。
pub const HISTORY_WINDOW_MAX_CHUNK_SIZE: u32 = 256 * 1024;

// ─── 来源与方向 ────────────────────────────────────────────────────────────────

/// 历史来源。`Inherited` 必须有冻结快照身份，不能拿父会话当前历史冒充子会话来源。
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistorySource {
    Own,
    Inherited {
        #[serde(rename = "frozenSnapshotId")]
        frozen_snapshot_id: String,
        /// 冻结前缀的直接祖先会话 id。旧 frozen 事实没有该字段时为 `None`，不伪造。
        #[serde(
            rename = "ancestorThreadId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        ancestor_thread_id: Option<ThreadId>,
    },
}

impl HistorySource {
    pub fn own() -> Self {
        Self::Own
    }

    pub fn inherited(frozen_snapshot_id: impl Into<String>) -> Self {
        Self::Inherited {
            frozen_snapshot_id: frozen_snapshot_id.into(),
            ancestor_thread_id: None,
        }
    }

    pub fn inherited_with_ancestor(
        frozen_snapshot_id: impl Into<String>,
        ancestor_thread_id: Option<ThreadId>,
    ) -> Self {
        Self::Inherited {
            frozen_snapshot_id: frozen_snapshot_id.into(),
            ancestor_thread_id,
        }
    }

    pub fn is_own(&self) -> bool {
        matches!(self, Self::Own)
    }

    pub fn frozen_snapshot_id(&self) -> Option<&str> {
        match self {
            Self::Own => None,
            Self::Inherited {
                frozen_snapshot_id, ..
            } => Some(frozen_snapshot_id),
        }
    }

    pub fn ancestor_thread_id(&self) -> Option<&ThreadId> {
        match self {
            Self::Own => None,
            Self::Inherited {
                ancestor_thread_id, ..
            } => ancestor_thread_id.as_ref(),
        }
    }
}

/// 窗口读取方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryWindowDirection {
    Around,
    Before,
    After,
}

impl HistoryWindowDirection {
    pub fn reads_before(self) -> bool {
        matches!(self, Self::Around | Self::Before)
    }

    pub fn reads_after(self) -> bool {
        matches!(self, Self::Around | Self::After)
    }
}

// ─── 稳定身份、持久边界与 checkpoint ──────────────────────────────────────────

/// 稳定条目身份：`{会话}:{message_id}`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntryId {
    pub thread_id: ThreadId,
    pub message_id: MessageId,
}

impl HistoryEntryId {
    pub fn new(thread_id: &ThreadId, message_id: MessageId) -> Self {
        Self {
            thread_id: thread_id.clone(),
            message_id,
        }
    }

    pub fn encode(&self) -> String {
        format!("{}:{}", self.thread_id, self.message_id.as_uuid())
    }

    pub fn parse(raw: &str) -> Result<Self, HistoryWindowError> {
        let (thread, message) = raw.rsplit_once(':').ok_or_else(|| {
            HistoryWindowError::invalid_request("entryId must be `{session}:{message}`")
        })?;
        if thread.is_empty() || message.is_empty() {
            return Err(HistoryWindowError::invalid_request(
                "entryId must be `{session}:{message}`",
            ));
        }
        let message_id = uuid::Uuid::parse_str(message)
            .map(MessageId::from)
            .map_err(|_| HistoryWindowError::invalid_request("entryId is not a stable entry id"))?;
        Ok(Self {
            thread_id: thread.to_owned(),
            message_id,
        })
    }

    /// 解析并校验会话归属；别的会话的条目不能被当作本会话的身份。
    pub fn parse_for_session(
        raw: &str,
        expected: &ThreadId,
    ) -> Result<MessageId, HistoryWindowError> {
        let entry = Self::parse(raw)?;
        if entry.thread_id != *expected {
            return Err(HistoryWindowError::AnchorUnavailable {
                detail: "entryId belongs to another session".into(),
            });
        }
        Ok(entry.message_id)
    }
}

/// 一次读取看到的 durable tail：稳定条目身份 + 存储序键。
///
/// 客户端只拿到 [`Self::encode`] 的不透明文本；adapter 只在内部用它固定 snapshot
/// 的读取上界。新 append 的 `rowid` 大于 tail，不进入旧窗口；tail 行被 rewind 删除后
/// 身份/位置复核失败，窗口失效。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryCommittedTail {
    pub thread_id: ThreadId,
    pub entry_id: MessageId,
    pub position: u64,
}

#[derive(Serialize, Deserialize)]
struct WireCommittedTail {
    v: u8,
    session: ThreadId,
    entry: MessageId,
    position: u64,
}

const WIRE_COMMITTED_TAIL_VERSION: u8 = 1;

impl HistoryCommittedTail {
    pub fn new(thread_id: &ThreadId, entry_id: MessageId, position: u64) -> Self {
        Self {
            thread_id: thread_id.clone(),
            entry_id,
            position,
        }
    }

    /// 不透明线上形式。
    pub fn encode(&self) -> String {
        serde_json::to_string(&WireCommittedTail {
            v: WIRE_COMMITTED_TAIL_VERSION,
            session: self.thread_id.clone(),
            entry: self.entry_id,
            position: self.position,
        })
        .expect("committed tail is a JSON object of strings and numbers")
    }

    /// 解析并校验会话归属。
    pub fn decode(raw: &str, expected: &ThreadId) -> Result<Self, HistoryWindowError> {
        let wire: WireCommittedTail =
            serde_json::from_str(raw).map_err(|_| HistoryWindowError::SnapshotMismatch {
                detail: "committedTail is not a valid window token".into(),
            })?;
        if wire.v != WIRE_COMMITTED_TAIL_VERSION {
            return Err(HistoryWindowError::SnapshotMismatch {
                detail: "committedTail version is not supported".into(),
            });
        }
        if wire.session != *expected {
            return Err(HistoryWindowError::CrossSession {
                expected: expected.clone(),
                actual: wire.session,
            });
        }
        Ok(Self {
            thread_id: wire.session,
            entry_id: wire.entry,
            position: wire.position,
        })
    }
}

/// 最近一次可由现有 canonical 行重建的 Full checkpoint。
///
/// `checkpoint_id` 首选该提交追加的 summary payload 的稳定 `entryId`；`position` 与
/// durable tail 绑定。没有可重建提交时为 `None`，不得伪造随机值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryCheckpoint {
    pub checkpoint_id: String,
    pub summary_entry_id: Option<MessageId>,
    pub position: u64,
}

/// 子会话持有的冻结祖先前缀快照。
///
/// `snapshot_id` 由服务端从子会话的 `inherited_context` 稳定事实派生（子会话 id、字节长度、
/// 条目数、首尾稳定 id），不是持久列；父会话之后的 append/compact/rewind 不改变它。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryInheritedSnapshot {
    pub snapshot_id: String,
    /// 冻结前缀的直接祖先会话 id；旧数据没有该事实时为 `None`。
    pub ancestor_thread_id: Option<ThreadId>,
    pub first_entry: Option<MessageId>,
    pub last_entry: Option<MessageId>,
    pub entry_count: usize,
}

impl HistoryInheritedSnapshot {
    pub fn entry_id(&self, child_session_id: &ThreadId, message_id: MessageId) -> String {
        HistoryEntryId::new(child_session_id, message_id).encode()
    }
}

/// 继承区（冻结祖先前缀）的窗口可用性。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryInheritedWindow {
    Absent,
    Present(HistoryInheritedSnapshot),
}

impl HistoryInheritedWindow {
    pub fn is_present(&self) -> bool {
        matches!(self, Self::Present(_))
    }

    pub fn snapshot(&self) -> Option<&HistoryInheritedSnapshot> {
        match self {
            Self::Present(snapshot) => Some(snapshot),
            Self::Absent => None,
        }
    }
}

// ─── 读取票据与正文分块 ────────────────────────────────────────────────────────

/// 不透明读取票据。`Debug` 永远不打印票据内容。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HistoryReadTicket(String);

impl HistoryReadTicket {
    pub fn new(opaque: impl Into<String>) -> Self {
        Self(opaque.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for HistoryReadTicket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HistoryReadTicket(<opaque>)")
    }
}

/// 超大条目的正文描述符；客户端按 `readTicket` 分块读取。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryBodyDescriptor {
    pub total_bytes: u64,
    pub chunk_size: u32,
    pub read_ticket: HistoryReadTicket,
    pub snapshot_token: String,
}

impl HistoryBodyDescriptor {
    pub fn new(
        total_bytes: u64,
        chunk_size: u32,
        snapshot_token: impl Into<String>,
        read_ticket: HistoryReadTicket,
    ) -> Result<Self, HistoryWindowError> {
        if total_bytes == 0 {
            return Err(HistoryWindowError::invalid_request(
                "oversize descriptor requires a positive byte length",
            ));
        }
        if !(1..=HISTORY_WINDOW_MAX_CHUNK_SIZE).contains(&chunk_size) {
            return Err(HistoryWindowError::invalid_request(
                "body chunk size is out of range",
            ));
        }
        Ok(Self {
            total_bytes,
            chunk_size,
            read_ticket,
            snapshot_token: snapshot_token.into(),
        })
    }
}

/// 一段有界正文块。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryBodyChunk {
    pub entry_id: String,
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub next_offset: Option<u64>,
    pub complete: bool,
}

impl HistoryBodyChunk {
    /// 校验块与请求一致：offset 不漂移、块大小不超限、结束标记与 nextOffset 自洽。
    pub fn validate(
        &self,
        expected_entry: &str,
        requested_offset: u64,
        max_bytes: u32,
    ) -> Result<(), HistoryWindowError> {
        if self.entry_id != expected_entry {
            return Err(HistoryWindowError::ReadFailed {
                detail: "body chunk entry identity mismatch".into(),
            });
        }
        if self.offset != requested_offset {
            return Err(HistoryWindowError::ReadFailed {
                detail: "body chunk offset mismatch".into(),
            });
        }
        if self.bytes.len() > max_bytes as usize {
            return Err(HistoryWindowError::ReadFailed {
                detail: "body chunk exceeds the requested bound".into(),
            });
        }
        let end = requested_offset
            .checked_add(self.bytes.len() as u64)
            .ok_or_else(|| HistoryWindowError::ReadFailed {
                detail: "body chunk offset overflow".into(),
            })?;
        if self.complete {
            if self.next_offset.is_some() {
                return Err(HistoryWindowError::ReadFailed {
                    detail: "complete body chunk must not carry nextOffset".into(),
                });
            }
        } else if self.next_offset != Some(end) || self.bytes.is_empty() {
            return Err(HistoryWindowError::ReadFailed {
                detail: "incomplete body chunk has an inconsistent nextOffset".into(),
            });
        }
        Ok(())
    }
}

// ─── 错误码 ────────────────────────────────────────────────────────────────────

/// 历史读取错误：每个变体对应一个稳定的 JSON-RPC code + `data.reason`。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HistoryWindowError {
    #[error("history window protocol is not negotiated")]
    NotNegotiated,

    #[error("invalid history request: {detail}")]
    InvalidRequest { detail: String },

    #[error("session not found")]
    SessionNotFound,

    #[error("session is closing")]
    SessionClosing,

    #[error("history mutation is in progress")]
    MutationInProgress,

    #[error("history cursor or ticket belongs to session {actual}, not {expected}")]
    CrossSession {
        expected: ThreadId,
        actual: ThreadId,
    },

    #[error("history window is invalidated: {detail}")]
    WindowInvalidated { detail: String },

    #[error("history read ticket is invalid: {detail}")]
    ReadTicketInvalid { detail: String },

    #[error("history read ticket has expired")]
    ReadTicketExpired,

    #[error("history snapshot mismatch: {detail}")]
    SnapshotMismatch { detail: String },

    #[error("history anchor is unavailable: {detail}")]
    AnchorUnavailable { detail: String },

    #[error("history read failed: {detail}")]
    ReadFailed { detail: String },
}

impl HistoryWindowError {
    pub fn invalid_request(detail: impl Into<String>) -> Self {
        Self::InvalidRequest {
            detail: detail.into(),
        }
    }

    pub fn window_invalidated(detail: impl Into<String>) -> Self {
        Self::WindowInvalidated {
            detail: detail.into(),
        }
    }

    pub fn anchor_unavailable(detail: impl Into<String>) -> Self {
        Self::AnchorUnavailable {
            detail: detail.into(),
        }
    }

    /// JSON-RPC 错误码。
    pub fn code(&self) -> i32 {
        match self {
            Self::NotNegotiated => -32601,
            Self::InvalidRequest { .. } | Self::SessionNotFound => -32602,
            Self::SessionClosing | Self::MutationInProgress => -32010,
            Self::CrossSession { .. } => -32011,
            Self::WindowInvalidated { .. } => -32012,
            Self::ReadTicketInvalid { .. } | Self::ReadTicketExpired => -32014,
            Self::SnapshotMismatch { .. } => -32015,
            Self::AnchorUnavailable { .. } => -32016,
            Self::ReadFailed { .. } => -32603,
        }
    }

    /// `error.data.reason` 的稳定取值。
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NotNegotiated => "history_window_not_negotiated",
            Self::InvalidRequest { .. } => "invalid_history_request",
            Self::SessionNotFound => "session_not_found",
            Self::SessionClosing => "session_closing",
            Self::MutationInProgress => "history_mutation_in_progress",
            Self::CrossSession { .. } => "cross_session_cursor",
            Self::WindowInvalidated { .. } => "history_window_invalidated",
            Self::ReadTicketInvalid { .. } => "history_read_ticket_invalid",
            Self::ReadTicketExpired => "history_read_ticket_expired",
            Self::SnapshotMismatch { .. } => "history_snapshot_mismatch",
            Self::AnchorUnavailable { .. } => "history_anchor_unavailable",
            Self::ReadFailed { .. } => "history_read_failed",
        }
    }
}

impl From<SessionResourceError> for HistoryWindowError {
    fn from(error: SessionResourceError) -> Self {
        match error.kind() {
            SessionResourceErrorKind::HistoryWindow(window) => window.clone(),
            SessionResourceErrorKind::NotFound => Self::SessionNotFound,
            SessionResourceErrorKind::Cursor(cursor_error) => match cursor_error {
                crate::history_page::HistoryCursorError::Malformed { detail } => {
                    Self::invalid_request(detail.clone())
                }
                crate::history_page::HistoryCursorError::Expired { .. } => {
                    Self::window_invalidated("history cursor has expired")
                }
            },
            SessionResourceErrorKind::InvalidInput { detail } => Self::invalid_request(detail),
            // 没有 index_not_ready 的 wire 取值：保留为读取失败，绝不伪装成
            // NotFound 或空页。V2 适配器在转换前直接映射到 IndexNotReady。
            SessionResourceErrorKind::IndexNotReady { detail } => Self::ReadFailed {
                detail: detail.clone(),
            },
            _ => Self::ReadFailed {
                detail: error.to_string(),
            },
        }
    }
}

#[cfg(test)]
#[path = "history_common_test.rs"]
mod tests;
