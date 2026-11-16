//! Open display.

use super::super::BridgeState;
use crate::kit::history_page_consumer::{self, WindowRequestContext};
use crate::kit::history_paging::WindowPageResponse;
use crate::kit::history_window::PersistentEntryKey;
use crate::kit::tui_render_unit::TuiRenderUnit;

use std::collections::HashSet;

/// open 页对 `committed` 与窗口身份的协调计划。
pub(super) struct OpenDisplayPlan {
    /// `false` 时页面不预留、不接纳，留待后续翻页重新获取。
    pub(super) allow_display_merge: bool,
    /// `Some` 覆盖 gap/placement 的正文插入槽位。
    pub(super) insert_slot: Option<usize>,
    /// open 页中已由 live 尾部实际持有的条目：只登记身份，不插入正文。
    pub(super) adopt_enabled: bool,
}

/// live 尾部对页条目单元的持有范围。
pub(super) enum AdoptScope {
    /// 条目身份本身在 committed：整条条目由 live 尾部持有。
    Whole,
    /// 仅这些下标的工具子项由 live 卡片持有；其余单元仍须随页插入。
    ToolUnits(Vec<usize>),
    /// 条目与 live 尾部无关。
    None,
}

/// live 尾部实际持有的持久条目身份。
///
/// 只从 `committed` 的**身份字段**取证：user/assistant 气泡的 `message_id`
/// （分别为投递 input id 与 ACP `messageId`），以及工具卡片的 `tool_id`（消息
/// 可能只有工具调用、没有气泡）。位置、内容哈希或到达顺序都不能替代身份。
pub(super) struct RetainedLiveEntries {
    pub(super) session_id: String,
    pub(super) message_ids: HashSet<String>,
    pub(super) tool_ids: HashSet<String>,
    /// 页响应中按 `tool_id` 匹配到 live 卡片的持久定位。live 工具事件不带
    /// `messageId`，这份映射是页出口给出的唯一可核实身份；只收本页被 adopt 的条目。
    pub(super) page_tool_entries: Vec<(String, PersistentEntryKey, Option<u64>)>,
}

impl RetainedLiveEntries {
    pub(super) fn collect(state: &BridgeState) -> Self {
        let mut message_ids = HashSet::new();
        let mut tool_ids = HashSet::new();
        for unit in &state.committed {
            match unit {
                TuiRenderUnit::TuiUserBubble(bubble) => {
                    if let Some(message_id) = &bubble.message_id {
                        message_ids.insert(message_id.clone());
                    }
                }
                TuiRenderUnit::TuiAssistantBubble(bubble) => {
                    if let Some(message_id) = &bubble.message_id {
                        message_ids.insert(message_id.clone());
                    }
                }
                TuiRenderUnit::TuiToolCard(card) => {
                    tool_ids.insert(card.tool_id.clone());
                }
                _ => {}
            }
        }
        Self {
            session_id: state.active_session_id.clone(),
            message_ids,
            tool_ids,
            page_tool_entries: Vec::new(),
        }
    }

    /// 页条目中哪些单元已被 live 尾部持有。
    pub(super) fn adoption(&self, key: &PersistentEntryKey, units: &[TuiRenderUnit]) -> AdoptScope {
        if !key.source.is_own() {
            return AdoptScope::None;
        }
        let Some((session, message_id)) = key.entry_id.rsplit_once(':') else {
            return AdoptScope::None;
        };
        if session == self.session_id && self.message_ids.contains(message_id) {
            return AdoptScope::Whole;
        }
        let held: Vec<usize> = units
            .iter()
            .enumerate()
            .filter_map(|(index, unit)| match unit {
                TuiRenderUnit::TuiToolCard(card) if self.tool_ids.contains(&card.tool_id) => {
                    Some(index)
                }
                _ => None,
            })
            .collect();
        if held.is_empty() {
            AdoptScope::None
        } else {
            AdoptScope::ToolUnits(held)
        }
    }

    /// 被 adopt 的页条目里，工具子项若对应 committed 中的 live 卡片，记录
    /// `tool_id -> 条目/位置`。只记录 live 卡片真正持有的工具，不扩大页身份。
    pub(super) fn bind_page_tool_entries(
        &mut self,
        key: &PersistentEntryKey,
        units: &[TuiRenderUnit],
        position: Option<u64>,
    ) {
        if !key.source.is_own() {
            return;
        }
        for unit in units {
            if let TuiRenderUnit::TuiToolCard(card) = unit
                && self.tool_ids.contains(&card.tool_id)
            {
                self.page_tool_entries
                    .push((card.tool_id.clone(), key.clone(), position));
            }
        }
    }
}

/// 展示账本区间与 `committed` 不一致；拒绝猜测性删除。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DisplayRangeError {
    pub(super) ranges: Vec<(usize, usize)>,
    pub(super) committed_len: usize,
}

/// 先快照判定，再统一应用：身份、展示区间与 `committed` 在同一事务内协调。
pub(super) fn prepare_open_display(
    state: &mut BridgeState,
    context: &WindowRequestContext,
    response: &WindowPageResponse,
) -> Result<OpenDisplayPlan, DisplayRangeError> {
    let mut snapshot = state.window.switch_snapshot(&response.identity);
    let live_ranges = if snapshot.identity_changed {
        state.window.live_seal_display_ranges()
    } else {
        Vec::new()
    };
    let live_range_set: HashSet<_> = live_ranges.iter().copied().collect();
    snapshot
        .displayed_slot_ranges
        .retain(|range| !live_range_set.contains(range));
    let live_slots: Vec<_> = live_ranges
        .iter()
        .flat_map(|(start, len)| {
            let shift: usize = snapshot
                .displayed_slot_ranges
                .iter()
                .filter(|(page_start, _)| page_start < start)
                .map(|(_, len)| *len)
                .sum();
            start.saturating_sub(shift)..start.saturating_sub(shift).saturating_add(*len)
        })
        .collect();
    let live_empty = super::page_admission::live_region_is_empty(state);
    // 迟到 open：请求发出后本地展示代次已经推进（新气泡或整轮回答已进入
    // committed）。此时不能整体清空 `committed`，否则旧快照会覆盖新内容。
    let display_changed = context.display_generation != state.generation;

    let mut allow_display_merge = true;
    let mut insert_slot = None;
    if snapshot.identity_changed {
        if snapshot.displayed_entry_count > 0 {
            // 按展示账本精确摘除旧窗口页正文；live 尾部（新 turn 已 flush 的
            // 内容）不在区间内，原样保留。
            let (removed_slot, removed_tools) =
                remove_displayed_ranges(&mut state.committed, &snapshot.displayed_slot_ranges)?;
            forget_committed_tool_links(state, removed_tools);
            insert_slot = Some(removed_slot);
            // 摘除后仍有 live 尾部时，open 页中实际仍由 committed 持有的条目
            // 只登记身份，阻止后续翻页重复插入；是否持有由消息身份判定，不用
            // 位置水位推断（已被淘汰的历史条目也高于水位）。
            // adopt 与 live 区域是否为空解耦：`RetainedLiveEntries` 只认
            // committed 中实际持有的身份，仅存在于流式 turn 的条目不会被采纳，
            // 仍照常插入；反之，新一轮正在流式时 committed 里已完成的条目也必须
            // adopt，否则重开页会把它们重复插入。
            if snapshot.displayed_slot_ranges.is_empty() && !state.committed.is_empty() {
                // adopt-only：账本没有可摘除的正文，剩余 committed 全是 live 尾部
                // （无论 live 区域当前是否为空）。本页未被 adopt 的条目属于 live
                // 之前的历史，必须插到其前面。
                insert_slot = Some(0);
            }
        } else if live_empty {
            if display_changed {
                // 只有能确认旧回放是严格前缀时才替换它；否则保留全部展示并
                // 放弃这一页的展示合并（迟到 open 丢弃展示，不覆盖新内容）。
                match super::page_admission::known_replay_prefix_end(state) {
                    Some(end) => {
                        let removed_tools = tool_ids_of(state.committed.iter().take(end));
                        state.committed = state.committed.iter().skip(end).cloned().collect();
                        forget_committed_tool_links(state, removed_tools);
                        insert_slot = Some(0);
                    }
                    None => allow_display_merge = false,
                }
            } else {
                // 初始 open 采用窗口视图：旧回放内容让位给窗口页，避免同一身份双写。
                // live/未持久区域不为空时绝不清空。
                let removed_tools = tool_ids_of(state.committed.iter());
                state.committed.clear();
                forget_committed_tool_links(state, removed_tools);
                insert_slot = Some(0);
            }
        } else {
            // open 在途期间 live 区域已有新内容：保留 committed，不合并本页。
            allow_display_merge = false;
        }
    }

    let generation = state.window.begin_window(response.identity.clone());
    // 成功的 open 是唯一能把窗口从 Degraded 拉回 Ready 的路径；同身份重试时
    // `begin_window` 是 no-op，必须显式恢复。
    state.window.mark_ready();
    history_page_consumer::note_window_generation(generation);
    if snapshot.identity_changed {
        // begin_window 已清模型；同步清理派生索引、浏览态与渲染身份映射。
        crate::kit::history_paging::reset_browsing_state();
        crate::kit::history_paging::sync_spans_from_window(&state.window);
        crate::kit::history_window::clear_window_unit_mappings();
        crate::kit::history_window::clear_detail_targets();
        // 仅重登记旧窗口明确持有的 live 槽位；position/durable 待新身份的页与提交确认。
        state.register_live_seal_segments(live_slots.into_iter());
        // adopt 条目的正文仍由保留的 live 尾部持有：在新窗口里延续其展示
        // 身份与位置，否则下一次 open 会重复插入同一持久条目。
        for (key, position) in &snapshot.slotless_entries {
            state.window.mark_entry_displayed(key.clone(), *position);
        }
    }
    crate::kit::history_window::publish_window_viewport(&state.window);
    crate::kit::history_paging::publish_window_gap_request(&state.window);

    // adopt 的判据是「`committed` 里仍有 live 尾部实际持有的条目」，与本次
    // open 是否切换窗口身份无关。先前只在 `identity_changed` 分支启用：同身份
    // 重试（degraded 恢复、重复 open 响应）会跳过 `RetainedLiveEntries` 与
    // `bind_page_entries`，页里的工具子项按新正文再次插入，而 live 工具卡没有
    // 展示槽位，最终同一 `tool_id` 出现两张卡片，未归属字节也无法回落。
    // 放在所有 `committed` 变更之后求值：身份切换时被清空/摘除的条目自然不
    // 参与 adopt；同身份重试则按现有 `committed` 去重（不重置窗口、不重复入段）。
    let adopt_enabled = !state.committed.is_empty();

    Ok(OpenDisplayPlan {
        allow_display_merge,
        insert_slot,
        adopt_enabled,
    })
}

/// 从 `committed` 精确移除窗口展示区间；返回插入槽位与被移除的工具 id。
///
/// `ranges` 是 `DisplayLedger` 的 (start, len)，必须升序、互不重叠且在边界内。
/// 校验失败时不修改 `committed`（不猜测删除）。被移除的工具卡已没有展示对象，
/// 调用方据此清理 live 归属映射。
pub(super) fn remove_displayed_ranges(
    committed: &mut im::Vector<TuiRenderUnit>,
    ranges: &[(usize, usize)],
) -> Result<(usize, Vec<String>), DisplayRangeError> {
    validate_display_ranges(committed.len(), ranges)?;
    let insert_at = ranges.first().map_or(committed.len(), |(start, _)| *start);
    let mut removed_tools = Vec::new();
    // 逆序保证前面的槽位不因后面的摘除而失效。
    {
        let iteration_items = ranges.iter().rev();
        for (start, len) in iteration_items {
            let end = start.saturating_add(*len);
            let tail = committed.split_off(end);
            let removed = committed.split_off(*start);
            debug_assert_eq!(removed.len(), *len);
            removed_tools.extend(tool_ids_of(removed.iter()));
            committed.append(tail);
        }
    }
    Ok((insert_at.min(committed.len()), removed_tools))
}

/// 展示单元集合中的工具卡 id（用于清理 live 归属映射）。
pub(super) fn tool_ids_of<'a>(units: impl IntoIterator<Item = &'a TuiRenderUnit>) -> Vec<String> {
    units
        .into_iter()
        .filter_map(|unit| match unit {
            TuiRenderUnit::TuiToolCard(card) => Some(card.tool_id.clone()),
            _ => None,
        })
        .collect()
}

/// 丢弃已离开 `committed` 的工具卡的 live 归属/未归属记录。
pub(super) fn forget_committed_tool_links(
    state: &BridgeState,
    tool_ids: impl IntoIterator<Item = String>,
) {
    for tool_id in tool_ids {
        super::super::live_tool_links::forget_tool(&state.active_session_id, &tool_id);
    }
}

pub(super) fn validate_display_ranges(
    committed_len: usize,
    ranges: &[(usize, usize)],
) -> Result<(), DisplayRangeError> {
    let mut previous_end = 0usize;
    {
        let iteration_items = ranges.iter().enumerate();
        for (index, (start, len)) in iteration_items {
            let valid = *len > 0
                && start
                    .checked_add(*len)
                    .is_some_and(|end| end <= committed_len)
                && (index == 0 || *start >= previous_end);
            if !valid {
                return Err(DisplayRangeError {
                    ranges: ranges.to_vec(),
                    committed_len,
                });
            }
            previous_end = start.saturating_add(*len);
        }
    }
    Ok(())
}
