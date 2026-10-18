//! 历史窗口桥接事件：`peri/history_committed` 与窗口页接纳。
//!
//! 这里只更新 `BridgeState.window` 与窗口账目，不调用工具完成、Todo、审批、输入或
//! steer handler；也不修改 `phase`/`current_request_id`/`turn_generation`/
//! `last_successful_todos`。段释放留给 S6.2。

use std::collections::{HashMap, HashSet};

use super::BridgeState;
use super::PublicationIntent;
use super::history_page::{self, ProjectedEntry, ProjectionItem};
use crate::kit::history_page_consumer::{self, WindowPagePlacement, WindowRequestContext};
use crate::kit::history_paging::{HistorySubItem, WindowPageResponse};
use crate::kit::history_window::{
    HistoryEntryView, HistorySource, PageDraft, PagePlacement, PersistentEntryKey,
    PersistentSubItemKey, SegmentDraft, SegmentOrigin,
};
use crate::kit::tui_render_unit::{TuiNoteLevel, TuiRenderUnit, TuiSystemNote};

pub(super) fn handle_history_committed(
    state: &mut BridgeState,
    info: &crate::kit::history_window::HistoryCommitInfo,
) {
    if state.active_session_id != info.session_id {
        return;
    }
    match state.window.apply_commit(info) {
        crate::kit::history_window::CommitOutcome::Applied { sealed_segments } => {
            tracing::debug!(
                checkpoint_id = %info.checkpoint_id,
                sealed_segments,
                "history_window: commit sealed display segments"
            );
        }
        crate::kit::history_window::CommitOutcome::Duplicate => {
            tracing::debug!(
                checkpoint_id = %info.checkpoint_id,
                "history_window: duplicate commit ignored"
            );
        }
        crate::kit::history_window::CommitOutcome::StaleGeneration => {
            tracing::debug!(
                checkpoint_id = %info.checkpoint_id,
                "history_window: stale commit generation/structure dropped"
            );
        }
        crate::kit::history_window::CommitOutcome::UnknownWindow => {
            tracing::debug!(
                checkpoint_id = %info.checkpoint_id,
                "history_window: commit without open window dropped"
            );
        }
        crate::kit::history_window::CommitOutcome::InvalidTail => {
            state
                .window
                .set_degraded("history commit carried an invalid committed tail");
        }
    }
}

/// S5.4：窗口页到达桥接任务。只登记窗口段/账目，不改执行状态。
///
/// 窗口页到 `committed` 的展示合并尚未在本切片接线；这里完成预留、接纳、
/// gap 布局与发布后结算，渲染正文仍由旧 `committed` 路径持有。
pub(super) fn handle_window_degraded(state: &mut BridgeState, session_id: &str, reason: &str) {
    if state.active_session_id != session_id {
        return;
    }
    state.window.set_degraded(reason);
    tracing::debug!(
        reason,
        "history_window: consumer degraded the bridge window"
    );
}

pub(super) fn handle_window_page_loaded(
    state: &mut BridgeState,
    context: &WindowRequestContext,
    response: &WindowPageResponse,
) -> PublicationIntent {
    if !state.window.is_enabled() {
        history_page_consumer::fail_window_request(
            &context.request_id,
            "history window is disabled",
            false,
        );
        return PublicationIntent::None;
    }
    if state.active_session_id != context.session_id {
        history_page_consumer::fail_window_request(
            &context.request_id,
            "history window page belongs to another session",
            true,
        );
        return PublicationIntent::None;
    }
    if response.reopen_required {
        state.window.set_degraded("history window requires reopen");
        history_page_consumer::fail_window_request(
            &context.request_id,
            "history window requires reopen",
            true,
        );
        return PublicationIntent::None;
    }
    let mut allow_display_merge = true;
    if context.is_open {
        crate::kit::atoms::HISTORY_INHERITED_NOTICE.set(None);
        crate::kit::history_window::clear_detail_targets();
        let live_empty = live_region_is_empty(state);
        allow_display_merge = live_empty;
        let generation = state.window.begin_window(response.identity.clone());
        history_page_consumer::note_window_generation(generation);
        if live_empty {
            // 初始 open 采用窗口视图：旧回放内容让位给窗口页，避免同一身份双写。
            // live/未持久区域不为空时绝不清空。
            state.committed.clear();
            crate::kit::history_paging::reset_browsing_state();
            state.window.clear_displayed_entries();
            crate::kit::history_paging::sync_spans_from_window(&state.window);
            crate::kit::history_window::clear_window_unit_mappings();
            crate::kit::history_window::clear_detail_targets();
            crate::kit::atoms::HISTORY_INHERITED_NOTICE.set(None);
        }
        crate::kit::history_window::publish_window_viewport(&state.window);
        crate::kit::history_paging::publish_window_gap_request(&state.window);
    } else if state.window.identity() != Some(&response.identity)
        || state.window.generation() != context.window_generation
    {
        history_page_consumer::fail_window_request(
            &context.request_id,
            "stale history window page dropped before admission",
            false,
        );
        return PublicationIntent::None;
    }
    if state.window.is_degraded() {
        history_page_consumer::fail_window_request(
            &context.request_id,
            "history window is degraded; waiting for open retry",
            false,
        );
        return PublicationIntent::None;
    }
    if !context.is_open {
        sync_window_anchor_from_viewport(state);
    }

    match state
        .window
        .reserve_page(&context.request_id, context.reserved_bytes)
    {
        crate::kit::history_window::ReserveOutcome::Granted(_) => {}
        other => {
            history_page_consumer::fail_window_request(
                &context.request_id,
                format!("history window page reservation rejected: {other:?}"),
                false,
            );
            return PublicationIntent::None;
        }
    }
    let projection = projection_items(response);
    let projected = history_page::project_items(state, &projection);
    let mut projected_by_key: HashMap<PersistentEntryKey, ProjectedEntry> = projected
        .into_iter()
        .map(|entry| {
            (
                PersistentEntryKey::new(entry.source.clone(), entry.entry_id.clone()),
                entry,
            )
        })
        .collect();
    let mut views: Vec<(
        PersistentEntryKey,
        ProjectionItem,
        Vec<TuiRenderUnit>,
        Option<crate::kit::history_window::DetailDescriptor>,
    )> = Vec::new();
    let mut seen = HashSet::new();
    for (window_item, item) in response.items.iter().zip(projection) {
        let key = PersistentEntryKey::new(item.source.clone(), item.entry_id.clone());
        if state.window.is_entry_displayed(&key)
            || state.window.index().contains_entry(&key)
            || !seen.insert(key.clone())
        {
            continue;
        }
        let mut units = projected_by_key
            .remove(&key)
            .map(|entry| entry.units)
            .unwrap_or_default();
        let descriptor = window_item.descriptor.as_ref().map(|descriptor| {
            crate::kit::history_window::DetailDescriptor::new(
                item.source.clone(),
                item.entry_id.clone(),
                descriptor.total_bytes,
                descriptor.chunk_size,
                descriptor
                    .snapshot_token
                    .clone()
                    .unwrap_or_else(|| response.identity.snapshot_token.clone()),
                descriptor.read_ticket.clone(),
            )
        });
        if units.is_empty()
            && let Some(descriptor) = descriptor.as_ref()
        {
            let text = format!(
                "超大内容 · {:.1} KiB · 按需加载详情",
                descriptor.total_bytes as f64 / 1024.0
            );
            units.push(TuiRenderUnit::TuiSystemNote(TuiSystemNote {
                content_hash: crate::kit::history_window::placeholder_hash(
                    &key,
                    descriptor.total_bytes,
                ),
                text,
                level: TuiNoteLevel::Info,
            }));
        }
        views.push((key, item, units, descriptor));
    }
    let segments = build_page_segments(&views, response);
    let insert_slot = match context.placement {
        WindowPagePlacement::Tail => state.committed.len(),
        WindowPagePlacement::BeforeFirst => state
            .window
            .index()
            .entries_in_slot_order()
            .first()
            .map_or(0, |(_, slot, _)| *slot),
    };
    let placement = match context.placement {
        WindowPagePlacement::Tail => PagePlacement::Tail,
        WindowPagePlacement::BeforeFirst => {
            match state.window.index().entries_in_slot_order().first() {
                Some((entry, _, _)) => PagePlacement::BeforeEntry(entry.clone()),
                None => PagePlacement::Tail,
            }
        }
    };
    let decision = state.window.admit_page(PageDraft {
        request_id: context.request_id.clone(),
        window_generation: state.window.generation(),
        identity: response.identity.clone(),
        placement,
        segments,
    });
    match decision {
        crate::kit::history_window::AdmissionDecision::Accepted { .. } => {
            let mut targets = Vec::new();
            for (key, _, _, descriptor) in &views {
                if let Some(descriptor) = descriptor {
                    state.window.register_detail_descriptor(descriptor.clone());
                    targets.push(crate::kit::history_window::HistoryDetailTarget {
                        content_hash: crate::kit::history_window::placeholder_hash(
                            key,
                            descriptor.total_bytes,
                        ),
                        descriptor: descriptor.clone(),
                    });
                }
            }
            crate::kit::history_window::publish_detail_targets(targets);
            if response.inherited_unsupported {
                crate::kit::atoms::HISTORY_INHERITED_NOTICE.set(Some(
                    "继承历史在冻结快照中不可分段读取；当前继承段保持固定。".into(),
                ));
                *crate::kit::atoms::NOTIFICATION.state().write() =
                    Some(crate::kit::atoms::Notification {
                        message: "继承历史不可分段读取；当前继承段保持固定".into(),
                        until: peri_time::monotonic_now() + std::time::Duration::from_secs(8),
                    });
            }
            if allow_display_merge {
                merge_window_units(state, insert_slot, context, &views);
            }
            let pending = state.window.take_pending_release_plans();
            if !pending.is_empty() {
                crate::kit::history_window::release_window_plans(
                    state,
                    pending,
                    crate::kit::history_window::ReleaseReason::BudgetPressure,
                );
            }
            crate::kit::history_window::publish_window_viewport(&state.window);
            crate::kit::history_paging::publish_window_gap_request(&state.window);
            if state.window.pending_merge_request_id().is_none() {
                history_page_consumer::finish_window_merge(&context.request_id);
                PublicationIntent::None
            } else {
                PublicationIntent::Deferred
            }
        }
        crate::kit::history_window::AdmissionDecision::Rejected(rejection) => {
            history_page_consumer::fail_window_request(
                &context.request_id,
                format!("history page admission rejected: {:?}", rejection.reason),
                false,
            );
            PublicationIntent::None
        }
        crate::kit::history_window::AdmissionDecision::Disabled => {
            history_page_consumer::fail_window_request(
                &context.request_id,
                "history window admission is disabled",
                false,
            );
            PublicationIntent::None
        }
    }
}

/// S6.2：释放事务清理未配对工具终点（S6.5 预算化后仍复用此入口）。
pub(crate) fn clear_pending_tool_ends() {
    history_page::clear_pending_tool_ends();
}

#[cfg(test)]
pub(crate) fn pending_tool_ends_fingerprint() -> (usize, usize) {
    history_page::pending_tool_ends_fingerprint()
}

#[cfg(test)]
pub(crate) fn insert_pending_tool_end_for_tests(tool_id: &str, output: &str, is_error: bool) {
    history_page::insert_pending_tool_end_for_tests(tool_id, output, is_error);
}

/// 发布后结算待合并页：窗口账本与 consumer 预留同时归还。
pub(super) fn settle_after_publication(state: &mut BridgeState) {
    let Some(request_id) = state.window.pending_merge_request_id() else {
        return;
    };
    state.window.settle_pending_merge();
    history_page_consumer::finish_window_merge(&request_id);
}

fn projection_items(response: &WindowPageResponse) -> Vec<ProjectionItem> {
    response
        .items
        .iter()
        .map(|item| ProjectionItem {
            entry_id: item.entry_id.clone(),
            source: item.source.clone(),
            position: item.position,
            sub_items: item
                .sub_items
                .iter()
                .map(|sub| HistorySubItem {
                    position: u64::from(sub.position),
                    update: sub.update.clone(),
                    system_reminder: sub.system_reminder,
                })
                .collect(),
        })
        .collect()
}

fn build_page_segments(
    views: &[(
        PersistentEntryKey,
        ProjectionItem,
        Vec<TuiRenderUnit>,
        Option<crate::kit::history_window::DetailDescriptor>,
    )],
    response: &WindowPageResponse,
) -> Vec<SegmentDraft> {
    let mut groups: Vec<(HistorySource, Vec<HistoryEntryView>)> = Vec::new();
    for (key, item, units, _) in views {
        let mut body_bytes = 0usize;
        let mut reserved_bytes: usize = if units.is_empty() { 0 } else { 64 };
        for sub in &item.sub_items {
            let bytes = serde_json::to_vec(&sub.update)
                .map(|value| value.len())
                .unwrap_or(0);
            body_bytes = body_bytes.saturating_add(bytes);
            // 容器/字段开销的显式估算；S6.1 的统一 retained_bytes 会替换这里。
            reserved_bytes = reserved_bytes.saturating_add(bytes).saturating_add(48);
        }
        for unit in units {
            if let TuiRenderUnit::TuiSystemNote(note) = unit {
                body_bytes = body_bytes.saturating_add(note.text.capacity());
                reserved_bytes = reserved_bytes
                    .saturating_add(note.text.capacity())
                    .saturating_add(std::mem::size_of_val(note));
            }
        }
        let view = HistoryEntryView::new(
            key.clone(),
            item.position,
            units.len(),
            body_bytes,
            reserved_bytes,
            item.sub_items
                .iter()
                .filter_map(|sub| {
                    u32::try_from(sub.position)
                        .ok()
                        .map(|position| PersistentSubItemKey::new(key.clone(), position))
                })
                .collect(),
        );
        match groups.last_mut() {
            Some((source, entries)) if *source == view.key.source => entries.push(view),
            _ => groups.push((view.key.source.clone(), vec![view])),
        }
    }
    let last = groups.len().saturating_sub(1);
    groups
        .into_iter()
        .enumerate()
        .map(|(index, (source, entries))| {
            let body_bytes = entries.iter().map(|entry| entry.body_bytes).sum();
            let reserved_bytes = entries.iter().map(|entry| entry.reserved_bytes).sum();
            let mut draft = SegmentDraft::new(source, entries, body_bytes, reserved_bytes)
                .origin(SegmentOrigin::Page);
            if index == 0 {
                draft.before_cursor = response.before_cursor.clone();
            }
            if index == last {
                draft.after_cursor = response.after_cursor.clone();
            }
            draft
        })
        .collect()
}

fn merge_window_units(
    state: &mut BridgeState,
    insert_slot: usize,
    context: &WindowRequestContext,
    views: &[(
        PersistentEntryKey,
        ProjectionItem,
        Vec<TuiRenderUnit>,
        Option<crate::kit::history_window::DetailDescriptor>,
    )],
) {
    let mut cursor = insert_slot;
    let mut inserted = 0usize;
    for (_, _, units, _) in views {
        for unit in units {
            state.committed.insert(cursor, unit.clone());
            cursor = cursor.saturating_add(1);
            inserted = inserted.saturating_add(1);
        }
    }
    let entries: Vec<(PersistentEntryKey, usize)> = views
        .iter()
        .filter(|(_, _, units, _)| !units.is_empty())
        .map(|(key, _, units, _)| (key.clone(), units.len()))
        .collect();
    state.window.mark_display_block(&entries, insert_slot);
    if inserted > 0 && context.placement == WindowPagePlacement::BeforeFirst {
        crate::kit::atoms::HISTORY_PREPEND_EPOCH.set(
            crate::kit::atoms::HISTORY_PREPEND_EPOCH
                .get()
                .wrapping_add(1),
        );
    }
    let spans = crate::kit::history_paging::HistorySpans::from_resident_window(&state.window);
    history_page::apply_pending_tool_ends(state, &spans);
    crate::kit::history_paging::sync_spans_from_window(&state.window);
    let mappings = views
        .iter()
        .flat_map(|(key, _, units, _)| {
            units
                .iter()
                .map(|unit| crate::kit::history_window::WindowUnitMapping {
                    token: crate::kit::message_area::window_identity_token(unit),
                    entry: key.clone(),
                })
        })
        .collect();
    crate::kit::history_window::publish_window_unit_mappings(mappings);
}
fn live_region_is_empty(state: &BridgeState) -> bool {
    state.current_turn.is_empty()
        && !state
            .current_turn
            .tool_cards
            .iter()
            .any(|tool| tool.output_summary.is_none())
        && !state
            .current_turn
            .subagents
            .iter()
            .any(|sub| sub.is_running)
}

/// 把真实渲染视口发布的持久锚点同步到窗口模型，供 pin/淘汰/gap 决策使用。
fn sync_window_anchor_from_viewport(state: &mut BridgeState) {
    let handle = crate::kit::atoms::WINDOW_VIEWPORT.state();
    let anchor = handle
        .read()
        .as_ref()
        .and_then(|viewport| viewport.anchor.clone());
    if let Some(anchor) = anchor {
        state.window.set_anchor(Some(anchor));
    }
}
