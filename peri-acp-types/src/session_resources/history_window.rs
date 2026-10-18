//! `SessionResources` 的窗口读取端口类型：边界、双向 keyset 计划与有界结果。
//!
//! 这些类型只描述**存储行为**：不携带 `sessionGeneration`、ticket 或结构版本——那些是
//! Host 的运行期事实。adapter 只负责在给定 tail 身份/位置内做有界读取；任何全量读取后
//! 在 Rust 里过滤的实现都不满足本端口。
//!
//! 公开路径由父模块 `pub use` 保持为 `peri_acp_types::session_resources::*`。

use crate::history_page::HistoryPageItem;
use crate::history_window::{
    HistoryCheckpoint, HistoryCommittedTail, HistoryInheritedWindow, HistorySource,
    HistoryWindowDirection,
};
use crate::messages::MessageId;

/// 一次有界读取的窗口边界事实（不读正文）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionHistoryWindowBounds {
    /// 自有历史最旧一条的稳定 id；空会话为 `None`。
    pub own_oldest: Option<MessageId>,
    /// 当前 durable tail；空会话为 `None`。
    pub own_latest: Option<HistoryCommittedTail>,
    /// 最近一次可由现有 canonical 行重建的 Full checkpoint。
    pub checkpoint: Option<HistoryCheckpoint>,
    /// 冻结祖先前缀的可用性。S4 不支持 inherited 分段读取。
    pub inherited: HistoryInheritedWindow,
}

impl SessionHistoryWindowBounds {
    pub fn committed_tail(&self) -> Option<&HistoryCommittedTail> {
        self.own_latest.as_ref()
    }
}

/// 本次读取的入口，用来决定锚点消失时的失败分类与是否包含锚点自身。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryWindowReadIntent {
    /// 首次 open：以当前 durable tail 为界，锚点可缺省（latest）。
    Open,
    /// 旧 snapshot 的翻页：锚点缺失是窗口失效。
    Page,
    /// 按稳定 entryId 重新定位：锚点缺失是 anchor unavailable。
    Seek,
}

/// 本次读取必须固定到的持久边界。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryWindowTailExpectation {
    /// open：读取当前 durable tail，并把它写进返回结果。
    Current,
    /// page/seek：必须仍是 open 时的 tail。
    ///
    /// `Fixed(None)` 表示 open 时没有持久行；即使之后追加了新行，本次读取也不得纳入。
    Fixed(Option<HistoryCommittedTail>),
}

/// 窗口读取计划：双向 keyset 区间 + 固定 tail + 资源上限。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryWindowPagePlan {
    pub intent: HistoryWindowReadIntent,
    pub source: HistorySource,
    /// `None` = open 的 latest；page/seek 必须显式给出稳定条目 id。
    pub anchor: Option<MessageId>,
    pub direction: HistoryWindowDirection,
    /// seek/open(entry) 要把锚点自身纳入返回；page 游标已经消费过锚点，不重复返回。
    pub include_anchor: bool,
    pub before_limit: usize,
    pub after_limit: usize,
    pub byte_budget: usize,
    pub tail: HistoryWindowTailExpectation,
}

impl HistoryWindowPagePlan {
    /// 首次 open：没有快照尾界，adapter 读取当前 durable tail。
    pub fn open(
        source: HistorySource,
        anchor: Option<MessageId>,
        direction: HistoryWindowDirection,
        before_limit: usize,
        after_limit: usize,
        byte_budget: usize,
    ) -> Self {
        Self {
            intent: HistoryWindowReadIntent::Open,
            source,
            anchor,
            direction,
            include_anchor: anchor.is_some(),
            before_limit,
            after_limit,
            byte_budget,
            tail: HistoryWindowTailExpectation::Current,
        }
    }

    /// 旧 snapshot 翻页：tail 必须与 open 时一致。
    pub fn page(
        source: HistorySource,
        anchor: MessageId,
        direction: HistoryWindowDirection,
        limit: usize,
        byte_budget: usize,
        tail: Option<HistoryCommittedTail>,
    ) -> Self {
        Self {
            intent: HistoryWindowReadIntent::Page,
            source,
            anchor: Some(anchor),
            direction,
            include_anchor: false,
            before_limit: if direction.reads_before() { limit } else { 0 },
            after_limit: if direction.reads_after() { limit } else { 0 },
            byte_budget,
            tail: HistoryWindowTailExpectation::Fixed(tail),
        }
    }

    /// seek：围绕锚点返回两侧，并包含锚点自身。
    pub fn seek(
        source: HistorySource,
        anchor: MessageId,
        before_limit: usize,
        after_limit: usize,
        byte_budget: usize,
        tail: HistoryCommittedTail,
    ) -> Self {
        Self {
            intent: HistoryWindowReadIntent::Seek,
            source,
            anchor: Some(anchor),
            direction: HistoryWindowDirection::Around,
            include_anchor: true,
            before_limit,
            after_limit,
            byte_budget,
            tail: HistoryWindowTailExpectation::Fixed(Some(tail)),
        }
    }
}

/// 一条窗口结果：正常读取的条目，或超预算条目的 descriptor 候选（未取正文）。
// `Loaded` 是窗口页的常态载荷；对 `Oversized` 做 boxing 会给所有读取路径加一层间接，
// 改动面大于收益，故保持按值变体。
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum HistoryWindowEntry {
    Loaded(HistoryPageItem),
    Oversized {
        entry_id: MessageId,
        position: u64,
        total_bytes: u64,
    },
}

impl HistoryWindowEntry {
    pub fn entry_id(&self) -> MessageId {
        match self {
            Self::Loaded(item) => item.id,
            Self::Oversized { entry_id, .. } => *entry_id,
        }
    }

    pub fn position(&self) -> u64 {
        match self {
            Self::Loaded(item) => item.position,
            Self::Oversized { position, .. } => *position,
        }
    }
}

/// 继承区的一条结果：没有自有 `position`，身份由子会话 + message id + source 决定。
// 与 `HistoryWindowEntry` 同口径：`Loaded` 为常态载荷，boxing 的间接成本不划算。
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum InheritedHistoryEntry {
    Loaded(HistoryPageItem),
    Oversized {
        entry_id: MessageId,
        total_bytes: u64,
    },
}

impl InheritedHistoryEntry {
    pub fn entry_id(&self) -> MessageId {
        match self {
            Self::Loaded(item) => item.id,
            Self::Oversized { entry_id, .. } => *entry_id,
        }
    }
}

/// 一次窗口读取的有界结果（页内历史正序）。
#[derive(Clone, Debug)]
pub struct HistoryWindowRows {
    /// 本次读取实际固定的 durable tail；`None` = 空历史边界。
    pub committed_tail: Option<HistoryCommittedTail>,
    pub entries: Vec<HistoryWindowEntry>,
    /// 继承区条目（`source = inherited`）；own 读取为空。
    pub inherited_entries: Vec<InheritedHistoryEntry>,
    /// 继承区读取固定到的冻结快照 id；own 读取为 `None`。
    pub inherited_snapshot_id: Option<String>,
    /// 向更早方向还有内容时，下一次 before 读取的锚点（本页最旧一行）。
    pub before_cursor_anchor: Option<MessageId>,
    /// 向更晚方向还有内容时，下一次 after 读取的锚点（本页最新一行）。
    pub after_cursor_anchor: Option<MessageId>,
}

impl HistoryWindowRows {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.inherited_entries.is_empty()
    }
}
