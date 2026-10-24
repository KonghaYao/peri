//! 历史存储读取契约 — keyset 行读取、窗口页装配规则与标准回放段。
//!
//! 本模块只描述**存储读取**与展示装配：条目可来自归档区（被 compact 标成 `excluded`
//! 的条目仍要能读出来），过滤只发生在展示出口。ACP 已不再暴露旧私有分页请求；这些类型
//! 服务于 `SessionResources` 的存储端口与 History V2 的窗口读取计划。
//!
//! ## 读取分两阶段，装配规则只有一份
//!
//! 一页在**一个快照内**分两阶段读出（[`HistoryPagePlan`]）：
//!
//! 1. 阶段一只读「id + 存储序键 + 正文长度」，最多 `limit + 1` 行，**不取正文**；
//!    [`HistoryPagePlan::plan`] 按长度决定本页推进哪些行。
//! 2. 阶段二只取**这些行**的正文（[`HistoryPagePlan::body_ids`]），交给
//!    [`HistoryPagePlan::materialize_omitting_oversize`] 装配。
//!
//! 多读的那一条候选行只回答该方向还有没有内容，它的正文**永远不进内存**。
//! 页的边界、字节预算与 oversize 判定也只有一份实现（[`HistoryPagePlan::plan`] 与
//! [`HistoryPagePlan::materialize_omitting_oversize`]）：存储 adapter 只负责把有界 SQL 的
//! 行交进来。
//!
//! ## 条目身份与位置
//!
//! - 条目身份是 `{会话}:{条目 id}`；同一个持久 payload 只有这一个身份。
//! - [`HistoryPageItem::position`] 是存储序（`rowid`）原样给出的不透明整数，只在同一
//!   快照内可比；rewind / 精确删除后新插入的行会复用 rowid。
//! - 继承区（冻结的祖先前缀）条目在本线程没有行，因此没有 `position`。
//!
//! ## canonical 完整回放（向前方向，REQ §5.3）
//!
//! 标准 ACP 客户端走**完整回放**：继承区 + 全部
//! 自有行（含 Full 归档原文）。核心不重新持有整份历史，改用**向前**方向流式投递：
//! [`HistorySegment`]（一段载荷）+ [`HistoryForwardCursor`]（本段推进到哪、本次回放的固定
//! 上界是谁）+ [`HistorySegmentPlan`]（两个存储 adapter 共用的唯一一份段规则）。
//!
//! 与窗口页读取的三点差别是刻意的：
//!
//! - 方向相反：窗口按锚点向两侧读，回放从最旧一行向「更晚」推进（`rowid` 升序）。
//! - **不做可展示性过滤**：段里 System 条目与 model-only reminder 照常出现——完整回放不
//!   允许静默丢内容，过滤属于出口投影。
//! - 游标承载**物理排序键**（`rowid` 的十进制文本）而不是条目身份：回放区间由首段那一刻的
//!   最大 `rowid`（`tail`）固定，之后**追加**的行只要 `rowid > tail` 就不进入本次遍历；
//!   起点行消失仍失败关闭（不得回退成从头开始，也不得用空段冒充结束）。游标的线上形式
//!   有独立的结构版本。
//!   已知边界（契约不保证、也不加校验）：`tail` 那一行被删（rewind 的尾部切除）之后，SQLite
//!   会把它的 `rowid` 复用给新插入的行，这类新行 `rowid <= tail` 因而会被本次遍历读到，而被
//!   删行的内容静默消失（实测见 `.recon/S9-boundedness.md` 第 3.4 节第 6 点与第 6 节 R1）。

#[path = "history_page_visibility.rs"]
mod visibility;
pub use visibility::{history_body_displayable_to_client, HistoryEntryRole};

use serde::{Deserialize, Serialize};

use crate::messages::MessageId;
use crate::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use crate::store::{deserialize_persisted_payload, PersistedPayload};
use crate::thread::ThreadId;

/// 回放游标不能用于本次读取。
///
/// 两种失败对应两种客户端处置：形式/版本错误是坏请求，失效（起点行或回放上界已不存在）
/// 是「重新开始回放」。两者都必须**显式失败**：降级成首段会把一段续读变成从头重放。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryCursorError {
    /// 形式无法解释：不是本实现产出的游标（损坏、缺字段、版本不支持）。
    Malformed { detail: String },
    /// 起点行已不存在（rewind / 删除）。
    Expired { thread_id: ThreadId },
}

impl std::fmt::Display for HistoryCursorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed { detail } => {
                write!(formatter, "history cursor is malformed: {detail}")
            }
            Self::Expired { thread_id } => write!(
                formatter,
                "history cursor anchor no longer exists in session {thread_id}"
            ),
        }
    }
}

impl std::error::Error for HistoryCursorError {}

impl From<HistoryCursorError> for SessionResourceError {
    fn from(error: HistoryCursorError) -> Self {
        Self::new(SessionResourceErrorKind::Cursor(error))
    }
}

/// canonical 完整回放（向前方向）的游标：本次回放的**固定区间**。
///
/// 本游标承载的是**物理排序键**——`rowid` 的十进制文本，只在本次回放内有效。
/// 区间由首段读取那一刻的最大 `rowid`（[`Self::tail`]）固定：回放期间**追加**的行只要
/// `rowid > tail` 就不进入本次遍历；起点行消失时读取失败关闭，不回退成从头开始，也不用空段
/// 冒充结束。
///
/// 已知边界：`tail` 那一行被删后 `rowid` 会被新插入的行复用，这类新行因此落在区间内被读到
/// （见模块文档的 rowid 复用边界；契约不保证行集合同源，也不加校验）。
///
/// 字段是服务端语义，客户端不解释它；游标的线上形式有独立的结构版本。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryForwardCursor {
    /// 本段最后一条**已推进**的自有行：更晚方向继续从它之后读（`rowid > after`）。
    pub after: StorageOrderKey,
    /// 本次完整回放的固定上界：首段读取那一刻该线程自有行的最大 `rowid`。
    ///
    /// 判定用 `rowid <= tail`，**不要求** `tail` 那一行还在：上界是一个数值，不是一条要查
    /// 回来的行（它被删后其 `rowid` 会被新行复用，那些新行因此会进入本次遍历，见模块文档的
    /// 已知边界）。它随每一段透传（首段确定，此后不变）。
    pub tail: StorageOrderKey,
}

/// 存储序键（`rowid`）的**十进制文本形式**。
///
/// 用文本而不是 [`MessageId`]：`rowid` 不是条目身份（删掉尾部行之后新插入的行会复用这些
/// rowid），这里要的正是「那一刻的物理排序键」。整数文本与数值之间来回转换是精确的，两个
/// 存储 adapter 与调用方各自转换一次不会把一次正常续读变成失效。
pub type StorageOrderKey = String;

/// 线上形式：版本化 JSON。客户端不解释，只在下一段请求里原样回传。
#[derive(Serialize, Deserialize)]
struct WireHistoryForwardCursor {
    v: u8,
    after: StorageOrderKey,
    tail: StorageOrderKey,
}

const WIRE_HISTORY_FORWARD_CURSOR_VERSION: u8 = 1;

impl HistoryForwardCursor {
    pub fn new(after: impl Into<StorageOrderKey>, tail: impl Into<StorageOrderKey>) -> Self {
        Self {
            after: after.into(),
            tail: tail.into(),
        }
    }

    /// 线上不透明形式。
    pub fn encode(&self) -> String {
        serde_json::to_string(&WireHistoryForwardCursor {
            v: WIRE_HISTORY_FORWARD_CURSOR_VERSION,
            after: self.after.clone(),
            tail: self.tail.clone(),
        })
        .expect("history forward cursor is a JSON object of strings")
    }

    /// 解析线上形式并校验形式（两个存储序键都是十进制非负整数，且 `after < tail`）。
    ///
    /// 形式错误**不降级**：调用方据此拒绝本次读取，不得把坏游标当成首段（那会把一段续读
    /// 变成从头重放），也不得把 `after >= tail` 的游标读成「已经读到上界」——本实现铸造的
    /// 游标只在确实还有更晚内容时给出，且给出时 `after < tail` 必然成立。
    pub fn decode(raw: &str) -> Result<Self, HistoryCursorError> {
        let wire: WireHistoryForwardCursor =
            serde_json::from_str(raw).map_err(|error| HistoryCursorError::Malformed {
                detail: error.to_string(),
            })?;
        if wire.v != WIRE_HISTORY_FORWARD_CURSOR_VERSION {
            return Err(HistoryCursorError::Malformed {
                detail: format!("unsupported history forward cursor version {}", wire.v),
            });
        }
        let cursor = Self {
            after: wire.after,
            tail: wire.tail,
        };
        cursor.rowid_bounds()?;
        Ok(cursor)
    }

    /// 两个存储序键的数值形式 `(after, tail)`，并校验 `0 <= after < tail`。
    ///
    /// 存储后端在绑定参数之前**必须**走这里：文本形式不能直接与 `rowid` 比较（SQLite 按
    /// 存储类排序，`INTEGER < TEXT` 恒成立，拿文本比较等于把区间判成空）。形式不符是坏游标
    /// （[`HistoryCursorError::Malformed`]）：不猜、不取绝对值、不把 `after` 当成 0。
    pub fn rowid_bounds(&self) -> Result<(i64, i64), HistoryCursorError> {
        let after = rowid_of(&self.after)?;
        let tail = rowid_of(&self.tail)?;
        if after >= tail {
            return Err(HistoryCursorError::Malformed {
                detail: "history forward cursor does not advance within its tail".to_owned(),
            });
        }
        Ok((after, tail))
    }
}

/// 存储序键的文本 → 数值：非十进制非负整数即形式不符（负号、空格、空串都不接受）。
fn rowid_of(key: &str) -> Result<i64, HistoryCursorError> {
    key.parse::<i64>()
        .ok()
        .filter(|value| *value >= 0)
        .ok_or_else(|| HistoryCursorError::Malformed {
            detail: "history forward cursor key is not a storage order key".to_owned(),
        })
}

/// 页内一条条目：**原始**持久化载荷 + 计量。
///
/// 展示投影（转成客户端能渲染的更新）是 ACP 层的事，存储层不预先裁剪正文。
#[derive(Clone, Debug)]
pub struct HistoryPageItem {
    pub id: MessageId,
    pub payload: PersistedPayload,
    /// 持久化信封的编码字节数 —— 页面预算与页面计量的同一口径。
    pub encoded_bytes: usize,
    /// 存储序键（`rowid`）原样给出的**不透明整数**：同一次浏览（同一个 `snapshotToken`）
    /// 内可比较，跨快照不可比（删尾部行后新插入的行会复用 rowid）。客户端不得解释它，
    /// 只用于排序与区间合并（合并契约见模块文档）。
    pub position: u64,
}

/// 一页历史（页内正序：更早 → 更晚）。
///
/// History V2 的窗口读取用它承载同一方向的行；是否还有更多由窗口邻接事实单独回答。
#[derive(Clone, Debug)]
pub struct HistoryPage {
    pub items: Vec<HistoryPageItem>,
}

/// 一次有界读取读出的一行候选（尚未解码），按存储序（`rowid`）**降序**：最靠近读取锚点的在前。
#[derive(Clone, Debug)]
pub struct HistoryPageRow {
    pub message_id: MessageId,
    /// 持久化信封原文（`serialize_persisted_payload` 的产物）。
    pub content: String,
}

impl HistoryPageRow {
    /// 从持久化行的 id 文本（uuid）与信封原文构造候选行；id 不可解释即损坏。
    pub fn new(message_id: &str, content: String) -> SessionResourceResult<Self> {
        Ok(Self {
            message_id: message_id_of(message_id)?,
            content,
        })
    }
}

/// 阶段一的一行候选：身份 + 存储序键 + **编码长度**，**不含正文**。
///
/// 长度是持久化信封原文的字节数（与 [`HistoryPageRow::content`] 的 `len()` 同口径、
/// 与 [`HistoryPageItem::encoded_bytes`] 同口径）：页预算按它判定，因此正文不必进内存。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryPageCandidate {
    pub message_id: MessageId,
    /// 存储序键（`rowid`），见 [`HistoryPageItem::position`]。
    pub position: u64,
    /// 持久化信封的编码字节数。
    pub encoded_bytes: usize,
    /// `messages.role` 的粗分类；窗口读取用它判定 oversize 可见性。
    pub role: HistoryEntryRole,
}

impl HistoryPageCandidate {
    /// 从持久化行的 id 文本、存储序键与字节长度构造候选行。
    ///
    /// id 不是 uuid 即损坏（与 [`HistoryPageRow::new`] 同规）；负的行号或长度不可能出现
    /// （`rowid` 与长度都非负），出现即损坏，不取绝对值兜底。
    pub fn new(message_id: &str, position: i64, encoded_bytes: i64) -> SessionResourceResult<Self> {
        Self::new_with_role(
            message_id,
            position,
            encoded_bytes,
            HistoryEntryRole::Unknown,
        )
    }

    /// 带存储 role 的候选行；窗口读取用它避免为可见性判定解码正文。
    pub fn new_with_role(
        message_id: &str,
        position: i64,
        encoded_bytes: i64,
        role: HistoryEntryRole,
    ) -> SessionResourceResult<Self> {
        Ok(Self {
            message_id: message_id_of(message_id)?,
            position: u64::try_from(position)
                .map_err(|_| corrupt("history row position is negative"))?,
            encoded_bytes: usize::try_from(encoded_bytes)
                .map_err(|_| corrupt("history entry length is negative"))?,
            role,
        })
    }
}

/// 一页的读取计划（阶段一的产物）：**先按长度定页，再只取这些行的正文**。
///
/// 这是两个存储 adapter 共用的唯一一份页规则：候选行按存储序给入，本类型决定
/// 本页推进哪些行。
///
/// 规则：
///
/// 1. 一次只推进 `limit` 行；调用方多给一条候选行（最多 `limit + 1` 行），用来判断该
///    方向是否还有内容。判据是**未过滤的**行数而不是展示条数，所以一整段不可展示条目
///    不会假造终点。
/// 2. 字节预算按**被推进的每一行**计（含不可展示行）：不可展示行的正文同样要读出来才
///    知道它能不能展示，预算要约束的正是「读了多少」。因此 [`Self::plan`] 只看长度就能
///    定页，不必先解码。
/// 3. 预算之外**只多留一条**：整行原文就超预算的那条留给下一页，本页已推进的行不受
///    影响；第一条就超预算时独占一页（作为 descriptor 候选返回），仍然推进。
/// 4. 页内顺序与展示性在阶段二（[`Self::materialize_omitting_oversize`]）决定：不可展示
///    行不进入 `items`，但位置照常推进。
///
/// 候选行的给入顺序：同一条预算规则，两个方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlanOrder {
    /// 存储序降序（最靠近最新一行在前）：继承区窗口的「更早」方向。
    NewestFirst,
    /// 存储序升序（最靠近锚点一行在前）：窗口的「更晚」方向。
    OldestFirst,
}

#[derive(Clone, Debug)]
pub struct HistoryPagePlan {
    byte_budget: usize,
    /// 本页推进的行，也是阶段二**唯一**允许取正文的集合。
    entries: Vec<HistoryPageCandidate>,
    order: PlanOrder,
}

impl HistoryPagePlan {
    /// 按长度决定本页推进哪些行（阶段一，不解码任何正文）。
    ///
    /// `candidates` 必须按存储序降序给入，最多 `limit + 1` 行（多出的那一条只用于
    /// 判断「更早方向还有没有内容」）。
    pub fn plan(candidates: Vec<HistoryPageCandidate>, limit: usize, byte_budget: usize) -> Self {
        Self::plan_ordered(candidates, limit, byte_budget, PlanOrder::NewestFirst)
    }

    /// 窗口「更晚」方向的页计划（同一份长度预算规则，方向相反）。
    ///
    /// `candidates` 必须按存储序**升序**给入（离锚点最近的一行在前），最多 `limit + 1`
    /// 行；多出的那一条只用于判断「更晚方向还有没有内容」。页内顺序仍是历史正序。
    pub fn plan_after(
        candidates: Vec<HistoryPageCandidate>,
        limit: usize,
        byte_budget: usize,
    ) -> Self {
        Self::plan_ordered(candidates, limit, byte_budget, PlanOrder::OldestFirst)
    }

    fn plan_ordered(
        candidates: Vec<HistoryPageCandidate>,
        limit: usize,
        byte_budget: usize,
        order: PlanOrder,
    ) -> Self {
        let mut entries: Vec<HistoryPageCandidate> = Vec::new();
        let mut remaining = byte_budget;
        for candidate in &candidates {
            if entries.len() >= limit {
                break;
            }
            // 已经有条目在手时，整行原文就超预算的不再收下：本页到此为止，游标停在上一条
            // 已推进的行上，这条留给下一页。
            if !entries.is_empty() && candidate.encoded_bytes > remaining {
                break;
            }
            remaining = remaining.saturating_sub(candidate.encoded_bytes);
            entries.push(*candidate);
        }
        Self {
            byte_budget,
            entries,
            order,
        }
    }

    /// 本页推进的行（按候选给入顺序），含不可展示行。
    pub fn entries(&self) -> &[HistoryPageCandidate] {
        &self.entries
    }

    /// 本页使用的字节预算（窗口据此区分 descriptor 条目）。
    pub fn byte_budget(&self) -> usize {
        self.byte_budget
    }

    /// 阶段二**唯一**允许取正文的条目集合：就是本页那些行的 id。
    ///
    /// 多读的那一条候选行不在其中；它的正文永远不该被读出来。
    pub fn body_ids(&self) -> Vec<MessageId> {
        self.entries.iter().map(|entry| entry.message_id).collect()
    }

    /// 窗口装配：超预算的条目**不取正文**，原样作为 descriptor 候选返回。
    ///
    /// `rows` 必须与 [`Self::entries`] **同一集合、同一顺序**（阶段二只按 [`Self::body_ids`]
    /// 取行）。集合或顺序不符即损坏：把少读/多读的行静默装配成一页，等于把一次不完整读取
    /// 当成正常页返回。长度与位置取阶段一的事实（页面预算当初就是按它判定的）。
    ///
    /// 返回值第二项是被省略的条目（`encoded_bytes > byte_budget`），调用方据此签发
    /// [`crate::history_common::HistoryBodyDescriptor`]；不得先把完整正文取回再切片。
    pub fn materialize_omitting_oversize(
        self,
        rows: Vec<HistoryPageRow>,
    ) -> SessionResourceResult<(HistoryPage, Vec<HistoryPageCandidate>)> {
        let selected: Vec<(HistoryPageCandidate, bool)> = self
            .entries
            .iter()
            .map(|entry| (*entry, entry.encoded_bytes > self.byte_budget))
            .collect();
        let loaded: Vec<HistoryPageCandidate> = selected
            .iter()
            .filter(|(_, omitted)| !omitted)
            .map(|(entry, _)| *entry)
            .collect();
        if rows.len() != loaded.len() {
            return Err(corrupt(
                "history page bodies do not match the planned page size",
            ));
        }
        let mut items = Vec::new();
        for (row, planned) in rows.iter().zip(loaded.iter()) {
            if row.message_id != planned.message_id {
                return Err(corrupt(
                    "history page bodies are not the planned page entries",
                ));
            }
            let payload = decode_row(row)?;
            if payload.displayable_to_client() {
                items.push(HistoryPageItem {
                    id: planned.message_id,
                    payload,
                    encoded_bytes: planned.encoded_bytes,
                    position: planned.position,
                });
            }
        }
        // 继承区窗口按存储序降序进入（更靠近最新一行的在前），返回前反转成历史正序；
        // 窗口的「更晚」计划已经按升序给入，保持原序。
        if self.order == PlanOrder::NewestFirst {
            items.reverse();
        }
        let omitted = selected
            .into_iter()
            .filter(|(_, omitted)| *omitted)
            .map(|(entry, _)| entry)
            .collect();
        Ok((HistoryPage { items }, omitted))
    }
}

/// 一段 canonical 完整回放（标准 ACP 客户端路径）。
///
/// 段内载荷按 canonical 顺序给出：继承区在前（**只在首段**出现），其后是自有行按 `rowid`
/// 升序。**不过滤任何条目**：已排除的归档原文、System 条目、model-only reminder 都在其中——
/// 这是完整历史，可展示性与投影是调用方（ACP 出口）的事。
#[derive(Clone, Debug)]
pub struct HistorySegment {
    pub payloads: Vec<PersistedPayload>,
    /// `None` = 本次回放**没有更多自有行**了（已到固定上界，或本来就没有行）。
    ///
    /// 它不表示「本次读取失败」或「后端不支持」：那两种情形都必须显式失败，不能用一个空段
    /// 冒充回放结束。
    pub next_cursor: Option<HistoryForwardCursor>,
}

/// 一段的读取计划（阶段一的产物）：**先按长度定段，再只取这些行的正文**。
///
/// 这是两个存储 adapter 共用的唯一一份段规则（与 [`HistoryPagePlan`] 同构，方向相反）：
/// 候选行按存储序**升序**给入，本类型决定本段推进哪些行、游标停在谁之后、更晚方向还有没有
/// 内容。
///
/// 规则：
///
/// 1. 一次最多推进 `limit` 行；调用方多给一条候选行（最多 `limit + 1` 行），用来判断更晚
///    方向是否还有内容。判据是**未过滤的**行数，因此段里不会出现「读到的比推进的多」。
/// 2. 字节预算按**被推进的每一行**计（完整回放不做可展示性过滤，所以每一行都要读正文）。
///    预算之外只多留一条：整行原文就超预算的那条留给下一段，本段已推进的行不受影响；
///    第一条就超预算时独占一段（完整回放不允许丢内容），仍然推进并给出游标——否则大条目
///    之后的历史永远读不到。
/// 3. `limit` 必须为正：`limit = 0` 的段既不推进任何行也铸不出游标（`after` 不存在），而
///    调用方会把「没有游标」读成「回放结束」——静默截断，因此 [`Self::materialize`] 失败
///    关闭（`InvalidInput`），不返回一个看起来正常的空段。
#[derive(Clone, Debug)]
pub struct HistorySegmentPlan {
    /// 本段推进的行（升序存储序），也是阶段二**唯一**允许取正文的集合。
    entries: Vec<HistoryPageCandidate>,
    /// 更晚方向是否还有内容（`false` ⇒ `next_cursor = None`）。
    has_more: bool,
}

impl HistorySegmentPlan {
    /// 按长度决定本段推进哪些行（阶段一，不解码任何正文）。
    ///
    /// `candidates` 必须按存储序**升序**给入，最多 `limit + 1` 行（多出的那一条只用于
    /// 「更晚方向还有没有内容」）。
    pub fn plan(candidates: Vec<HistoryPageCandidate>, limit: usize, byte_budget: usize) -> Self {
        let mut entries: Vec<HistoryPageCandidate> = Vec::new();
        let mut remaining = byte_budget;
        for candidate in &candidates {
            if entries.len() >= limit {
                break;
            }
            // 已经有条目在手时，整行原文就超预算的不再收下：本段到此为止，游标停在上一条
            // 已推进的行上，这条留给下一段。
            if !entries.is_empty() && candidate.encoded_bytes > remaining {
                break;
            }
            remaining = remaining.saturating_sub(candidate.encoded_bytes);
            entries.push(*candidate);
        }
        // 只有「候选行全部推进完」且「没有多出来的那一条」才表示更晚方向确实没有内容。
        let exhausted = entries.len() == candidates.len() && candidates.len() <= limit;
        Self {
            entries,
            has_more: !exhausted,
        }
    }

    /// 阶段二**唯一**允许取正文的条目集合：就是本段那些行的 id。
    ///
    /// 多读的那一条候选行不在其中；它的正文永远不该被读出来。
    pub fn body_ids(&self) -> Vec<MessageId> {
        self.entries.iter().map(|entry| entry.message_id).collect()
    }

    /// 阶段二：把本段的正文与首段的继承区交给段（两个存储 adapter 共用的唯一装配实现）。
    ///
    /// - `inherited`：继承区 payload，**只在首段非空**（来自单行 `inherited_context` 快照）；
    ///   继承区在本切片不分页，因此它整份跟在首段最前面。
    /// - `tail`：本次回放的固定上界文本。首段由读取给出、后续段透传游标里的值；只有确实
    ///   还有更晚内容时才会用到它，那时它必须存在——缺失即读取不完整，不当成「没有更多」。
    /// - `rows`：阶段二按 [`Self::body_ids`] 取回的本段正文行，集合与顺序都必须一致。长度
    ///   或身份不符即损坏：把少读/多读的行静默装配成一段，等于把一次不完整读取当成正常回放
    ///   （完整回放尤其不能丢内容）。长度与位置取阶段一的事实（段预算当初就是按它判定的）。
    pub fn materialize(
        self,
        inherited: Vec<PersistedPayload>,
        tail: Option<&str>,
        rows: Vec<HistoryPageRow>,
    ) -> SessionResourceResult<HistorySegment> {
        if self.entries.is_empty() && self.has_more {
            return Err(invalid_input("history segment limit must be positive"));
        }
        if rows.len() != self.entries.len() {
            return Err(corrupt(
                "history segment bodies do not match the planned segment size",
            ));
        }
        let mut payloads = inherited;
        for (row, planned) in rows.iter().zip(self.entries.iter()) {
            if row.message_id != planned.message_id {
                return Err(corrupt(
                    "history segment bodies are not the planned segment entries",
                ));
            }
            payloads.push(decode_row(row)?);
        }
        let next_cursor = match (self.has_more, self.entries.last()) {
            (true, Some(last)) => Some(HistoryForwardCursor::new(
                last.position.to_string(),
                tail.ok_or_else(|| corrupt("history segment tail is missing"))?,
            )),
            _ => None,
        };
        Ok(HistorySegment {
            payloads,
            next_cursor,
        })
    }
}

/// 持久化行里的条目 id 文本 → [`MessageId`]：不是 uuid 即损坏。
///
/// id 的解析规则只有这一份（候选行、正文行、存储序键读取共用）：两边各解释一次就可能把
/// 同一条目的两个身份混起来。
pub fn message_id_of(message_id: &str) -> SessionResourceResult<MessageId> {
    Ok(uuid::Uuid::parse_str(message_id)
        .map_err(|_| corrupt("persisted payload message id is not a uuid"))?
        .into())
}

/// 行内 id 与载荷 id 必须一致：不一致是数据故障，不把两套身份混起来（与整段历史读取同规）。
fn decode_row(row: &HistoryPageRow) -> SessionResourceResult<PersistedPayload> {
    let payload = deserialize_persisted_payload(&row.content)
        .map_err(|_| corrupt("history entry is not decodable"))?;
    if payload.id() != row.message_id {
        return Err(corrupt("persisted payload message id mismatch"));
    }
    Ok(payload)
}

fn corrupt(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Corrupt {
        detail: detail.to_owned(),
    })
}

/// 调用方给错了参数（段长为零）：不是数据故障，也不能降级成一次正常回答。
fn invalid_input(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
        detail: detail.to_owned(),
    })
}

#[cfg(test)]
#[path = "history_page_test.rs"]
mod tests;
