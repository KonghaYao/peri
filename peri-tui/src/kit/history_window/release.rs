//! S6.2 统一释放事务与 pending release/ack。
//!
//! 模型层只记录“正文已从 canonical 移除、等待渲染 ack”的保守额度；正文与派生
//! holder 的删除由 [`crate::kit::acp_events::history_window`] 的桥接事务完成。

use std::collections::HashSet;

use super::HistoryWindow;
use super::model::{PersistentEntryKey, ReleasePlan, ReleaseReason};

/// 唯一释放事务入口：只允许在 bridge 串行链调用。
pub fn release_window_plans(
    state: &mut crate::kit::acp_events::BridgeState,
    mut plans: Vec<ReleasePlan>,
    reason: ReleaseReason,
) -> ReleaseOutcome {
    tracing::debug!(
        ?reason,
        plan_count = plans.len(),
        "history_window: consume release plans"
    );
    let anchor_entry = state.window.anchor().map(|anchor| anchor.entry.clone());
    let mut keys = HashSet::new();
    let mut outcome = ReleaseOutcome::default();
    let mut accepted = Vec::new();
    plans.retain(|plan| {
        let touches_anchor = anchor_entry
            .as_ref()
            .is_some_and(|anchor| plan.entries.iter().any(|entry| entry == anchor));
        if touches_anchor && state.window.segment(plan.segment).is_some() {
            state.window.pin(plan.segment, super::PinReason::Viewport);
            outcome.skipped += 1;
            return false;
        }
        if state.window.segment(plan.segment).is_some() {
            let Some(segment) = state.window.segment(plan.segment) else {
                return false;
            };
            if !segment.durable || segment.pins.is_pinned() {
                outcome.skipped += 1;
                return false;
            }
            let Some(applied) = state.window.apply_release_plan(plan) else {
                outcome.skipped += 1;
                return false;
            };
            accepted.push(applied.plan);
        } else {
            accepted.push(plan.clone());
        }
        for entry in &plan.entries {
            keys.insert(entry.clone());
        }
        true
    });
    state.window.clear_taken_release_plans();
    if accepted.is_empty() && keys.is_empty() {
        return outcome;
    }

    let mut ranges = state.window.take_display_ranges(&keys);
    ranges.sort_unstable();
    let mut removed_units = 0usize;
    for (start, len) in ranges.iter().rev() {
        let tail = state.committed.split_off(start.saturating_add(*len));
        let removed = state.committed.split_off(*start);
        removed_units = removed_units.saturating_add(removed.len());
        for unit in &removed {
            if let crate::kit::tui_render_unit::TuiRenderUnit::TuiToolCard(card) = unit {
                crate::kit::acp_events::history_window::forget_released_tool(
                    &state.active_session_id,
                    &card.tool_id,
                );
            }
        }
        state.committed.append(tail);
    }
    state.folded_history.invalidate();
    crate::kit::acp_events::render::invalidate_structural_caches();
    crate::kit::history_window::remove_window_unit_mappings(&keys);
    crate::kit::history_window::remove_detail_targets(&keys);
    crate::kit::history_paging::sync_spans_from_window(&state.window);
    // 只清理本次释放集合覆盖的暂存终点：写入时记录了结果条目身份，映射可靠；
    // 不做全局清空，避免释放一个段时误删其他段等待配对的工具结果。
    let removed_pending_tool_ends =
        crate::kit::acp_events::history_window::remove_pending_tool_ends_for_entries(&keys);
    tracing::debug!(
        removed_pending_tool_ends,
        "history_window: pruned pending tool ends for released entries"
    );
    let focus = {
        let handle = crate::kit::atoms::FOCUSED_ENTRY.state();
        handle.read().clone()
    };
    if let Some(focus) = focus
        && ranges
            .iter()
            .any(|(start, len)| (*start..start.saturating_add(*len)).contains(&focus.slot))
    {
        crate::kit::atoms::FOCUSED_ENTRY.set(None);
    }
    state.window.note_released_entries(&keys);

    let body_bytes = accepted.iter().map(|plan| plan.body_bytes).sum::<usize>();
    let structural_epoch = crate::kit::atoms::STRUCTURAL_EPOCH.get().saturating_add(1);
    crate::kit::atoms::STRUCTURAL_EPOCH.set(structural_epoch);
    state
        .window
        .record_pending_release(body_bytes, structural_epoch);
    outcome.plans = accepted;
    outcome.released_entries = keys.len();
    outcome.released_units = removed_units;
    outcome.body_bytes = body_bytes;
    outcome.structural_epoch = structural_epoch;
    crate::kit::atoms::HISTORY_WINDOW_STATS.set(state.window.diagnostics());
    let detail_key = crate::kit::atoms::HISTORY_DETAIL
        .state()
        .read()
        .as_ref()
        .map(|detail| detail.descriptor.key.clone());
    if detail_key.as_ref().is_some_and(|key| keys.contains(key)) {
        crate::kit::panels::history_detail::close_history_detail();
    }
    let (edge_before, edge_after) = state.window.edge_cursors();
    crate::kit::history_page_consumer::note_window_cursors(edge_before, edge_after);
    crate::kit::history_window::publish_window_viewport(&state.window);
    crate::kit::history_paging::publish_window_gap_request(&state.window);
    outcome
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleaseOutcome {
    pub plans: Vec<ReleasePlan>,
    pub released_entries: usize,
    pub released_units: usize,
    pub body_bytes: usize,
    pub structural_epoch: u64,
    pub skipped: usize,
}

impl HistoryWindow {
    /// 释放事务登记“已移除正文、等待渲染 ack”的保守额度。
    pub fn record_pending_release(&mut self, bytes: usize, epoch: u64) {
        self.budget.pending_release_bytes = self.budget.pending_release_bytes.saturating_add(bytes);
        self.pending_release_epoch = self.pending_release_epoch.max(epoch);
    }

    /// 渲染侧 ack 后归还保守额度；重复/迟到 ack 幂等。
    pub fn ack_release_epoch(&mut self, achieved_epoch: u64) {
        if achieved_epoch >= self.pending_release_epoch {
            self.budget.pending_release_bytes = 0;
        }
    }

    pub fn pending_release_bytes(&self) -> usize {
        self.budget.pending_release_bytes
    }

    pub fn pending_release_epoch(&self) -> u64 {
        self.pending_release_epoch
    }

    pub fn clear_taken_release_plans(&mut self) {
        self.pending_release.clear();
    }

    pub fn set_detail_window_bytes(&mut self, bytes: usize) {
        self.budget.detail_bytes = bytes;
    }
}

impl HistoryWindow {
    /// 按身份移除展示账本区间；返回升序的 `(start, len)`，供 committed 删除。
    pub fn take_display_ranges(
        &mut self,
        keys: &HashSet<PersistentEntryKey>,
    ) -> Vec<(usize, usize)> {
        self.display.remove_entries_at(keys)
    }

    /// 释放事务的条目级 UI 清理接口（详情窗口 S7.3 接入）。
    pub fn note_released_entries(&self, _keys: &HashSet<PersistentEntryKey>) {}
}

#[allow(dead_code)]
fn _release_reason_marker(_: ReleaseReason) {}
