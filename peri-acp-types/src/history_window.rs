//! `peri.historyWindowV1` 协议类型：open/page/seek、双向游标、读取票据与错误码。
//!
//! 本模块只放**纯类型与纯校验**，不接触 `SessionState`、订阅或执行锁。Host 负责在锁内
//! 铸造 `sessionGeneration`/`snapshotToken`/`readTicket`，在锁外调用
//! [`crate::session_resources::SessionResources`] 的窗口读取，再于返回前复核
//! [`HistoryWindowMetadata`] 与 [`HistoryReadTicketBinding`]。
//!
//! ## 身份与失效
//!
//! - 条目的稳定身份是 [`HistoryEntryId`]（`{会话}:{message_id}`）；`position`/`rowid`
//!   只是同一 `snapshotToken` 内的排序键，不能作为 seek 锚点。
//! - [`HistoryWindowCursor`] 同时携带 generation/structure/snapshot/tail，任何一项与
//!   [`HistoryWindowMetadata`] 不一致都显式失败：generation/structure 变化是窗口失效
//!   （`-32012`），snapshot/tail/投影版本不一致是快照不匹配（`-32015`）。
//! - [`HistoryReadTicket`] 是不透明短期票据；本模块只校验**绑定字段**，签名与撤销由
//!   Host 的连接/会话事实负责。票据内容不进入 `Debug`、日志或错误文本。
//!
//! ## 与旧分页的关系
//!
//! 旧 `session/load` 的 `cursor` 仍使用 [`crate::history_page::HistoryCursor`] 与
//! [`crate::history_page::HistoryCursorEnvelope`]；V1 不复用旧游标的字段，也不把
//! before/after 混进旧 `nextCursor`。条目投影复用
//! [`crate::history_page::HistoryPageItem`]，不另写第三套投影。

use serde::{Deserialize, Serialize};

use crate::history_page::HistoryPageItem;
use crate::messages::MessageId;
use crate::session_resources::{SessionResourceError, SessionResourceErrorKind};
use crate::thread::ThreadId;

/// 窗口投影/预算口径版本；进入 snapshotToken 与 ticket 绑定。
pub const HISTORY_WINDOW_PROJECTION_VERSION: u16 = 1;
/// 服务端默认单页行数。
pub const HISTORY_WINDOW_DEFAULT_LIMIT: u16 = 50;
/// 单页行数上限（含 open 的 `limit` 与 page 的必填 `limit`）。
pub const HISTORY_WINDOW_MAX_LIMIT: u16 = 100;
/// 服务端默认页字节预算。
pub const HISTORY_WINDOW_DEFAULT_BYTE_BUDGET: u32 = 512 * 1024;
/// 客户端可请求的页字节预算上限。
pub const HISTORY_WINDOW_MAX_BYTE_BUDGET: u32 = 4 * 1024 * 1024;
/// 正文分块读取的默认块大小。
pub const HISTORY_WINDOW_DEFAULT_CHUNK_SIZE: u32 = 64 * 1024;
/// 正文分块读取的单次上限。
pub const HISTORY_WINDOW_MAX_CHUNK_SIZE: u32 = 256 * 1024;

// ─── 来源、方向与锚点 ──────────────────────────────────────────────────────────

/// 历史来源。V1 首版只实现 `Own`；`Inherited` 必须有冻结快照身份，不能拿父会话
/// 当前历史冒充子会话来源。
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

/// `open` 的锚点：缺省是 `latest`；`entry` 必须携带稳定条目身份与来源。
///
/// 反序列化是手写的：serde 的内部 tag 枚举无法同时启用 `deny_unknown_fields`，而 V1
/// 要求未知锚点字段显式失败，不能静默忽略。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HistoryWindowAnchor {
    #[default]
    Latest,
    Entry {
        #[serde(rename = "entryId")]
        entry_id: String,
        source: HistorySource,
    },
}

impl<'de> Deserialize<'de> for HistoryWindowAnchor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct EntryFields {
            entry_id: String,
            source: HistorySource,
        }

        let value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| D::Error::custom("anchor must be an object"))?;
        let kind = object
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| D::Error::custom("anchor.kind is required"))?;
        match kind {
            "latest" => {
                if object.len() != 1 {
                    return Err(D::Error::custom("latest anchor has unknown fields"));
                }
                Ok(Self::Latest)
            }
            "entry" => {
                let mut fields = object.clone();
                fields.remove("kind");
                let fields: EntryFields = serde_json::from_value(serde_json::Value::Object(fields))
                    .map_err(D::Error::custom)?;
                Ok(Self::Entry {
                    entry_id: fields.entry_id,
                    source: fields.source,
                })
            }
            _ => Err(D::Error::custom("unknown anchor.kind")),
        }
    }
}

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

    /// 解析并校验会话归属；别的会话的条目不能作为本窗口的锚点。
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

mod request;

pub use request::{
    HistoryWindowOpenPlan, HistoryWindowOpenRequest, HistoryWindowPageRequest,
    HistoryWindowRequest, HistoryWindowSeekAnchor, HistoryWindowSeekRequest,
};

// ─── 持久边界、checkpoint 与 metadata ─────────────────────────────────────────

/// 一次 open 看到的 durable tail：稳定条目身份 + 存储序键。
///
/// 客户端只拿到 [`Self::encode`] 的不透明文本；adapter 只在内部用它固定旧 snapshot
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

/// 窗口响应的轻量 metadata（不含正文）。
///
/// `session_id` 是响应顶层 `sessionId` 的事实源；其余字段由服务端在 open 时签发，
/// 客户端只回传不透明值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryWindowMetadata {
    pub session_id: ThreadId,
    pub session_generation: String,
    pub structure_version: u64,
    pub snapshot_token: String,
    /// `None` = 本次 snapshot 固定在「没有持久行」的边界上。
    pub committed_tail: Option<HistoryCommittedTail>,
    pub checkpoint_id: Option<String>,
}

impl HistoryWindowMetadata {
    /// 返回前复核 live 事实：generation 或 structure 变化即窗口失效。
    pub fn verify_live(
        &self,
        session_id: &ThreadId,
        session_generation: &str,
        structure_version: u64,
    ) -> Result<(), HistoryWindowError> {
        if self.session_id != *session_id {
            return Err(HistoryWindowError::CrossSession {
                expected: session_id.clone(),
                actual: self.session_id.clone(),
            });
        }
        if self.session_generation != session_generation
            || self.structure_version != structure_version
        {
            return Err(HistoryWindowError::WindowInvalidated {
                detail: "session generation or structure version changed".into(),
            });
        }
        Ok(())
    }

    pub fn verify_cursor(&self, cursor: &HistoryWindowCursor) -> Result<(), HistoryWindowError> {
        if cursor.thread_id != self.session_id {
            return Err(HistoryWindowError::CrossSession {
                expected: self.session_id.clone(),
                actual: cursor.thread_id.clone(),
            });
        }
        if cursor.session_generation != self.session_generation
            || cursor.structure_version != self.structure_version
        {
            return Err(HistoryWindowError::WindowInvalidated {
                detail: "cursor generation or structure version is stale".into(),
            });
        }
        if cursor.snapshot_token != self.snapshot_token
            || cursor.committed_tail != self.committed_tail
            || cursor.projection_version != HISTORY_WINDOW_PROJECTION_VERSION
        {
            return Err(HistoryWindowError::SnapshotMismatch {
                detail: "cursor does not belong to this snapshot".into(),
            });
        }
        Ok(())
    }
}

// ─── 双向游标 ──────────────────────────────────────────────────────────────────

/// 窗口游标（不透明）。`before` 只能向更早推进，`after` 只能向更晚推进。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryWindowCursor {
    pub thread_id: ThreadId,
    pub session_generation: String,
    pub structure_version: u64,
    pub snapshot_token: String,
    pub committed_tail: Option<HistoryCommittedTail>,
    pub source: HistorySource,
    /// before 游标 = 本页最旧一条；after 游标 = 本页最新一条。
    pub anchor: MessageId,
    pub direction: HistoryWindowDirection,
    pub projection_version: u16,
}

#[derive(Serialize, Deserialize)]
struct WireHistoryWindowCursor {
    v: u8,
    session: ThreadId,
    generation: String,
    structure: u64,
    snapshot: String,
    tail: Option<WireCommittedTail>,
    source: HistorySource,
    anchor: MessageId,
    direction: HistoryWindowDirection,
    projection: u16,
}

const WIRE_HISTORY_WINDOW_CURSOR_VERSION: u8 = 1;

impl HistoryWindowCursor {
    pub fn encode(&self) -> String {
        serde_json::to_string(&WireHistoryWindowCursor {
            v: WIRE_HISTORY_WINDOW_CURSOR_VERSION,
            session: self.thread_id.clone(),
            generation: self.session_generation.clone(),
            structure: self.structure_version,
            snapshot: self.snapshot_token.clone(),
            tail: self.committed_tail.as_ref().map(|tail| WireCommittedTail {
                v: WIRE_COMMITTED_TAIL_VERSION,
                session: tail.thread_id.clone(),
                entry: tail.entry_id,
                position: tail.position,
            }),
            source: self.source.clone(),
            anchor: self.anchor,
            direction: self.direction,
            projection: self.projection_version,
        })
        .expect("history window cursor is a JSON object of strings and numbers")
    }

    pub fn decode(raw: &str, expected: &ThreadId) -> Result<Self, HistoryWindowError> {
        let wire: WireHistoryWindowCursor =
            serde_json::from_str(raw).map_err(|_| HistoryWindowError::InvalidRequest {
                detail: "history cursor is malformed".into(),
            })?;
        if wire.v != WIRE_HISTORY_WINDOW_CURSOR_VERSION {
            return Err(HistoryWindowError::InvalidRequest {
                detail: "history cursor version is not supported".into(),
            });
        }
        if wire.session != *expected {
            return Err(HistoryWindowError::CrossSession {
                expected: expected.clone(),
                actual: wire.session,
            });
        }
        let tail = match wire.tail {
            Some(tail) => {
                if tail.v != WIRE_COMMITTED_TAIL_VERSION {
                    return Err(HistoryWindowError::InvalidRequest {
                        detail: "history cursor tail version is not supported".into(),
                    });
                }
                if tail.session != *expected {
                    return Err(HistoryWindowError::CrossSession {
                        expected: expected.clone(),
                        actual: tail.session,
                    });
                }
                Some(HistoryCommittedTail {
                    thread_id: tail.session,
                    entry_id: tail.entry,
                    position: tail.position,
                })
            }
            None => None,
        };
        Ok(Self {
            thread_id: wire.session,
            session_generation: wire.generation,
            structure_version: wire.structure,
            snapshot_token: wire.snapshot,
            committed_tail: tail,
            source: wire.source,
            anchor: wire.anchor,
            direction: wire.direction,
            projection_version: wire.projection,
        })
    }

    pub fn verify_against(
        &self,
        metadata: &HistoryWindowMetadata,
        expected_source: &HistorySource,
    ) -> Result<(), HistoryWindowError> {
        metadata.verify_cursor(self)?;
        if &self.source != expected_source {
            return Err(HistoryWindowError::SnapshotMismatch {
                detail: "cursor source does not match the window".into(),
            });
        }
        Ok(())
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

/// 票据绑定的服务端事实。Host 负责签名/撤销；这里提供可测试的纯校验。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryReadTicketBinding {
    /// 签发票据的**连接身份**。票据是 per-connection 的：另一个连接即使拿到
    /// 不透明字符串，也不能用同一张票据读取窗口，必须自己 `open`。
    pub connection_id: String,
    pub session_id: ThreadId,
    pub session_generation: String,
    pub structure_version: u64,
    pub snapshot_token: String,
    pub source: HistorySource,
    pub committed_tail: Option<HistoryCommittedTail>,
    pub projection_version: u16,
    pub issued_at_unix_ms: i64,
    pub expires_at_unix_ms: i64,
    /// 每次签发唯一的随机值；不得写入日志。
    pub nonce: String,
}

impl HistoryReadTicketBinding {
    /// 校验票据仍绑定当前 live metadata，且未过期。
    pub fn validate_at(
        &self,
        now_unix_ms: i64,
        metadata: &HistoryWindowMetadata,
        expected_source: &HistorySource,
    ) -> Result<(), HistoryWindowError> {
        if self.session_id != metadata.session_id {
            return Err(HistoryWindowError::CrossSession {
                expected: metadata.session_id.clone(),
                actual: self.session_id.clone(),
            });
        }
        if self.expires_at_unix_ms <= now_unix_ms
            || self.expires_at_unix_ms <= self.issued_at_unix_ms
        {
            return Err(HistoryWindowError::ReadTicketExpired);
        }
        if self.nonce.is_empty() || self.projection_version != HISTORY_WINDOW_PROJECTION_VERSION {
            return Err(HistoryWindowError::ReadTicketInvalid {
                detail: "ticket binding is incomplete".into(),
            });
        }
        if self.session_generation != metadata.session_generation
            || self.structure_version != metadata.structure_version
        {
            return Err(HistoryWindowError::WindowInvalidated {
                detail: "ticket generation or structure version is stale".into(),
            });
        }
        if self.snapshot_token != metadata.snapshot_token
            || self.committed_tail != metadata.committed_tail
            || &self.source != expected_source
        {
            return Err(HistoryWindowError::SnapshotMismatch {
                detail: "ticket does not belong to this snapshot".into(),
            });
        }
        Ok(())
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

/// 一次已确认 Full commit 的轻量元信息；只能在持久化确认后构造。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryCommitMetadata {
    pub checkpoint_id: String,
    pub summary_entry_id: Option<String>,
    pub committed_tail: String,
    pub structure_version: u64,
}

impl HistoryCommitMetadata {
    pub fn new(
        checkpoint_id: impl Into<String>,
        summary_entry_id: Option<String>,
        committed_tail: impl Into<String>,
        structure_version: u64,
    ) -> Result<Self, HistoryWindowError> {
        let checkpoint_id = checkpoint_id.into();
        let committed_tail = committed_tail.into();
        if checkpoint_id.is_empty() || committed_tail.is_empty() {
            return Err(HistoryWindowError::ReadFailed {
                detail: "history commit metadata is incomplete".into(),
            });
        }
        Ok(Self {
            checkpoint_id,
            summary_entry_id,
            committed_tail,
            structure_version,
        })
    }
}

// ─── 响应与失效窗口 ────────────────────────────────────────────────────────────

/// rewind 失效状态。正常响应只返回 `Active`；发现窗口已失效时必须返回错误，不得用
/// `ReopenRequired` 掩盖旧正文。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryRewindStatus {
    Active,
    ReopenRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryRewindInvalidation {
    pub status: HistoryRewindStatus,
    pub reopen_required: bool,
}

impl HistoryRewindInvalidation {
    pub fn active() -> Self {
        Self {
            status: HistoryRewindStatus::Active,
            reopen_required: false,
        }
    }

    pub fn is_active(&self) -> bool {
        self.status == HistoryRewindStatus::Active && !self.reopen_required
    }
}

/// open/page/seek 的响应（同形）。`items` 复用旧分页的条目类型与投影。
///
/// **V1 的唯一 wire 形状**：超预算条目不进入普通 item 的 `subItems`，而是在对应
/// item 上内联 `body` descriptor（`entryId`/`position`/`oversize` + `body.totalBytes`
/// /`body.chunkSize`/`body.readTicket`/`body.snapshotToken`）。不存在顶层
/// `bodyDescriptors` 数组，避免两套形状。正文从未被完整解码。
#[derive(Clone, Debug)]
pub struct HistoryWindowPage {
    pub metadata: HistoryWindowMetadata,
    pub read_ticket: HistoryReadTicket,
    pub before_cursor: Option<HistoryWindowCursor>,
    pub after_cursor: Option<HistoryWindowCursor>,
    pub items: Vec<HistoryPageItem>,
    /// 后端未实现 inherited 分段读取时为 true（本地 SQLite 为 false）；客户端必须把该
    /// 前缀视为未交付内容，而不是“没有 inherited 历史”。
    pub inherited_unsupported: bool,
    /// 本窗口可见的冻结 inherited 快照；own 窗口也会报告。
    pub inherited: Option<HistoryInheritedSnapshot>,
    pub rewind_invalidation: HistoryRewindInvalidation,
}

// ─── 错误码 ────────────────────────────────────────────────────────────────────

/// V1 窗口错误：每个变体对应一个稳定的 JSON-RPC code + `data.reason`。
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

    /// JSON-RPC 错误码（映射表见计划 §2.6）。
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
                crate::history_page::HistoryCursorError::CrossSession { expected, cursor } => {
                    Self::CrossSession {
                        expected: expected.clone(),
                        actual: cursor.clone(),
                    }
                }
                crate::history_page::HistoryCursorError::Expired { .. } => {
                    Self::window_invalidated("history cursor has expired")
                }
            },
            SessionResourceErrorKind::InvalidInput { detail } => Self::invalid_request(detail),
            _ => Self::ReadFailed {
                detail: error.to_string(),
            },
        }
    }
}

#[cfg(test)]
#[path = "history_window_test.rs"]
mod tests;
