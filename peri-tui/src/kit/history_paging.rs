//! TUI 历史分页：浏览状态、页响应解码、区间合并与触发（REQ-C01、验收 A11/A20/A23/A24）。
//!
//! ACP 侧的契约（见 `peri-acp/src/host/history_page.rs` 模块文档）：
//!
//! - 入口仍是 `session/load`：带 `cursor` + `limit` = 历史页，响应形状
//!   `{sessionId, snapshotToken, nextCursor, items[{entryId, position, oversize,
//!   subItems[{position, update}]}]}`；初始 load 的响应同样带 `nextCursor` /
//!   `snapshotToken`（仅协商过 `peri.historyPagination` 的连接才有）。
//! - 只在**同一个 `snapshotToken`** 内可以比较 `position`：它是存储序键（不透明整数），
//!   客户端不得解释，只用于排序与区间合并。继承区条目没有 `position`，先于全部自有条目。
//! - `entryId` 是页与首屏共用的唯一去重身份（`{会话}:{条目}`）。
//!
//! 本模块只做三件事，全部与实时状态无关（REQ-C01-6）：
//!
//! 1. **浏览状态**：`snapshotToken` / `nextCursor` / 在途请求。初始 load 响应与页响应
//!    是仅有的两个写入来源；触发侧只在「相同 cursor + 相同 limit 已在途」时合并请求。
//! 2. **区间合并**：按 `entryId` 去重、按 `position` 插入到正确区间（[`merge_page`]）。
//!    整页都早于当前最早条目时表现为插到自有条目区最前（继承前缀仍在更前）。
//! 3. **触发判定**：滚动接近向前边界时请求一页（[`request_older_page_if_due`]）。翻页
//!    loading 与执行状态分离——它不碰 `ACP_STATE.is_loading`，也不建立/结束 turn。
//!
//! 明确不做（REQ-C03）：不建前端持久化缓存、不做全历史虚拟化、不解析 `position` 的语义。

use std::collections::BTreeMap;

use serde_json::Value;

use crate::kit::atoms;
use crate::kit::history_window::{
    AnchorRelocation, GapId, GapSeek, GapSeekDirection, HistoryWindow, PersistentEntryKey,
    RegionKey,
};
use crate::kit::tui_render_unit::TuiRenderUnit;

pub mod window;
#[allow(unused_imports)] // S5.4 接入 open/page/seek 前由测试覆盖
pub(crate) use window::*;

/// 单页条数（服务端 `1..=100`，默认 50；见 REQ-H03）。
pub(crate) const PAGE_LIMIT: usize = 50;

/// 触发阈值：视口顶端距**向前边界**不超过这么多视觉行时请求更早一页。
///
/// 「向前边界」对 TUI 而言就是已载入窗口的顶端：稀疏首屏里的历史空缺
/// （`[继承 I][摘要 S]` 中缺失的旧 own 历史）也在这条边界之前，因此同一个触发点
/// 即可覆盖 REQ-C01-1 的两种边界。
pub(crate) const FETCH_MARGIN_LINES: usize = 2;

/// 同一 cursor 的最小请求间隔——负载保护；失败后同样要等过它才允许重试。
const REQUEST_BACKOFF_MS: u64 = 250;

/// 空页自动续读上限：一页全是不可展示条目时服务端返回空页 + 前进的游标
/// （契约允许），继续往前读但不无限循环。
pub(crate) const MAX_AUTO_CONTINUE_PAGES: usize = 4;

// ---------------------------------------------------------------------------
// 响应解码
// ---------------------------------------------------------------------------

/// 一条历史页条目：一个持久 payload 的完整展示封装。
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryPageItem {
    pub entry_id: String,
    /// 存储序键（不透明整数）——只在同一 `snapshotToken` 内用于排序与区间合并。
    pub position: u64,
    /// 单条内容超过页字节目标（服务端标记）。客户端不因它改变处置方式，保留字段
    /// 以便排查与测试断言。
    pub oversize: bool,
    pub sub_items: Vec<HistorySubItem>,
}

/// 一个投影子项：`update` 就是回放路径同一份 `session/update` 形状。
#[derive(Debug, Clone, PartialEq)]
pub struct HistorySubItem {
    /// 子项在本条 payload 投影序列中的下标——与条目 `position` 不是同一把尺子。
    pub position: u64,
    pub update: Value,
    /// 该子项是结构化 reminder（`systemReminder` 字段）而不是 `session/update`。
    /// 两者在 wire 上是不同的字段名，解码后仍然必须能区分（受众判据与展示形式都不同）。
    pub system_reminder: bool,
}

/// 一次历史页请求的定位信息（不含响应 `nextCursor`）。
///
/// 跨页工具结果的正文可能超过内存预算；补读必须回到**结果来源页的请求
/// cursor**，否则响应 `nextCursor` 已经指向更早一页，补读会错过结果。
/// 它随 `HistoryPageLoaded` 事件传递，桥接侧不再从进程级队列猜来源。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryPageOrigin {
    pub session_id: String,
    pub snapshot_token: String,
    pub cursor: String,
    pub limit: usize,
}

/// 一次历史页响应的客户端视图。
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryPageResponse {
    pub session_id: String,
    /// 本次浏览快照身份；与当前浏览不一致的迟到页必须丢弃（rewind / 切会话）。
    pub snapshot_token: String,
    /// 更早一页的游标；`None` = 没有更早的可展示内容。
    pub next_cursor: Option<String>,
    pub items: Vec<HistoryPageItem>,
}

/// 解码历史页响应。形状不符（缺 `items` 数组 / 缺身份字段）时返回 `None`——调用方
/// 丢弃整页，不猜测、不降级为「没有更早历史」。
pub(crate) fn decode_page_response(value: &Value) -> Option<HistoryPageResponse> {
    let session_id = value.get("sessionId")?.as_str()?.to_string();
    let snapshot_token = value.get("snapshotToken")?.as_str()?.to_string();
    let next_cursor = value
        .get("nextCursor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let items = value.get("items")?.as_array()?;
    let items = items.iter().filter_map(decode_item).collect();
    Some(HistoryPageResponse {
        session_id,
        snapshot_token,
        next_cursor,
        items,
    })
}

fn decode_item(value: &Value) -> Option<HistoryPageItem> {
    let entry_id = value.get("entryId")?.as_str()?.to_string();
    let position = value.get("position")?.as_u64()?;
    let sub_items = value
        .get("subItems")?
        .as_array()?
        .iter()
        .filter_map(|sub| {
            let position = sub.get("position")?.as_u64()?;
            let (update, system_reminder) = match sub.get("update") {
                Some(update) => (update.clone(), false),
                // 结构化 reminder 与 `update` 同层互斥（见 `peri-acp` 的 `sub_item`）。
                None => (sub.get("systemReminder")?.clone(), true),
            };
            Some(HistorySubItem {
                position,
                update,
                system_reminder,
            })
        })
        .collect();
    Some(HistoryPageItem {
        entry_id,
        position,
        oversize: value
            .get("oversize")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        sub_items,
    })
}

/// 从初始 load 响应里取本次浏览身份（`snapshotToken`, `nextCursor`）。
///
/// 未协商分页能力的连接两个字段都不存在——返回 `(None, None)`，触发侧因此永不请求
/// 历史页（REQ §5.3：未协商的连接不该收到分页私有语义）。
pub(crate) fn decode_session_cursor(value: &Value) -> (Option<String>, Option<String>) {
    let snapshot_token = value
        .get("snapshotToken")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let next_cursor = value
        .get("nextCursor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    (snapshot_token, next_cursor)
}

// ---------------------------------------------------------------------------
// S5.6 gap 恢复
// ---------------------------------------------------------------------------

/// 锚点落在缺口时的重读目标；消费端据此发 page（单边缺口）或 seek（中间缺口）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WindowGapRequest {
    Page {
        gap_id: GapId,
        cursor: WindowPageCursor,
    },
    Seek {
        gap_id: GapId,
        anchor: PersistentEntryKey,
    },
}

impl WindowGapRequest {
    pub(crate) fn gap_id(&self) -> GapId {
        match self {
            Self::Page { gap_id, .. } | Self::Seek { gap_id, .. } => *gap_id,
        }
    }

    /// 该候选是否响应指定滚动方向；seek 同时覆盖两侧。
    pub(crate) fn matches_direction(&self, direction: Option<GapSeekDirection>) -> bool {
        match self {
            Self::Seek { .. } => true,
            Self::Page { cursor, .. } => {
                let candidate = match cursor {
                    WindowPageCursor::Before(_) => GapSeekDirection::Before,
                    WindowPageCursor::After(_) => GapSeekDirection::After,
                };
                direction.is_none_or(|wanted| wanted == candidate)
            }
        }
    }
}

pub(crate) fn window_gap_recovery(window: &HistoryWindow) -> Option<WindowGapRequest> {
    let AnchorRelocation::Gap { gap_id, seek } = window.relocate_anchor() else {
        return None;
    };
    match seek? {
        GapSeek::Page {
            direction, cursor, ..
        } => Some(WindowGapRequest::Page {
            gap_id,
            cursor: match direction {
                GapSeekDirection::Before => WindowPageCursor::Before(cursor),
                GapSeekDirection::After => WindowPageCursor::After(cursor),
            },
        }),
        GapSeek::Seek { anchor, .. } => Some(WindowGapRequest::Seek { gap_id, anchor }),
    }
}

/// 桥接在 admission/release 后发布全部缺口的重读目标；滚动按方向消费。
///
/// 锚点恰好落入某个缺口时优先发布精确目标（中间缺口用 seek）；其余缺口按
/// **内侧邻居游标**各给一个方向候选：`before_cursor` 用 page(Before) 从缺口上端
/// 向下补，`after_cursor` 用 page(After) 从缺口下端向上补。
pub(crate) fn publish_window_gap_request(window: &HistoryWindow) {
    let mut requests = Vec::new();
    if let Some(request) = window_gap_recovery(window) {
        requests.push(request);
    }
    let mut gaps: Vec<_> = window.gaps().iter().collect();
    gaps.sort_by_key(|(gap_id, _)| **gap_id);
    for (gap_id, gap) in gaps {
        if requests
            .iter()
            .any(|request: &WindowGapRequest| request.gap_id() == *gap_id)
        {
            // 锚点缺口已有确定性更好的目标，不再发布按游标的候选。
            continue;
        }
        if let Some(cursor) = &gap.before_cursor {
            requests.push(WindowGapRequest::Page {
                gap_id: *gap_id,
                cursor: WindowPageCursor::Before(cursor.clone()),
            });
        }
        if let Some(cursor) = &gap.after_cursor {
            requests.push(WindowGapRequest::Page {
                gap_id: *gap_id,
                cursor: WindowPageCursor::After(cursor.clone()),
            });
        }
    }
    crate::kit::atoms::WINDOW_GAP_REQUEST.set(requests);
}

// ---------------------------------------------------------------------------
// 区间合并
// ---------------------------------------------------------------------------

/// 一个待并入窗口的条目：身份、存储序键与已经装配好的展示单元。
#[derive(Debug, Clone)]
pub(crate) struct PageEntry {
    pub entry_id: String,
    pub position: u64,
    pub units: Vec<TuiRenderUnit>,
}

/// 已载入条目的区间索引：存储序键 → 它在 `committed` 中的连续槽位。
///
/// 只记录**自有条目**（有 `position` 的条目）；继承区条目没有键、也不参与插入点计算
/// ——它们先于全部自有条目，插入点因此天然落在它们之后。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct HistorySpans {
    spans: BTreeMap<u64, HistorySpan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HistorySpan {
    entry_id: String,
    slot: usize,
    len: usize,
}

impl HistorySpans {
    /// 空索引（`static` 初始化用；`BTreeMap::new` 不分配）。
    pub(crate) const fn new() -> Self {
        Self {
            spans: BTreeMap::new(),
        }
    }

    /// 该条目是否已在窗口里（去重判据：`entryId` 是页与首屏共用的唯一身份）。
    pub(crate) fn contains(&self, entry_id: &str) -> bool {
        self.spans.values().any(|span| span.entry_id == entry_id)
    }

    /// 插入点：第一个 `position` 更大的条目的槽位；没有更大的条目时排在最后一个
    /// 已载入条目之后（页只向更早方向推进，这条兜底几乎不会走到）。
    fn insert_slot_for(&self, position: u64) -> usize {
        if let Some((_, span)) = self
            .spans
            .range((
                std::ops::Bound::Excluded(position),
                std::ops::Bound::Unbounded,
            ))
            .next()
        {
            return span.slot;
        }
        self.spans
            .values()
            .next_back()
            .map_or(0, |span| span.slot + span.len)
    }

    /// 登记新并入的条目，并把插入点之后的已有条目整体后移 `len` 个槽位。
    fn record(&mut self, position: u64, entry_id: String, slot: usize, len: usize) {
        if self.contains(&entry_id) {
            return;
        }
        for span in self.spans.values_mut() {
            if span.slot >= slot {
                span.slot += len;
            }
        }
        self.spans.insert(
            position,
            HistorySpan {
                entry_id,
                slot,
                len,
            },
        );
    }

    /// 已登记区间的槽位区间（升序）——按来源限定的只读遍历用（例如未配对工具
    /// 终点只允许配对到历史来源的卡片，不碰实时 turn 落进 `committed` 的卡片）。
    pub(crate) fn slots(&self) -> Vec<(usize, usize)> {
        let mut slots: Vec<(usize, usize)> = self
            .spans
            .values()
            .map(|span| (span.slot, span.len))
            .collect();
        slots.sort_unstable();
        slots
    }

    pub(crate) fn clear(&mut self) {
        self.spans.clear();
    }

    /// 从窗口驻留段派生 legacy 区间索引（S5 适配层）。
    ///
    /// 只登记自有来源且带 `position` 的条目；槽位取自 `ResidentIndex` 的前缀和。
    /// 旧 `HistorySpans` 的写入路径保持不变，开关关闭时不会调用本函数。
    pub(crate) fn from_resident_window(window: &HistoryWindow) -> Self {
        let mut spans = Self::new();
        for region in window.layout() {
            let RegionKey::Resident(segment_key) = *region else {
                continue;
            };
            let Some(segment) = window.segment(segment_key) else {
                continue;
            };
            for entry in &segment.entries {
                if !entry.key.source.is_own() {
                    continue;
                }
                let Some(position) = entry.position else {
                    continue;
                };
                if spans.contains(&entry.key.entry_id) {
                    continue;
                }
                let Some(locator) = window.index().locate_entry(&entry.key) else {
                    continue;
                };
                spans.spans.insert(
                    position,
                    HistorySpan {
                        entry_id: entry.key.entry_id.clone(),
                        slot: locator.slot_start,
                        len: entry.unit_count,
                    },
                );
            }
        }
        spans
    }
}

/// 合并结果（供发布与测试断言）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MergeOutcome {
    /// 新并入窗口的展示单元数。
    pub inserted_units: usize,
    /// 新并入的条目数。
    pub inserted_entries: usize,
    /// 因身份重复被丢弃的条目数（重试同一页时应当等于页条目数）。
    pub duplicates: usize,
    /// 新并入条目在窗口里的槽位包络 `[first, end)`。
    ///
    /// 口径是**包络**：页整体更早的契约下它恰好等于「插入行集合」；若将来允许
    /// 页与既有条目交错插入，包络内部会含既有行。
    ///
    /// [Why 保留] 这是**桥接层内部**的并合事实（`state.committed` 槽位坐标）：
    /// 用于并入日志与 merge 层用例断言。它**不得**参与渲染侧计算——渲染槽位是发布后
    /// `VIEW_MODELS.items` 的下标，发布前的折叠 pass 会改变槽位数（相邻成功工具卡
    /// 压成折叠组），两套坐标不可互换；跨层用槽位号正是 `.recon/S7d-verification.md`
    /// §5.1 判定 FAIL 的路径。阅读锚点已改为渲染侧身份重定位
    /// （`message_area::viewport_anchor`）。
    pub inserted_slot_range: Option<(usize, usize)>,
}

/// 把一页并入窗口：按 `entryId` 去重、按 `position` 合并到正确区间。
///
/// - 条目按 `position` 升序逐个插入（页内本来正序，乱序页也被规范化）；
/// - 整页都早于当前最早条目时，插入点在首个自有条目的槽位——继承前缀仍在它之前，
///   因此结果是 `[继承 I][更早的 H][原有的 S]`，而不是把 H 插到 I 前面
///   （REQ-H01-6、A20）；
/// - 插入点落在窗口中间时同样是按 `position` 落到正确区间，不移动其它条目的相对顺序。
pub(crate) fn merge_page(
    window: &mut im::Vector<TuiRenderUnit>,
    spans: &mut HistorySpans,
    entries: &[PageEntry],
) -> MergeOutcome {
    let mut outcome = MergeOutcome::default();
    let mut ordered: Vec<&PageEntry> = entries.iter().collect();
    ordered.sort_by_key(|entry| entry.position);
    for entry in ordered {
        if entry.units.is_empty() {
            continue;
        }
        if spans.contains(&entry.entry_id) {
            outcome.duplicates += 1;
            continue;
        }
        let slot = spans.insert_slot_for(entry.position);
        for (offset, unit) in entry.units.iter().enumerate() {
            window.insert(slot + offset, unit.clone());
        }
        spans.record(
            entry.position,
            entry.entry_id.clone(),
            slot,
            entry.units.len(),
        );
        outcome.inserted_units += entry.units.len();
        outcome.inserted_entries += 1;
        let end = slot + entry.units.len();
        outcome.inserted_slot_range = Some(match outcome.inserted_slot_range {
            Some((first, current_end)) => (first.min(slot), current_end.max(end)),
            None => (slot, end),
        });
    }
    outcome
}

// ---------------------------------------------------------------------------
// 浏览状态
// ---------------------------------------------------------------------------

/// 在途页请求的身份（请求发出时冻结；响应回来时用它判定是否仍是当前浏览）。
///
/// 身份只由**页参数**构成：触发侧认领请求与消费侧结算各自构造它，两侧必须相等——
/// 「同一页」的判据就是同会话、同快照、同 cursor、同 limit（与
/// [`crate::kit::history_page_consumer::is_duplicate_of`] 同一份规则）。时间戳不进
/// 身份：它只在两侧各取一次 `Instant::now()`，永远不可能相等。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InFlightPage {
    pub session_id: String,
    pub snapshot_token: String,
    pub cursor: String,
    pub limit: usize,
}

/// 历史浏览状态（单一事实源）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HistoryPagingState {
    /// 当前浏览所属会话。
    pub session_id: String,
    /// 本次浏览快照身份；`None` = 未开始（未协商 / 尚未 load 成功 / 已失效）。
    pub snapshot_token: Option<String>,
    /// 更早一页的游标；`None` = 没有更早内容或浏览已失效。
    pub next_cursor: Option<String>,
    pub in_flight: Option<InFlightPage>,
    /// 最近一次请求的落地时刻——成功率与失败重试共用同一份退避规则。
    pub last_request_at: Option<std::time::Instant>,
    /// 最近一次失败的展示文本（保持当前画面并提供重试，不新增展示面）。
    pub last_error: Option<String>,
}

impl HistoryPagingState {
    /// 请求是否仍是当前浏览的一部分（会话 + 快照身份都必须一致）。
    pub(crate) fn accepts(&self, session_id: &str, snapshot_token: &str) -> bool {
        self.session_id == session_id && self.snapshot_token.as_deref() == Some(snapshot_token)
    }
}

/// 已载入条目的区间索引（桥接任务独占写入；会话边界清空）。
static HISTORY_SPANS: parking_lot::Mutex<HistorySpans> =
    parking_lot::Mutex::new(HistorySpans::new());

/// 回放正在铺进窗口的**自有**条目（`_meta.periEntryId` + `periPosition`）。
///
/// 首屏的区间索引只能从回放标记里长出来：契约里继承区条目**没有** `position`
/// （`periPosition` 缺席），因此「首个有位置的条目」就是自有区起点——整页更早的历史
/// 必须插在它之前、继承前缀之后（REQ-H01-6、A20）。
struct OpenOwnEntry {
    entry_id: String,
    position: u64,
    /// 首个展示单元落进 `committed` 的槽位（并入后才知道，因此延后登记）。
    slot: Option<usize>,
    /// 本条目的展示单元个数（回放按顺序 push_back，因此区间连续）。
    len: usize,
}

static OPEN_OWN_ENTRY: parking_lot::Mutex<Option<OpenOwnEntry>> = parking_lot::Mutex::new(None);

/// 打开（或延续）一个回放条目区间。`entry_id`/`position` 来自 wire 的回放标记。
pub(crate) fn mark_own_entry(entry_id: &str, position: u64) {
    let mut guard = OPEN_OWN_ENTRY.lock();
    if let Some(open) = guard.as_ref()
        && open.entry_id == entry_id
        && open.position == position
    {
        return;
    }
    if let Some(open) = guard.take() {
        flush_own_entry(open);
    }
    *guard = Some(OpenOwnEntry {
        entry_id: entry_id.to_string(),
        position,
        slot: None,
        len: 0,
    });
}

/// 回放推入一个展示单元（`slot` = 该单元在 `committed` 里的槽位）。
pub(crate) fn note_own_entry_unit(slot: usize) {
    let mut guard = OPEN_OWN_ENTRY.lock();
    if let Some(open) = guard.as_mut() {
        open.slot.get_or_insert(slot);
        open.len += 1;
    }
}

/// 回放结束：关闭当前条目区间并登记到索引。
pub(crate) fn close_own_entry() {
    let mut guard = OPEN_OWN_ENTRY.lock();
    if let Some(open) = guard.take() {
        flush_own_entry(open);
    }
}

fn flush_own_entry(open: OpenOwnEntry) {
    let Some(slot) = open.slot else {
        return;
    };
    if open.len == 0 {
        return;
    }
    with_spans(|spans| spans.record(open.position, open.entry_id, slot, open.len));
}

/// 会话边界：清空浏览状态与区间索引
/// （会话切换 / `/clear` / rewind 后的重新 load，见 `session_boundary`）。
pub(crate) fn reset_browsing_state() {
    invalidate_browsing_state();
    HISTORY_SPANS.lock().clear();
    *OPEN_OWN_ENTRY.lock() = None;
    // 并入信号（`HISTORY_PREPEND_EPOCH`）不在这里作废：它只承载「并合发生过」这一件事，
    // 与哪个窗口无关，由渲染侧按「观察到的发布换代」配对消费（`viewport_anchor`）。
}

fn invalidate_browsing_state() {
    let handle = atoms::HISTORY_PAGING.state();
    let mut state = handle.write();
    *state = HistoryPagingState::default();
}

/// 初始 load 成功后记录本次浏览身份（`nextCursor` / `snapshotToken` 来自响应）。
pub(crate) fn note_initial_cursor(
    session_id: &str,
    snapshot_token: Option<String>,
    next_cursor: Option<String>,
) {
    let handle = atoms::HISTORY_PAGING.state();
    let mut state = handle.write();
    state.session_id = session_id.to_string();
    state.snapshot_token = snapshot_token;
    state.next_cursor = next_cursor;
    state.in_flight = None;
    state.last_request_at = None;
    state.last_error = None;
}

/// 在区间索引上执行一段操作（合并时调用；把全局索引的借用收在一处）。
pub(crate) fn with_spans<R>(f: impl FnOnce(&mut HistorySpans) -> R) -> R {
    let mut spans = HISTORY_SPANS.lock();
    f(&mut spans)
}

/// 窗口路径合并后，把全局区间索引同步为驻留索引的派生视图。
pub(crate) fn sync_spans_from_window(window: &HistoryWindow) {
    let derived = HistorySpans::from_resident_window(window);
    with_spans(|spans| *spans = derived);
}

/// 视口是否已经贴到**向前边界**（已载入窗口的最早一行）——REQ-C01-1 的触发判据。
///
/// - 浏览态（`follow_bottom == false`）：视口顶端落在最早一行附近
///   （[`FETCH_MARGIN_LINES`] 行内）；
/// - 内容不足一屏：没有「尾部」可言，视口顶端就是边界——稀疏首屏的旧历史空缺
///   （`[继承 I][摘要 S]`）正是这种形状；
/// - 跟随态且内容超出一屏：看的是尾部，不在向前边界上。
///
/// 判据只有这一份实现：渲染 effect 路径（内容变化 / 稀疏首屏自动续读）与滚动事件
/// 路径（空闲会话里用户滚到顶端）都经它判定。
pub(crate) fn viewport_at_forward_boundary(
    follow_bottom: bool,
    scroll_y: usize,
    total_visual_rows: usize,
    vis_height: usize,
) -> bool {
    if vis_height == 0 || total_visual_rows == 0 {
        return false;
    }
    let content_fits = total_visual_rows <= vis_height;
    let browsing = !follow_bottom;
    (browsing || content_fits) && (scroll_y <= FETCH_MARGIN_LINES || content_fits)
}

/// 向后边界：接近已载入窗口最底部（含内容不足一屏/跟随态）。
pub(crate) fn viewport_at_backward_boundary(
    follow_bottom: bool,
    scroll_y: usize,
    total_visual_rows: usize,
    vis_height: usize,
) -> bool {
    if vis_height == 0 || total_visual_rows == 0 {
        return false;
    }
    let content_fits = total_visual_rows <= vis_height;
    if content_fits || follow_bottom {
        return true;
    }
    scroll_y
        .saturating_add(vis_height)
        .saturating_add(FETCH_MARGIN_LINES)
        >= total_visual_rows
}

/// 触发口：视口贴向前边界就请求更早一页，返回本次是否真的派发。
///
/// 只有「贴边」这一个前置判断在调用方之前；去重（相同 cursor + limit 在途即合并）、
/// 退避窗口、未协商 / 无下一页都在 [`request_older_page_if_due`] 的读—判—写同锁认领内
/// 结算，因此本函数可以被滚动事件高频调用而不违反 REQ-C01-2。
///
/// 两条调用路径都必须存在：
///
/// 1. **渲染 effect**（`message_area/scroll/auto_follow.rs`）：内容变化后用最新窗口
///    事实判定，覆盖「稀疏首屏自动续读」——那时没有滚动事件可依赖；
/// 2. **滚动事件**（`message_area/scroll/event.rs`）：**空闲会话**（无 turn、无内容
///    变化）里 effect deps 不变、`use_effect` 不会重跑，用户滚到已载入窗口顶端只能
///    由滚动本身触发，否则翻页与失败后的重试都没有触发源。该路径按
///    `scroll::project_viewport_after_pending` 预判节流窗口内 pending 落地后的视口
///    事实——滚轮的 offset 落地晚于事件本身，只看当帧事实会漏掉那一次滚动。
pub(crate) fn request_older_page_at_boundary(
    follow_bottom: bool,
    scroll_y: usize,
    total_visual_rows: usize,
    vis_height: usize,
) -> bool {
    if !viewport_at_forward_boundary(follow_bottom, scroll_y, total_visual_rows, vis_height) {
        return false;
    }
    request_older_page_if_due(true)
}

/// 触发：向上滚动接近向前边界时请求更早一页。
///
/// 返回 `true` 表示本次真的派发了请求。相同 cursor + 相同 limit 已在途时**合并**
/// 为一个请求（不重复派发，REQ-C01-2）；未协商 / 没有下一页 / 退避窗口内都是 no-op。
pub(crate) fn request_older_page_if_due(near_forward_boundary: bool) -> bool {
    // V2 激活时页请求只走 V2 reducer（before 方向），旧页/旧窗口两套队列都不参与。
    if crate::kit::history_v2::consumer::v2_path_active() {
        return near_forward_boundary
            && crate::kit::history_v2::consumer::request_older_page_if_due();
    }
    if crate::kit::history_page_consumer::window_path_enabled() {
        return near_forward_boundary
            && crate::kit::history_page_consumer::request_window_older_page_if_due();
    }
    if !near_forward_boundary {
        return false;
    }
    let Some(tx) = atoms::HISTORY_PAGES_TX.get() else {
        return false;
    };
    let handle = atoms::HISTORY_PAGING.state();
    // 读—判—写在同一把锁内完成：并发触发只有一个能认领这次请求。
    let mut state = handle.write();
    let (Some(cursor), Some(snapshot_token)) =
        (state.next_cursor.clone(), state.snapshot_token.clone())
    else {
        return false;
    };
    if state.session_id.is_empty() || state.in_flight.is_some() {
        return false;
    }
    if state
        .last_request_at
        .is_some_and(|last| last.elapsed() < std::time::Duration::from_millis(REQUEST_BACKOFF_MS))
    {
        return false;
    }
    let request = InFlightPage {
        session_id: state.session_id.clone(),
        snapshot_token,
        cursor,
        limit: PAGE_LIMIT,
    };
    state.in_flight = Some(request.clone());
    drop(state);
    let sent = tx
        .send(crate::kit::history_page_consumer::HistoryPageRequest {
            session_id: request.session_id.clone(),
            snapshot_token: request.snapshot_token.clone(),
            cursor: request.cursor.clone(),
            limit: request.limit,
        })
        .is_ok();
    if !sent {
        // 通道已关闭：撤销认领，保持状态一致。
        let handle = atoms::HISTORY_PAGING.state();
        let mut state = handle.write();
        state.in_flight = None;
    }
    sent
}

/// 消费侧请求完成后的状态结算：成功则把游标推进到下一页，失败只记录原因并保留
/// 当前 cursor（可重试）。
///
/// 返回 `None` 表示这次响应已经不属于当前浏览（会话 / 快照身份 / 在途请求都不匹配），
/// 调用方必须丢弃它（REQ-C01-5、A11）。
pub(crate) fn settle_request(
    request: &InFlightPage,
    outcome: Result<Option<String>, String>,
) -> Option<Option<String>> {
    let handle = atoms::HISTORY_PAGING.state();
    let mut state = handle.write();
    if !state.accepts(&request.session_id, &request.snapshot_token) {
        return None;
    }
    if state.in_flight.as_ref() != Some(request) {
        return None;
    }
    state.in_flight = None;
    state.last_request_at = Some(std::time::Instant::now());
    match outcome {
        Ok(next_cursor) => {
            state.last_error = None;
            state.next_cursor = next_cursor.clone();
            Some(next_cursor)
        }
        Err(error) => {
            state.last_error = Some(error);
            None
        }
    }
}

/// 游标 / 浏览身份失效（-32012 范围失效、-32011 跨会话）：停止对本次浏览继续翻页，
/// 待下一次初始 load 重新建立浏览身份。
pub(crate) fn invalidate_browsing(session_id: &str, reason: &str) {
    let handle = atoms::HISTORY_PAGING.state();
    let mut state = handle.write();
    if state.session_id != session_id {
        return;
    }
    state.next_cursor = None;
    state.snapshot_token = None;
    state.in_flight = None;
    state.last_error = Some(reason.to_string());
}

// `pub(crate)`：`pages_channel`/`drain_pages_channel` 是**进程级唯一**的请求通道
// 接收端（见该文件顶部说明），滚动事件路径的触发用例（`message_area::scroll_test`）
// 必须复用同一份接收端——另建一份 `OnceLock` 会因重复 set 而永远收不到请求。
#[cfg(test)]
#[path = "history_paging_test.rs"]
pub(crate) mod tests;
