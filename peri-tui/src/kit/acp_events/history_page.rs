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

use std::collections::HashMap;

use parking_lot::Mutex;

use super::BridgeState;
use crate::kit::acp_types::AcpEventData;
use crate::kit::atoms;
use crate::kit::history_paging::{self, HistoryPageItem, HistoryPageResponse, PageEntry};
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiSystemReminder, TuiUserBubble};

/// 已加载窗口里还没有起点的工具结束事件：`tool_id` → `(会话, 输出, 是否错误)`。
///
/// 页边界会把 `tool_call` 与它的 `tool_call_update` 分到相邻两页，而**更晚**的一页
/// 先被读到——那时卡片还不存在。留着它，等起点页并进来时补齐；会话一换就作废
/// （结果不能跨会话错配到同 id 的卡片上）。
static PENDING_TOOL_ENDS: Mutex<Option<(String, HashMap<String, (String, bool)>)>> =
    Mutex::new(None);

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
    let entries = build_entries(state, &response.items);
    // 并入前先关闭回放区间：生产路径**没有**回放结束通知（`SessionReplayDone` 是
    // 死路径，见 `turn::handle_session_replay_done` 的注释），首屏最后一个自有条目
    // 会一直停留在「打开」状态、不进索引。那时索引可能整段为空——整页更早的历史会
    // 被插到槽位 0，越过继承前缀（违反 REQ-H01-6、A20）。合并前收口即可拿到正确的
    // 插入点：所有未标记单元都排在首个已标记的自有条目之前。
    history_paging::close_own_entry();
    let outcome = history_paging::with_spans(|spans| {
        let outcome = history_paging::merge_page(&mut state.committed, spans, &entries);
        if outcome.inserted_units > 0 {
            apply_pending_tool_ends(state, spans);
        }
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
fn build_entries(state: &BridgeState, items: &[HistoryPageItem]) -> Vec<PageEntry> {
    let mut entries: Vec<PageEntry> = Vec::with_capacity(items.len());
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
                        // 起点在更早一页：先记下来，那一页并进来时补齐。
                        with_pending(|pending| {
                            pending.insert(tool_id, (output_summary, is_error));
                        });
                    }
                }
                // 回放的 user 气泡：只建单元，不走 `handle_local_user_bubble`
                // （那里会 flush current_turn 并推进 turn 代际）。`input_id` 不参与
                // 展示（队列认领是执行侧状态），历史页不消费它。
                AcpEventData::ReplayedUserBubble { text, .. }
                | AcpEventData::LocalUserBubble { text } => {
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
        entries.push(PageEntry {
            entry_id: item.entry_id.clone(),
            position: item.position,
            units,
        });
    }
    entries
}

/// 页内配对：把已经放进 `entries` 的卡片原地升级成终态。
fn complete_in_place(
    entries: &mut [PageEntry],
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
fn apply_pending_tool_ends(state: &mut BridgeState, spans: &history_paging::HistorySpans) {
    let pending: Vec<(String, String, bool)> = with_pending(|pending| {
        pending
            .iter()
            .map(|(tool_id, (output, is_error))| (tool_id.clone(), output.clone(), *is_error))
            .collect()
    });
    if pending.is_empty() {
        return;
    }
    let history_slots = spans.slots();
    for (tool_id, output_summary, is_error) in pending {
        let slot = history_slots.iter().find_map(|(slot, len)| {
            (*slot..slot.saturating_add(*len)).find(|index| {
                matches!(
                    state.committed.get(*index),
                    Some(TuiRenderUnit::TuiToolCard(card)) if card.tool_id == tool_id
                )
            })
        });
        let Some(slot) = slot else {
            continue;
        };
        let unit = state.committed[slot].clone();
        state.committed = state
            .committed
            .update(slot, complete_unit(unit, &output_summary, is_error));
        with_pending(|pending| {
            pending.remove(&tool_id);
        });
    }
    with_pending(|pending| {
        if pending.len() > PENDING_TOOL_ENDS_LIMIT {
            // 上限只防无界增长（例如一整段终点都落在已加载窗口之前）：超出的先丢掉，
            // 起点仍以「结果缺失」展示，只是不会错配到别的卡片上。
            pending.clear();
        }
    });
}

/// 未配对终点的上限（超过即丢弃，避免长会话里无界增长）。
const PENDING_TOOL_ENDS_LIMIT: usize = 256;

/// 测试清理：丢弃全部暂存终点。
///
/// 暂存表是进程级静态状态，跨用例残留会改写后续用例的卡片终态
/// （`tool_id` 撞名时）；清理点与写入点是同一份实现。
#[cfg(test)]
pub(crate) fn clear_pending_tool_ends_for_tests() {
    *PENDING_TOOL_ENDS.lock() = None;
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

/// 在未配对终点表上执行一段操作（会话身份与表内容同源：换会话即作废）。
fn with_pending<R>(f: impl FnOnce(&mut HashMap<String, (String, bool)>) -> R) -> R {
    let session_id = atoms::ACTIVE_SESSION_ID.state().read().clone();
    let mut guard = PENDING_TOOL_ENDS.lock();
    let (session, pending) = guard.get_or_insert_with(|| (session_id.clone(), HashMap::new()));
    if *session != session_id {
        pending.clear();
        *session = session_id;
    }
    f(pending)
}
