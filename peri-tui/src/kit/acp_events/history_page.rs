//! 历史页 → 展示单元 → 并入窗口（REQ-C01、A20 合并、A23 投影隔离、A24 不碰执行状态）。
//!
//! ## 为什么不能复用实时 handler
//!
//! 页里的 `subItems[].update` 与回放路径同源，但**并入窗口不是一次回放**：回放由
//! `session/load` 触发、按顺序从头铺满 committed；翻页发生在任意时刻，可能与正在跑的
//! turn、TodoWrite 基线、审批或输入队列并存。因此本模块：
//!
//! - 只构造**展示单元**（[`TuiRenderUnit`]），构造过程不写 `committed`、不 flush
//!   `current_turn`、不动 `turn_generation` / `INPUT_BUFFER` / `STEERS` / 审批弹窗；
//! - 不复用 `handle_replay_tool_started` 的副作用（`record_todo_started` 会写入执行侧
//!   Todo 基线，翻页不得触碰）；
//! - 不复用 `handle_system_reminder`（它会 flush `current_turn`）。
//!
//! 单元构造仍然共用回放路径的**同一份**构造函数（`turn::committed_assistant_unit`、
//! `tool::replay_tool_started_unit`、`tool::completed_tool_card`），避免第二份展示规则。
//!
//! ## 身份与丢弃
//!
//! 两层独立判据，任何一层不匹配就整页丢弃（REQ-C01-5、A11）：
//!
//! 1. 消费者侧：RPC 返回后按「会话 + 快照」核对当前浏览，不匹配不入队；
//! 2. 桥接侧：本模块再核对响应自带的 `sessionId` / `snapshotToken` 与当前浏览状态，
//!    以及桥接自己的 active session（`acp_bridge` 还会按事件携带的 session 过滤一次）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;

use super::BridgeState;
use crate::acp_client::AcpTuiClient;
use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch, ToolResultRefetch};
use crate::kit::atoms;
use crate::kit::history_page_consumer::PAGE_FETCH_TIMEOUT;
use crate::kit::history_paging::{
    self, HistoryPageItem, HistoryPageOrigin, HistoryPageResponse, HistorySubItem, PageEntry,
};
use crate::kit::history_window::{
    HistorySource, PENDING_TOOL_ENDS_BYTES_MAX, PENDING_TOOL_ENDS_LIMIT, PersistentEntryKey,
};
use crate::kit::tui_render_unit::{FoldState, TuiRenderUnit, TuiSystemReminder, TuiUserBubble};

/// 窗口/旧页共用的投影输入：持久身份 + 投影子项（尚未物化展示单元）。
#[derive(Clone, Debug)]
pub(super) struct ProjectionItem {
    pub entry_id: String,
    pub source: HistorySource,
    pub position: Option<u64>,
    pub sub_items: Vec<HistorySubItem>,
}

/// 投影结果：保留来源身份，供窗口路径去重与段边界使用。
#[derive(Clone, Debug)]
pub(super) struct ProjectedEntry {
    pub entry_id: String,
    pub source: HistorySource,
    pub position: Option<u64>,
    pub units: Vec<TuiRenderUnit>,
}

/// 已加载窗口里还没有起点的工具结束事件：完整正文（预算内）或来源页定位
/// （超出预算，等用户展开再补读）。
///
/// 页边界会把 `tool_call` 与它的 `tool_call_update` 分到相邻两页，而**更晚**的一页
/// 先被读到——那时卡片还不存在。留着它，等起点页并进来时补齐；会话一换就作废
/// （结果不能跨会话错配到同 id 的卡片上）。
///
/// 条数/字节上限的权威值在 [`super::super::history_window::budget`]（与窗口预算
/// 同源），这里不再维护第二份常量。
pub(crate) const DEFERRED_TOOL_RESULT_TEXT: &str = "结果正文较大，展开后按需加载";
pub(crate) const UNAVAILABLE_TOOL_RESULT_TEXT: &str = "结果正文较大，无法按需加载";

/// 结果来源页的请求定位。补读必须重放**结果那一页的请求参数**，不能使用响应
/// `nextCursor`（它已指向更早一页）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolResultSource {
    pub session_id: String,
    pub snapshot_token: String,
    pub cursor: String,
    pub limit: usize,
    pub entry_id: String,
    pub entry_position: u64,
    pub sub_item_position: u64,
    pub tool_id: String,
}

#[derive(Clone, Debug)]
struct PendingToolEnd {
    is_error: bool,
    /// 结果事件所在的持久条目身份。释放事务必须用它与释放计划条目精确配对；
    /// 它来自投影输入本身，不由 `tool_id` 反推 message id。
    entry: PersistentEntryKey,
    /// `None` = 超出内存预算，保留来源定位等待按需补读。
    output: Option<Arc<str>>,
    source: Option<ToolResultSource>,
    /// 用户展开触发的补读结果必须跳过预算再次转引用（一次性正文）。
    force_complete: bool,
    placeholder_applied: bool,
    /// 当前在途补读的请求代次；`Some` 时拒绝重复补读。
    in_flight_request: Option<u64>,
}

impl PendingToolEnd {
    fn retained_bytes(&self) -> usize {
        self.output.as_ref().map_or(0, |output| output.len())
    }
}

#[derive(Default)]
struct PendingToolResults {
    entries: HashMap<String, PendingToolEnd>,
    order: VecDeque<String>,
    /// 预算内完整正文的字节数；引用条目不计入。
    bytes: usize,
}

impl PendingToolResults {
    fn insert(
        &mut self,
        tool_id: String,
        entry: PersistentEntryKey,
        output: &str,
        is_error: bool,
        source: Option<ToolResultSource>,
        force_complete: bool,
    ) {
        let previous = self.remove(&tool_id);
        let source = source.or_else(|| previous.as_ref().and_then(|entry| entry.source.clone()));
        let output = (force_complete || output.len() <= PENDING_TOOL_ENDS_BYTES_MAX)
            .then(|| Arc::from(output));
        let end = PendingToolEnd {
            is_error,
            entry,
            output,
            source,
            force_complete,
            placeholder_applied: false,
            in_flight_request: None,
        };
        self.entries.insert(tool_id.clone(), end);
        self.order.push_back(tool_id);
        self.rebalance();
    }

    /// 预算内保留正文，超预算的旧条目**降级为引用**而不是删除；条数超限才丢弃
    /// 最旧元数据。
    fn rebalance(&mut self) {
        while self.entries.len() > PENDING_TOOL_ENDS_LIMIT {
            // 条数上限优先丢弃已完整保留正文的旧条目；引用条目只占少量元数据，
            // 尽量保留，避免用户展开时失去补读定位。
            let candidate = self
                .order
                .iter()
                .find(|id| {
                    self.entries
                        .get(*id)
                        .is_some_and(|entry| entry.output.is_some())
                })
                .cloned()
                .or_else(|| self.order.front().cloned());
            let Some(oldest) = candidate else {
                break;
            };
            self.order.retain(|id| id != &oldest);
            self.entries.remove(&oldest);
        }
        let mut total: usize = self
            .entries
            .values()
            .filter(|entry| !entry.force_complete)
            .map(PendingToolEnd::retained_bytes)
            .sum();
        while total > PENDING_TOOL_ENDS_BYTES_MAX {
            let Some(oldest) = self
                .order
                .iter()
                .find(|id| {
                    self.entries
                        .get(*id)
                        .is_some_and(|entry| entry.output.is_some() && !entry.force_complete)
                })
                .cloned()
            else {
                break;
            };
            if let Some(entry) = self.entries.get_mut(&oldest)
                && let Some(output) = entry.output.take()
            {
                total = total.saturating_sub(output.len());
            }
        }
        self.bytes = total;
    }

    fn remove(&mut self, tool_id: &str) -> Option<PendingToolEnd> {
        let removed = self.entries.remove(tool_id)?;
        if !removed.force_complete {
            // `bytes` 只统计非 force_complete 正文；补读正文是一次性完整结果，
            // 不占预算，删除时也不能从预算里扣。
            self.bytes = self.bytes.saturating_sub(removed.retained_bytes());
        }
        self.order.retain(|id| id != tool_id);
        Some(removed)
    }

    /// 按释放计划里的持久条目身份精准移除暂存终点；返回移除条数。
    ///
    /// 只删除结果条目本身在释放集合中的条目。其他段的暂存终点原样保留，
    /// 因此释放一个段不会清掉别段等待配对的工具结果。
    fn remove_entries(&mut self, entries: &HashSet<PersistentEntryKey>) -> usize {
        if entries.is_empty() {
            return 0;
        }
        let matched: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, end)| entries.contains(&end.entry))
            .map(|(tool_id, _)| tool_id.clone())
            .collect();
        for tool_id in &matched {
            self.remove(tool_id);
        }
        matched.len()
    }

    /// 用户展开卡片：取出来源定位并标记在途；已完整/在途的条目不重复补读。
    fn deferred_source(&mut self, tool_id: &str, request_id: u64) -> Option<ToolResultSource> {
        let entry = self.entries.get_mut(tool_id)?;
        if entry.output.is_some() || entry.in_flight_request.is_some() {
            return None;
        }
        let source = entry.source.clone()?;
        entry.in_flight_request = Some(request_id);
        Some(source)
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

static PENDING_TOOL_ENDS: Mutex<Option<(String, PendingToolResults)>> = Mutex::new(None);

/// 大结果补读的请求代次。身份随 `HistoryPageLoaded.refetch` 传递，迟到的旧请求
/// 只能释放自己（不能清掉新请求的 `in_flight_request`）。
static NEXT_REFETCH_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_refetch_request_id() -> u64 {
    NEXT_REFETCH_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

/// 大结果补读的单次 RPC 预算——与普通翻页共用同一期限（服务端静默时必须终止）。
const TOOL_RESULT_REFETCH_TIMEOUT: Duration = PAGE_FETCH_TIMEOUT;

/// 并入一页（桥接任务内调用；调用方已保证事件串行）。
///
/// `origin` / `refetch` 与响应同事件到达：来源只可能属于本响应，`force_complete`
/// 也只作用于 `refetch.tool_id`。`refetch.error` 是补读失败：卡片显示失败终态，
/// 但暂存来源保留，释放在途后用户可再次展开重试。
pub(super) fn handle_history_page_loaded(
    state: &mut BridgeState,
    response: &HistoryPageResponse,
    origin: Option<&HistoryPageOrigin>,
    refetch: Option<&ToolResultRefetch>,
) {
    if !accepts_page(state, response) {
        if let Some(refetch) = refetch {
            release_in_flight(&refetch.tool_id, refetch.request_id);
        }
        tracing::debug!(
            session_id = %response.session_id,
            items = response.items.len(),
            "history_page: page dropped (not the current browsing identity)"
        );
        return;
    }
    if let Some(refetch) = refetch
        && let Some(message) = refetch.error.as_deref()
    {
        // 只有仍持有该请求的条目才接受失败结果：迟到的旧失败不得覆盖新请求。
        if release_in_flight(&refetch.tool_id, refetch.request_id) {
            apply_refetch_failure(state, &refetch.tool_id, message);
        }
        return;
    }
    let force_complete_tool = refetch.map(|refetch| refetch.tool_id.as_str());
    let entries = build_entries(state, &response.items, origin, force_complete_tool);
    // 并入前先关闭回放区间：生产路径**没有**回放结束通知（`SessionReplayDone` 是
    // 死路径，见 `turn::handle_session_replay_done` 的注释），首屏最后一个自有条目
    // 会一直停留在「打开」状态、不进索引。那时索引可能整段为空——整页更早的历史会
    // 被插到槽位 0，越过继承前缀（违反 REQ-H01-6、A20）。合并前收口即可拿到正确的
    // 插入点：所有未标记单元都排在首个已标记的自有条目之前。
    history_paging::close_own_entry();
    let outcome = history_paging::with_spans(|spans| {
        let outcome = history_paging::merge_page(&mut state.committed, spans, &entries);
        // 重复页（例如按定位补读结果页）不会插入新单元，但仍必须结算暂存终点：
        // 完整正文刚落进 pending，卡片要在这里被补全。
        apply_pending_tool_ends(state, spans);
        outcome
    });
    // 成功 / 无新内容：本次请求都已结束。成功合并时条目已被 `apply_pending_tool_ends`
    // 移除，这里是 no-op；重复页（没有新单元）也必须释放，否则 `in_flight` 永久占用。
    if let Some(refetch) = refetch {
        release_in_flight(&refetch.tool_id, refetch.request_id);
    }
    if outcome.inserted_units == 0 {
        tracing::debug!(
            session_id = %response.session_id,
            duplicates = outcome.duplicates,
            "history_page: page merged with no new content"
        );
        return;
    }
    // 并入信号：只在真的并入内容时推进代数（REQ-C01-3）。这是桥接侧对渲染侧唯一的
    // 并合事实——**不带槽位号**：桥接 `committed` 槽位与发布后渲染槽位是两把尺子
    // （发布前折叠 pass 会改变槽位数），锚点换算由渲染侧按单元身份完成
    // （`message_area::viewport_anchor`）。
    atoms::HISTORY_PREPEND_EPOCH.set(atoms::HISTORY_PREPEND_EPOCH.get().wrapping_add(1));
    tracing::info!(
        session_id = %response.session_id,
        inserted_entries = outcome.inserted_entries,
        inserted_units = outcome.inserted_units,
        duplicates = outcome.duplicates,
        inserted_slot_range = ?outcome.inserted_slot_range,
        has_more = response.next_cursor.is_some(),
        "history_page: older page merged into the transcript window"
    );
    super::render::push_acp_state(state);
}

/// 响应是否仍属于当前浏览（会话 + 快照身份）。
fn accepts_page(state: &BridgeState, response: &HistoryPageResponse) -> bool {
    response.session_id == state.active_session_id
        && atoms::HISTORY_PAGING
            .state()
            .read()
            .accepts(&response.session_id, &response.snapshot_token)
}

/// 把一页条目装配成展示单元（只读 `state` 的展示基线，不写任何状态）。
///
/// 工具卡片按 `tool_id` 在一页内配对：起点先到就原地升级成终态；终点先到（起点在更早一页）
/// 就记进 [`PENDING_TOOL_ENDS`]，等起点页并进来时补齐。
fn build_entries(
    state: &BridgeState,
    items: &[HistoryPageItem],
    origin: Option<&HistoryPageOrigin>,
    force_complete_tool: Option<&str>,
) -> Vec<PageEntry> {
    let projection: Vec<ProjectionItem> = items
        .iter()
        .map(|item| ProjectionItem {
            entry_id: item.entry_id.clone(),
            source: HistorySource::Own,
            position: Some(item.position),
            sub_items: item.sub_items.clone(),
        })
        .collect();
    project_items(state, &projection, origin, force_complete_tool)
        .into_iter()
        .map(|entry| PageEntry {
            entry_id: entry.entry_id,
            position: entry.position.unwrap_or(0),
            units: entry.units,
        })
        .collect()
}

/// 复用同一份历史投影语义（reminder/tool/文本），窗口页也只调用这里。
///
/// `force_complete_tool` 只由补读响应携带：只有该 `tool_id` 的未配对终点允许
/// 一次性保留超预算正文；普通页一律按预算决定是否驻留。
pub(super) fn project_items(
    state: &BridgeState,
    items: &[ProjectionItem],
    origin: Option<&HistoryPageOrigin>,
    force_complete_tool: Option<&str>,
) -> Vec<ProjectedEntry> {
    let mut entries: Vec<ProjectedEntry> = Vec::with_capacity(items.len());
    // 页内 `tool_id` → (条目下标, 单元下标)。
    let mut tool_slots: HashMap<String, (usize, usize)> = HashMap::new();
    for item in items {
        let mut units: Vec<TuiRenderUnit> = Vec::new();
        for sub in &item.sub_items {
            if sub.system_reminder {
                if let Ok(reminder) = serde_json::from_value(sub.update.clone())
                    && let Some(unit) =
                        TuiSystemReminder::from_wire(reminder).map(TuiRenderUnit::TuiSystemReminder)
                {
                    units.push(unit);
                }
                continue;
            }
            let Some(event) = crate::kit::acp_notifier::decode_history_sub_item(&sub.update) else {
                continue;
            };
            match event {
                AcpEventData::CommittedAssistantText { text, reasoning } => {
                    units.push(super::turn::committed_assistant_unit(&text, &reasoning));
                }
                AcpEventData::ReplayToolStarted {
                    tool_id,
                    tool_name,
                    input_summary,
                    raw_input,
                } => {
                    let unit = super::tool::replay_tool_started_unit(
                        &tool_id,
                        &tool_name,
                        &input_summary,
                        &raw_input,
                        state.last_successful_todos.as_ref(),
                    );
                    tool_slots.insert(tool_id, (entries.len(), units.len()));
                    units.push(unit);
                }
                AcpEventData::ReplayToolEnded {
                    tool_id,
                    output_summary,
                    is_error,
                } => {
                    if !complete_in_place(
                        &mut entries,
                        &tool_slots,
                        &tool_id,
                        &output_summary,
                        is_error,
                    ) {
                        // 起点在更早一页：先记下**完整输出**与来源页定位，那一页
                        // 并进来时补齐。摘要不能冒充完整结果；超出内存预算时保留
                        // 定位，等用户展开再补读。
                        let source = origin.map(|origin| ToolResultSource {
                            session_id: origin.session_id.clone(),
                            snapshot_token: origin.snapshot_token.clone(),
                            cursor: origin.cursor.clone(),
                            limit: origin.limit,
                            entry_id: item.entry_id.clone(),
                            entry_position: item.position.unwrap_or(0),
                            sub_item_position: sub.position,
                            tool_id: tool_id.clone(),
                        });
                        with_pending(|pending| {
                            let force = force_complete_tool == Some(tool_id.as_str());
                            let entry =
                                PersistentEntryKey::new(item.source.clone(), item.entry_id.clone());
                            pending.insert(
                                tool_id,
                                entry,
                                &output_summary,
                                is_error,
                                source,
                                force,
                            );
                        });
                    }
                }
                // 回放的 user 气泡：只建单元，不走 `handle_local_user_bubble`
                // （那里会 flush current_turn 并推进 turn 代际）。`input_id` 不参与
                // 展示（队列认领是执行侧状态），历史页不消费它。
                AcpEventData::ReplayedUserBubble { text, .. }
                | AcpEventData::LocalUserBubble { text, .. } => {
                    units.push(TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(text)));
                }
                // 其余变体（usage_update / 未知 tag / 无展示形式的投影）在历史投影里
                // 没有对应物：跳过，不猜测。
                _ => {}
            }
        }
        if units.is_empty() {
            continue;
        }
        entries.push(ProjectedEntry {
            entry_id: item.entry_id.clone(),
            source: item.source.clone(),
            position: item.position,
            units,
        });
    }
    entries
}

/// 页内配对：把已经放进 `entries` 的卡片原地升级成终态。
fn complete_in_place(
    entries: &mut [ProjectedEntry],
    tool_slots: &HashMap<String, (usize, usize)>,
    tool_id: &str,
    output_summary: &str,
    is_error: bool,
) -> bool {
    let Some((entry_index, unit_index)) = tool_slots.get(tool_id).copied() else {
        return false;
    };
    let unit = entries[entry_index].units[unit_index].clone();
    entries[entry_index].units[unit_index] = complete_unit(unit, output_summary, is_error);
    true
}

/// 未配对终点：在已并入的**历史来源**区间里找同 `tool_id` 的卡片补齐；找不到就继续留着。
///
/// [来源限定] 实时 turn 的卡片在 `TurnDone` 后同样落进 `committed`；若不限来源，
/// 历史页「结果先到、起点未到」的暂存结果会被补到同 `tool_id` 的实时卡片上
/// （REQ-H03、A10：不得挂到当前实时 turn）。区间索引只覆盖历史页与回放的自有
/// 条目，用它限定配对范围即可。
type CompleteToolResult = (String, Arc<str>, bool, bool);
/// `(tool_id, is_error, has_source)`：占位也必须保留工具真实成功/失败语义。
type DeferredToolResult = (String, bool, bool);

pub(super) fn apply_pending_tool_ends(
    state: &mut BridgeState,
    spans: &history_paging::HistorySpans,
) {
    let (complete, deferred): (Vec<CompleteToolResult>, Vec<DeferredToolResult>) =
        with_pending(|pending| {
            let mut complete = Vec::new();
            let mut deferred = Vec::new();
            for (tool_id, end) in &pending.entries {
                match &end.output {
                    Some(output) => {
                        complete.push((
                            tool_id.clone(),
                            Arc::clone(output),
                            end.is_error,
                            end.force_complete,
                        ));
                    }
                    None if !end.placeholder_applied => {
                        deferred.push((tool_id.clone(), end.is_error, end.source.is_some()));
                    }
                    None => {}
                }
            }
            (complete, deferred)
        });
    if complete.is_empty() && deferred.is_empty() {
        return;
    }
    for (tool_id, output, is_error, force_complete) in complete {
        let Some(slot) = tool_card_slot(state, spans, &tool_id) else {
            if force_complete {
                // 卡片已被淘汰：补读正文不再有展示对象，立即释放，避免内存悬空。
                with_pending(|pending| {
                    pending.remove(&tool_id);
                });
            }
            continue;
        };
        let unit = state.committed[slot].clone();
        let updated = if force_complete {
            complete_unit_preserving_fold(unit, output.as_ref(), is_error)
        } else {
            complete_unit(unit, output.as_ref(), is_error)
        };
        state.committed = state.committed.update(slot, updated);
        with_pending(|pending| {
            pending.remove(&tool_id);
        });
    }
    for (tool_id, is_error, has_source) in deferred {
        let Some(slot) = tool_card_slot(state, spans, &tool_id) else {
            continue;
        };
        let text = if has_source {
            DEFERRED_TOOL_RESULT_TEXT
        } else {
            UNAVAILABLE_TOOL_RESULT_TEXT
        };
        let unit = state.committed[slot].clone();
        let updated = deferred_unit(unit, text, is_error);
        state.committed = state.committed.update(slot, updated.clone());
        with_pending(|pending| {
            if let Some(entry) = pending.entries.get_mut(&tool_id) {
                entry.placeholder_applied = true;
            }
        });
        // 结果页晚于用户展开到达：占位落地时卡片仍是打开态，立即按需补读。
        if matches!(
            &updated,
            TuiRenderUnit::TuiToolCard(card) if card.fold != FoldState::Collapsed
        ) {
            let _ = maybe_request_tool_result(&updated);
        }
    }
    // 预算/条数上限在插入时执行；这里不再整表清空。
}

fn tool_card_slot(
    state: &BridgeState,
    spans: &history_paging::HistorySpans,
    tool_id: &str,
) -> Option<usize> {
    spans.slots().into_iter().find_map(|(slot, len)| {
        (slot..slot.saturating_add(len)).find(|index| {
            matches!(
                state.committed.get(*index),
                Some(TuiRenderUnit::TuiToolCard(card)) if card.tool_id == tool_id
            )
        })
    })
}

/// 用来源页请求定位补读完整工具结果；返回只含目标子项的合成页。
///
/// 身份核对分两层：响应 `sessionId/snapshotToken` 必须与来源一致；目标条目与
/// 子项（`entryId` + `subItem.position` + `toolCallId`）必须唯一命中。
/// 预算内未返回按普通失败处理（不失效浏览身份），调用方负责释放 `in_flight`
/// 与在卡片上显式呈现失败。
pub(super) async fn refetch_tool_result(
    client: &AcpTuiClient,
    source: &ToolResultSource,
) -> Result<HistoryPageResponse, String> {
    refetch_tool_result_with_budget(client, source, TOOL_RESULT_REFETCH_TIMEOUT).await
}

pub(super) async fn refetch_tool_result_with_budget(
    client: &AcpTuiClient,
    source: &ToolResultSource,
    budget: Duration,
) -> Result<HistoryPageResponse, String> {
    if !atoms::HISTORY_PAGING
        .state()
        .read()
        .accepts(&source.session_id, &source.snapshot_token)
    {
        return Err("历史结果补读失败：当前浏览身份已变化".to_string());
    }
    let raw = peri_time::timeout(
        budget,
        client.load_history_page(&source.session_id, &source.cursor, source.limit),
    )
    .await
    .map_err(|_| format!("历史结果补读失败：请求超时（{budget:?}）"))?
    .map_err(|error| format!("历史结果补读失败：{error}"))?;
    let response = history_paging::decode_page_response(&raw)
        .ok_or_else(|| "历史结果补读失败：响应形状不符".to_string())?;
    if response.session_id != source.session_id || response.snapshot_token != source.snapshot_token
    {
        return Err("历史结果补读失败：快照身份已变化".to_string());
    }
    let item = response
        .items
        .iter()
        .find(|item| item.entry_id == source.entry_id)
        .ok_or_else(|| "历史结果补读失败：目标条目不存在".to_string())?;
    if item.position != source.entry_position {
        return Err("历史结果补读失败：条目位置与来源不一致".to_string());
    }
    let sub_item = item
        .sub_items
        .iter()
        .find(|sub| sub.position == source.sub_item_position)
        .ok_or_else(|| "历史结果补读失败：目标子项不存在".to_string())?;
    let decoded = crate::kit::acp_notifier::decode_history_sub_item(&sub_item.update)
        .ok_or_else(|| "历史结果补读失败：子项无法解码".to_string())?;
    match decoded {
        AcpEventData::ReplayToolEnded {
            tool_id,
            output_summary,
            is_error,
        } if tool_id == source.tool_id => Ok(synthetic_result_response(
            source,
            output_summary.as_str(),
            is_error,
            sub_item.position,
        )),
        _ => Err("历史结果补读失败：工具结果身份不匹配".to_string()),
    }
}

/// 用户展开工具卡片时触发。返回被认领的来源定位，便于测试与诊断。
pub(super) fn maybe_request_tool_result(unit: &TuiRenderUnit) -> Option<ToolResultSource> {
    let TuiRenderUnit::TuiToolCard(card) = unit else {
        return None;
    };
    if card.fold == FoldState::Collapsed {
        return None;
    }
    let request_id = next_refetch_request_id();
    let source = with_pending(|pending| pending.deferred_source(&card.tool_id, request_id))?;
    let Some(client) = atoms::ACP_CLIENT_HANDLE.get().cloned() else {
        release_in_flight(&card.tool_id, request_id);
        notify_tool_result_failure("历史结果补读失败：ACP client 不可用");
        return Some(source);
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        release_in_flight(&card.tool_id, request_id);
        notify_tool_result_failure("历史结果补读失败：异步运行时不可用");
        return Some(source);
    };
    let task_source = source.clone();
    handle.spawn(async move {
        let (response, error) = match refetch_tool_result(&client, &task_source).await {
            Ok(response) => (response, None),
            Err(message) => {
                notify_tool_result_failure(&message);
                // 错误正文仍走同一合成页形状，桥接侧按 `refetch.error` 只更新卡片，
                // 不消费暂存来源（保留重试能力）。
                (
                    tool_result_error_response(&task_source, &message),
                    Some(message),
                )
            }
        };
        let Some(tx) = atoms::LOCAL_EVENT_TX.get() else {
            release_in_flight(&task_source.tool_id, request_id);
            notify_tool_result_failure("历史结果补读失败：本地桥接通道不可用");
            return;
        };
        let event = AcpEventWithEpoch {
            event: AcpEventData::HistoryPageLoaded {
                response: Box::new(response),
                origin: None,
                refetch: Some(Box::new(ToolResultRefetch {
                    request_id,
                    tool_id: task_source.tool_id.clone(),
                    error,
                })),
            },
            active_session_id: task_source.session_id,
        };
        if tx.send(event).is_err() {
            release_in_flight(&task_source.tool_id, request_id);
            notify_tool_result_failure("历史结果补读失败：本地桥接通道已关闭");
        }
    });
    Some(source)
}

fn synthetic_result_response(
    source: &ToolResultSource,
    output: &str,
    is_error: bool,
    sub_item_position: u64,
) -> HistoryPageResponse {
    let update = serde_json::json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": source.tool_id,
        "rawOutput": output,
        "status": if is_error { "failed" } else { "completed" },
    });
    HistoryPageResponse {
        session_id: source.session_id.clone(),
        snapshot_token: source.snapshot_token.clone(),
        next_cursor: None,
        items: vec![HistoryPageItem {
            entry_id: source.entry_id.clone(),
            position: source.entry_position,
            oversize: false,
            sub_items: vec![HistorySubItem {
                position: sub_item_position,
                update,
                system_reminder: false,
            }],
        }],
    }
}

fn tool_result_error_response(source: &ToolResultSource, message: &str) -> HistoryPageResponse {
    synthetic_result_response(source, message, true, source.sub_item_position)
}

fn notify_tool_result_failure(message: &str) {
    *atoms::NOTIFICATION.state().write() = Some(atoms::Notification {
        message: message.to_string(),
        until: peri_time::monotonic_now() + std::time::Duration::from_secs(8),
    });
}

/// 补读失败：在卡片上显式显示原因（`is_error`），但**不消费**暂存来源——
/// 释放 `in_flight_request` 后，用户再次展开即可重试。
fn apply_refetch_failure(state: &mut BridgeState, tool_id: &str, message: &str) {
    let updated = history_paging::with_spans(|spans| {
        let Some(slot) = tool_card_slot(state, spans, tool_id) else {
            // 卡片已被淘汰：来源不再有展示对象，立即释放。
            with_pending(|pending| {
                pending.remove(tool_id);
            });
            return false;
        };
        let unit = state.committed[slot].clone();
        state.committed = state
            .committed
            .update(slot, complete_unit_preserving_fold(unit, message, true));
        with_pending(|pending| {
            if let Some(entry) = pending.entries.get_mut(tool_id) {
                // 失败文案已落地：后续普通页不必再用占位覆盖它；来源仍保留。
                entry.placeholder_applied = true;
            }
        });
        true
    });
    if updated {
        super::render::push_acp_state(state);
    }
}

/// 释放该请求的在途认领；只有代次匹配才清空（迟到的旧响应不得清掉新请求）。
/// 返回本次请求是否真的持有认领。
fn release_in_flight(tool_id: &str, request_id: u64) -> bool {
    with_pending(|pending| {
        let Some(entry) = pending.entries.get_mut(tool_id) else {
            return false;
        };
        if entry.in_flight_request == Some(request_id) {
            entry.in_flight_request = None;
            true
        } else {
            false
        }
    })
}

/// 按本次释放的持久条目身份精准清理暂存终点，返回移除条数。
///
/// 每条暂存终点在写入时记录了结果条目身份（[`PendingToolEnd::entry`]），因此这里
/// 只删除释放集合覆盖的条目；其他段等待配对的工具结果继续保留。生产写入路径
/// 不存在无身份条目，无需回退到全局清空。
pub(super) fn remove_pending_tool_ends_for_entries(entries: &HashSet<PersistentEntryKey>) -> usize {
    with_pending(|pending| pending.remove_entries(entries))
}

/// 测试清理：丢弃全部暂存终点（生产释放走按条目精准清理）。
#[cfg(test)]
pub(super) fn clear_pending_tool_ends() {
    *PENDING_TOOL_ENDS.lock() = None;
}

/// 测试清理：丢弃全部暂存终点。
///
/// 暂存表是进程级静态状态，跨用例残留会改写后续用例的卡片终态
/// （`tool_id` 撞名时）；清理点与写入点是同一份实现。
#[cfg(test)]
pub(crate) fn clear_pending_tool_ends_for_tests() {
    clear_pending_tool_ends();
}

#[cfg(test)]
pub(crate) fn pending_tool_ends_fingerprint() -> (usize, usize) {
    with_pending(|pending| (pending.len(), pending.bytes))
}

#[cfg(test)]
pub(crate) fn pending_tool_ends_limits_for_tests() -> (usize, usize) {
    (PENDING_TOOL_ENDS_LIMIT, PENDING_TOOL_ENDS_BYTES_MAX)
}

#[cfg(test)]
pub(crate) fn pending_tool_end_entry_for_tests(tool_id: &str) -> Option<PersistentEntryKey> {
    with_pending(|pending| {
        pending
            .entries
            .get(tool_id)
            .map(|entry| entry.entry.clone())
    })
}

#[cfg(test)]
pub(crate) fn insert_pending_tool_end_for_tests(
    tool_id: &str,
    entry: PersistentEntryKey,
    output: &str,
    is_error: bool,
) {
    with_pending(|pending| {
        pending.insert(tool_id.to_string(), entry, output, is_error, None, false);
    });
}

#[cfg(test)]
pub(crate) fn deferred_tool_result_source_for_tests(tool_id: &str) -> Option<ToolResultSource> {
    with_pending(|pending| {
        pending
            .entries
            .get(tool_id)
            .filter(|entry| entry.output.is_none())
            .and_then(|entry| entry.source.clone())
    })
}

#[cfg(test)]
pub(crate) fn insert_deferred_tool_result_for_tests(tool_id: &str, source: ToolResultSource) {
    with_pending(|pending| {
        pending.remove(tool_id);
        pending.entries.insert(
            tool_id.to_string(),
            PendingToolEnd {
                is_error: false,
                entry: PersistentEntryKey::own(source.entry_id.clone()),
                output: None,
                source: Some(source),
                force_complete: false,
                placeholder_applied: false,
                in_flight_request: None,
            },
        );
        pending.order.push_back(tool_id.to_string());
    });
}

/// 测试用：认领一次补读（等价于用户展开卡片）。
#[cfg(test)]
pub(crate) fn claim_deferred_tool_result_for_tests(
    tool_id: &str,
    request_id: u64,
) -> Option<ToolResultSource> {
    with_pending(|pending| pending.deferred_source(tool_id, request_id))
}

#[cfg(test)]
pub(crate) fn deferred_in_flight_request_for_tests(tool_id: &str) -> Option<u64> {
    with_pending(|pending| {
        pending
            .entries
            .get(tool_id)
            .and_then(|entry| entry.in_flight_request)
    })
}

#[cfg(test)]
pub(crate) fn refetch_timeout_for_tests() -> Duration {
    TOOL_RESULT_REFETCH_TIMEOUT
}

#[cfg(test)]
pub(crate) fn tool_result_error_response_for_tests(
    source: &ToolResultSource,
    message: &str,
) -> HistoryPageResponse {
    tool_result_error_response(source, message)
}

/// 用回放路径同一份构造函数把「结果缺失」卡片升级成终态。
fn complete_unit(unit: TuiRenderUnit, output_summary: &str, is_error: bool) -> TuiRenderUnit {
    match unit {
        TuiRenderUnit::TuiToolCard(card) => TuiRenderUnit::TuiToolCard(
            super::tool::completed_tool_card(&card, output_summary, is_error),
        ),
        other => other,
    }
}

/// 按需补读完成：保留用户当前的展开态，避免补读后卡片突然收起。
fn complete_unit_preserving_fold(
    unit: TuiRenderUnit,
    output_summary: &str,
    is_error: bool,
) -> TuiRenderUnit {
    match unit {
        TuiRenderUnit::TuiToolCard(card) => {
            let fold = card.fold;
            let user_modified = card.user_modified;
            let mut updated = super::tool::completed_tool_card(&card, output_summary, is_error);
            updated.fold = fold;
            updated.user_modified = user_modified;
            updated.recompute_hash();
            TuiRenderUnit::TuiToolCard(updated)
        }
        other => other,
    }
}

/// 超预算占位：不是结果摘要，也不改变用户当前展开态；工具成功/失败语义如实保留。
fn deferred_unit(unit: TuiRenderUnit, text: &str, is_error: bool) -> TuiRenderUnit {
    complete_unit_preserving_fold(unit, text, is_error)
}

/// 在未配对终点表上执行一段操作（会话身份与表内容同源：换会话即作废）。
fn with_pending<R>(f: impl FnOnce(&mut PendingToolResults) -> R) -> R {
    let session_id = atoms::ACTIVE_SESSION_ID.state().read().clone();
    let mut guard = PENDING_TOOL_ENDS.lock();
    let (session, pending) =
        guard.get_or_insert_with(|| (session_id.clone(), PendingToolResults::default()));
    if *session != session_id {
        pending.clear();
        *session = session_id;
    }
    f(pending)
}
