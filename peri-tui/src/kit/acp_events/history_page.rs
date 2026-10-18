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

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use parking_lot::Mutex;

use super::BridgeState;
use crate::acp_client::AcpTuiClient;
use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch};
use crate::kit::atoms;
use crate::kit::history_page_consumer::{HistoryPageOrigin, take_page_origin};
use crate::kit::history_paging::{
    self, HistoryPageItem, HistoryPageResponse, HistorySubItem, PageEntry,
};
use crate::kit::history_window::HistorySource;
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
const PENDING_TOOL_ENDS_LIMIT: usize = 256;
/// 跨页暂存**完整结果**的内存预算。预算内按正文保留；超过预算的条目改为
/// 保存来源页请求定位，等用户展开时再用 `load_history_page()` 补读，绝不截断正文。
const PENDING_TOOL_ENDS_BYTES_MAX: usize = 8 * 1024 * 1024;
const DEFERRED_TOOL_RESULT_TEXT: &str = "结果正文较大，展开后按需加载";
const UNAVAILABLE_TOOL_RESULT_TEXT: &str = "结果正文较大，无法按需加载";

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
    /// `None` = 超出内存预算，保留来源定位等待按需补读。
    output: Option<Arc<str>>,
    source: Option<ToolResultSource>,
    /// 用户展开触发的补读结果必须跳过预算再次转引用（一次性正文）。
    force_complete: bool,
    placeholder_applied: bool,
    in_flight: bool,
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
        output: &str,
        is_error: bool,
        source: Option<ToolResultSource>,
    ) {
        let previous = self.remove(&tool_id);
        let source = source.or_else(|| previous.as_ref().and_then(|entry| entry.source.clone()));
        let force_complete = take_force_complete_tool(&tool_id);
        let output = (force_complete || output.len() <= PENDING_TOOL_ENDS_BYTES_MAX)
            .then(|| Arc::from(output));
        let end = PendingToolEnd {
            is_error,
            output,
            source,
            force_complete,
            placeholder_applied: false,
            in_flight: false,
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
        self.bytes = self.bytes.saturating_sub(removed.retained_bytes());
        self.order.retain(|id| id != tool_id);
        Some(removed)
    }

    /// 用户展开卡片：取出来源定位并标记在途；已完整/在途的条目不重复补读。
    fn deferred_source(&mut self, tool_id: &str) -> Option<ToolResultSource> {
        let entry = self.entries.get_mut(tool_id)?;
        if entry.output.is_some() || entry.in_flight {
            return None;
        }
        let source = entry.source.clone()?;
        entry.in_flight = true;
        Some(source)
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }

    #[allow(dead_code)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// 用户展开触发的补读：下一次 `insert` 允许一次性保留完整正文（即使超过预算）。
static FORCE_COMPLETE_TOOL: Mutex<Option<String>> = Mutex::new(None);

fn note_force_complete_tool(tool_id: &str) {
    *FORCE_COMPLETE_TOOL.lock() = Some(tool_id.to_string());
}

fn take_force_complete_tool(tool_id: &str) -> bool {
    let mut guard = FORCE_COMPLETE_TOOL.lock();
    if guard.as_deref() == Some(tool_id) {
        *guard = None;
        true
    } else {
        false
    }
}

static PENDING_TOOL_ENDS: Mutex<Option<(String, PendingToolResults)>> = Mutex::new(None);

/// 并入一页（桥接任务内调用；调用方已保证事件串行）。
pub(super) fn handle_history_page_loaded(state: &mut BridgeState, response: &HistoryPageResponse) {
    if !accepts_page(state, response) {
        tracing::debug!(
            session_id = %response.session_id,
            items = response.items.len(),
            "history_page: page dropped (not the current browsing identity)"
        );
        return;
    }
    let origin = take_page_origin(&response.session_id, &response.snapshot_token);
    let entries = build_entries(state, &response.items, origin.as_ref());
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
    project_items(state, &projection, origin)
        .into_iter()
        .map(|entry| PageEntry {
            entry_id: entry.entry_id,
            position: entry.position.unwrap_or(0),
            units: entry.units,
        })
        .collect()
}

/// 复用同一份历史投影语义（reminder/tool/文本），窗口页也只调用这里。
pub(super) fn project_items(
    state: &BridgeState,
    items: &[ProjectionItem],
    origin: Option<&HistoryPageOrigin>,
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
                            pending.insert(tool_id, &output_summary, is_error, source);
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
type DeferredToolResult = (String, bool);

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
                        deferred.push((tool_id.clone(), end.source.is_some()));
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
    for (tool_id, has_source) in deferred {
        let Some(slot) = tool_card_slot(state, spans, &tool_id) else {
            continue;
        };
        let text = if has_source {
            DEFERRED_TOOL_RESULT_TEXT
        } else {
            UNAVAILABLE_TOOL_RESULT_TEXT
        };
        let unit = state.committed[slot].clone();
        let updated = deferred_unit(unit, text);
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
pub(super) async fn refetch_tool_result(
    client: &AcpTuiClient,
    source: &ToolResultSource,
) -> Result<HistoryPageResponse, String> {
    if !atoms::HISTORY_PAGING
        .state()
        .read()
        .accepts(&source.session_id, &source.snapshot_token)
    {
        return Err("历史结果补读失败：当前浏览身份已变化".to_string());
    }
    let raw = client
        .load_history_page(&source.session_id, &source.cursor, source.limit)
        .await
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
    let source = with_pending(|pending| pending.deferred_source(&card.tool_id))?;
    let Some(client) = atoms::ACP_CLIENT_HANDLE.get().cloned() else {
        release_deferred_source(&card.tool_id);
        notify_tool_result_failure("历史结果补读失败：ACP client 不可用");
        return Some(source);
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        release_deferred_source(&card.tool_id);
        notify_tool_result_failure("历史结果补读失败：异步运行时不可用");
        return Some(source);
    };
    let task_source = source.clone();
    handle.spawn(async move {
        let response = match refetch_tool_result(&client, &task_source).await {
            Ok(response) => {
                note_force_complete_tool(&task_source.tool_id);
                response
            }
            Err(message) => {
                notify_tool_result_failure(&message);
                tool_result_error_response(&task_source, &message)
            }
        };
        let Some(tx) = atoms::LOCAL_EVENT_TX.get() else {
            release_deferred_source(&task_source.tool_id);
            notify_tool_result_failure("历史结果补读失败：本地桥接通道不可用");
            return;
        };
        let _ = tx.send(AcpEventWithEpoch {
            event: AcpEventData::HistoryPageLoaded {
                response: Box::new(response),
            },
            active_session_id: task_source.session_id,
        });
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

fn release_deferred_source(tool_id: &str) {
    with_pending(|pending| {
        if let Some(entry) = pending.entries.get_mut(tool_id) {
            entry.in_flight = false;
        }
    });
}

/// 未配对终点的上限（超过即丢弃，避免长会话里无界增长）。
/// 结构性释放：丢弃未配对终点（轻量引用，S6.5 预算化）。
pub(super) fn clear_pending_tool_ends() {
    *PENDING_TOOL_ENDS.lock() = None;
    *FORCE_COMPLETE_TOOL.lock() = None;
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
pub(crate) fn insert_pending_tool_end_for_tests(tool_id: &str, output: &str, is_error: bool) {
    with_pending(|pending| {
        pending.insert(tool_id.to_string(), output, is_error, None);
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
                output: None,
                source: Some(source),
                force_complete: false,
                placeholder_applied: false,
                in_flight: false,
            },
        );
        pending.order.push_back(tool_id.to_string());
    });
}

#[cfg(test)]
pub(crate) fn note_force_complete_tool_for_tests(tool_id: &str) {
    note_force_complete_tool(tool_id);
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

/// 超预算占位：不是结果摘要，也不改变用户当前展开态。
fn deferred_unit(unit: TuiRenderUnit, text: &str) -> TuiRenderUnit {
    complete_unit_preserving_fold(unit, text, false)
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
