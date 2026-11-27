//! 历史窗口桥接事件：`peri/history_committed` 与窗口页接纳。
//!
//! 更新窗口账目、展示正文并经统一事务释放段，不调用工具完成、Todo、审批、输入或
//! steer handler；也不修改 `phase`/`current_request_id`/`turn_generation`/
//! `last_successful_todos`。

use std::collections::{HashMap, HashSet};

use super::super::BridgeState;
use super::super::PublicationIntent;
use super::super::history_page::{self, ProjectedEntry, ProjectionItem};
use crate::kit::history_page_consumer::{self, WindowPagePlacement, WindowRequestContext};
use crate::kit::history_paging::{HistorySubItem, WindowPageResponse};
use crate::kit::history_window::{
    HistoryEntryView, HistorySource, PageDraft, PagePlacement, PersistentEntryKey,
    PersistentSubItemKey, SegmentDraft, SegmentKey, SegmentOrigin,
};
use crate::kit::tui_render_unit::{TuiNoteLevel, TuiRenderUnit, TuiSystemNote};

/// 用户展开工具卡片时的按需补读入口（`history_page` 是 acp_events 私有模块，
/// 统一从这里暴露给 message_area 的手势/键盘折叠路径）。
pub(crate) fn maybe_request_tool_result(
    unit: &TuiRenderUnit,
) -> Option<history_page::ToolResultSource> {
    history_page::maybe_request_tool_result(unit)
}

/// 展示单元释放后清掉 live 工具关联，避免跨窗口代次留下悬挂引用。
pub(crate) fn forget_released_tool(session_id: &str, tool_id: &str) {
    super::super::live_tool_links::forget_tool(session_id, tool_id);
}

#[cfg(test)]
pub(crate) fn insert_deferred_tool_result_for_tests(
    tool_id: &str,
    source: history_page::ToolResultSource,
) {
    history_page::insert_deferred_tool_result_for_tests(tool_id, source);
}

#[cfg(test)]
pub(crate) fn deferred_tool_result_source_for_tests(
    tool_id: &str,
) -> Option<history_page::ToolResultSource> {
    history_page::deferred_tool_result_source_for_tests(tool_id)
}

#[cfg(test)]
pub(crate) fn claim_deferred_tool_result_for_tests(
    tool_id: &str,
    request_id: u64,
) -> Option<history_page::ToolResultSource> {
    history_page::claim_deferred_tool_result_for_tests(tool_id, request_id)
}

#[cfg(test)]
pub(crate) fn deferred_in_flight_request_for_tests(tool_id: &str) -> Option<u64> {
    history_page::deferred_in_flight_request_for_tests(tool_id)
}

#[cfg(test)]
pub(crate) fn refetch_timeout_for_tests() -> std::time::Duration {
    history_page::refetch_timeout_for_tests()
}

#[cfg(test)]
pub(crate) fn tool_result_error_response_for_tests(
    source: &history_page::ToolResultSource,
    message: &str,
) -> crate::kit::history_paging::HistoryPageResponse {
    history_page::tool_result_error_response_for_tests(source, message)
}

pub(in super::super) fn handle_history_committed(
    state: &mut BridgeState,
    info: &crate::kit::history_window::HistoryCommitInfo,
) {
    if state.active_session_id != info.session_id {
        return;
    }
    apply_commit_and_release(state, info);
}

/// 应用一次提交事实；有视口锚点时立刻消费保留计划并释放正文。
///
/// 提交事实有两个来源：`peri/history_committed` 通知，以及窗口页响应自带的
/// `committedTail`/`checkpointId`。后者让逐条 position 提升后不必等下一次
/// Full Compact 才封印。
pub(super) fn apply_commit_and_release(
    state: &mut BridgeState,
    info: &crate::kit::history_window::HistoryCommitInfo,
) {
    match state.window.apply_commit(info) {
        crate::kit::history_window::CommitOutcome::Applied { sealed_segments } => {
            tracing::debug!(
                checkpoint_id = %info.checkpoint_id,
                sealed_segments,
                "history_window: commit sealed display segments"
            );
            if state.window.anchor().is_some() {
                let plans = state.window.retention_plan_after_commit();
                crate::kit::history_window::release_window_plans(
                    state,
                    plans,
                    crate::kit::history_window::ReleaseReason::Commit,
                );
            } else {
                // 无视口锚点时不可释放：无法证明当前可见页边界，淘汰可能移除整页。
                tracing::debug!(
                    checkpoint_id = %info.checkpoint_id,
                    "history_window: commit release skipped without viewport anchor"
                );
            }
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
pub(in super::super) fn handle_window_degraded(
    state: &mut BridgeState,
    session_id: &str,
    reason: &str,
) {
    if state.active_session_id != session_id {
        return;
    }
    state.window.set_degraded(reason);
    tracing::debug!(
        reason,
        "history_window: consumer degraded the bridge window"
    );
}

pub(in super::super) fn handle_window_page_loaded(
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
    let open_plan = if context.is_open {
        crate::kit::atoms::HISTORY_INHERITED_NOTICE.set(None);
        crate::kit::history_window::clear_detail_targets();
        match super::open_display::prepare_open_display(state, context, response) {
            Ok(plan) => Some(plan),
            Err(failure) => {
                tracing::error!(
                    ranges = ?failure.ranges,
                    committed_len = failure.committed_len,
                    "history_window: display ledger ranges inconsistent with committed; keeping prior window"
                );
                state
                    .window
                    .set_degraded("history display ledger inconsistent on reopen");
                history_page_consumer::fail_window_request(
                    &context.request_id,
                    "history display ledger inconsistent on reopen",
                    true,
                );
                return PublicationIntent::None;
            }
        }
    } else if state.window.identity() != Some(&response.identity)
        || state.window.generation() != context.window_generation
    {
        history_page_consumer::fail_window_request(
            &context.request_id,
            "stale history window page dropped before admission",
            false,
        );
        return PublicationIntent::None;
    } else {
        None
    };
    if open_plan
        .as_ref()
        .is_some_and(|plan| !plan.allow_display_merge)
    {
        // 无法安全判定旧展示边界：不预留、不接纳，页面留待后续翻页重新获取。
        history_page_consumer::fail_window_request(
            &context.request_id,
            "history window page cannot be merged safely",
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
    // The previous page may already be on screen since its publication sampled the budget.
    state
        .window
        .ack_release_epoch(crate::kit::atoms::RENDER_ACHIEVED_EPOCH.get());

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
    let projected = history_page::project_items(state, &projection, None, None);
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
    {
        let iteration_items = response.items.iter().zip(projection);
        for (window_item, item) in iteration_items {
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
    }
    let mut page_tool_entries = Vec::new();
    if open_plan.as_ref().is_some_and(|plan| plan.adopt_enabled) {
        // adopt 只认「live 尾部实际持有的持久条目」：从 committed 中的消息身份
        // （user 的 input id / assistant 的 messageId）与工具身份反推。不能用
        // 「位置大于当前展示水位」推断——已被预算淘汰的历史条目不在 committed、
        // 也不在账本，但位置仍高于水位；按位置 adopt 会把它们标记为已展示，
        // 随后被去重逻辑永久跳过（复审 P2-1）。
        let mut retained = RetainedLiveEntries::collect(state);
        let mut kept = Vec::with_capacity(views.len());
        for (key, item, mut units, descriptor) in views {
            match retained.adoption(&key, &units) {
                AdoptScope::Whole => {
                    // 工具卡没有 live messageId：页条目给出持久身份时记下映射，
                    // 供本身尚未建立段的 live 卡片回填（见下方 accept 分支）。
                    retained.bind_page_tool_entries(&key, &units, item.position);
                    // 正文已由 live 尾部持有：只登记展示身份与持久位置，不进入
                    // segments/index，也不再次插入 committed。
                    state.window.mark_entry_displayed(key, item.position);
                }
                AdoptScope::ToolUnits(held) => {
                    // 条目身份不在 live 尾部，只有工具子项被 live 卡片持有：只跳过
                    // 这些子项，兄弟正文仍须随页插入。整条标记为已展示会让文本永久
                    // 丢失，后续 open 也被展示身份去重挡住。
                    retained.bind_page_tool_entries(&key, &units, item.position);
                    let mut index = 0usize;
                    units.retain(|_| {
                        let keep = !held.contains(&index);
                        index += 1;
                        keep
                    });
                    if units.is_empty() {
                        state.window.mark_entry_displayed(key, item.position);
                    } else {
                        kept.push((key, item, units, descriptor));
                    }
                }
                AdoptScope::None => kept.push((key, item, units, descriptor)),
            }
        }
        views = kept;
        page_tool_entries = retained.page_tool_entries;
    }
    let segments = build_page_segments(&views, response);
    let insert_slot = if let Some(slot) = open_plan.as_ref().and_then(|plan| plan.insert_slot) {
        slot
    } else if let Some(gap_id) = context.gap {
        gap_insert_slot(state, gap_id)
    } else {
        match context.placement {
            WindowPagePlacement::Tail => state.committed.len(),
            WindowPagePlacement::BeforeFirst => state
                .window
                .index()
                .entry_keys_in_slot_order()
                .next()
                .and_then(|key| state.window.displayed_range(key))
                .map_or(0, |(slot, _)| slot),
        }
    };
    let placement = if let Some(gap_id) = context.gap {
        PagePlacement::FillGap(gap_id)
    } else if open_plan
        .as_ref()
        .and_then(|plan| plan.insert_slot)
        .is_some()
    {
        // open 页插在保留 live 前面时，模型顺序必须与 committed 同步。
        state
            .window
            .resident_entry_at_or_after_slot(insert_slot)
            .map_or(PagePlacement::Tail, |key| {
                PagePlacement::BeforeEntry(key.clone())
            })
    } else {
        match context.placement {
            WindowPagePlacement::Tail => PagePlacement::Tail,
            WindowPagePlacement::BeforeFirst => {
                match state.window.index().entry_keys_in_slot_order().next() {
                    Some(entry) => PagePlacement::BeforeEntry(entry.clone()),
                    None => PagePlacement::Tail,
                }
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
            // 去重后的 views 不含 live 条目；只在整页采纳后按响应的持久身份提升。
            for item in &response.items {
                if let Some(position) = item.position {
                    let key = PersistentEntryKey::new(item.source.clone(), item.entry_id.clone());
                    state.window.promote_live_seal_position(&key, position);
                }
            }
            // open 页按 tool_id 给出 live 工具卡的持久身份：先绑定；实际补登记推迟到
            // 页正文与展示账本落定之后（见 `merge_window_units` 之后），这样槽位与
            // 布局顺序都按最终 `committed` 计算。
            let bound = super::super::live_tool_links::bind_page_entries(
                &state.active_session_id,
                &page_tool_entries,
            );
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
            merge_window_units(state, insert_slot, context, &views);
            // 页正文与展示账本已落定：此时补登记「已归档但仍未归属」的工具卡，
            // 段的布局位置与槽位都按最终 `committed` 计算（不得先登记后被页插入
            // 右移），字节同时从 unattributed 移出。
            if !bound.is_empty() {
                let slots = live_tool_slots(state, &bound);
                state.register_live_seal_segments(slots.into_iter());
                // 新段改变了 index 槽位；重新发布 spans 并补齐在途工具终点。
                publish_spans_and_pair_tool_ends(state);
            }
            let pending = state.window.take_pending_release_plans();
            if !pending.is_empty() {
                crate::kit::history_window::release_window_plans(
                    state,
                    pending,
                    crate::kit::history_window::ReleaseReason::BudgetPressure,
                );
            }
            // 页响应自带 durable tail/checkpoint，等于逐条持久回执：提升过的
            // live 条目可在本次响应内封印并进入释放计划，不必等下一次 Full Compact。
            if let (Some(committed_tail), Some(checkpoint_id)) = (
                response.committed_tail.as_ref(),
                response.checkpoint_id.as_ref(),
            ) {
                let info = crate::kit::history_window::HistoryCommitInfo {
                    session_id: response.session_id.clone(),
                    session_generation: response.identity.session_generation.clone(),
                    structure_version: response.identity.structure_version,
                    checkpoint_id: checkpoint_id.clone(),
                    summary_entry_id: None,
                    committed_tail: committed_tail.clone(),
                };
                apply_commit_and_release(state, &info);
            }
            // 空页已由 consumer 按方向清空游标（EOF/无更多行）；再按驻留边缘回写会
            // 把该方向重新“点亮”，导致对同一 cursor 无限重试。adopt-only 页同样
            // 不建段：此时布局为空，边缘游标是 `(None, None)`，回写会覆盖
            // open 响应将在 finish_window_merge 时提交的边界。
            if !response.items.is_empty() && state.window.segment_count() > 0 {
                let (edge_before, edge_after) = state.window.edge_cursors();
                history_page_consumer::note_window_cursors(
                    Some(&context.request_id),
                    edge_before,
                    edge_after,
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

/// S6.2：释放事务按本次释放的持久条目身份精准清理未配对工具终点。
pub(crate) fn remove_pending_tool_ends_for_entries(entries: &HashSet<PersistentEntryKey>) -> usize {
    history_page::remove_pending_tool_ends_for_entries(entries)
}

/// 测试清理：丢弃全部未配对工具终点（生产释放走按条目精准清理）。
#[cfg(test)]
pub(crate) fn clear_pending_tool_ends() {
    history_page::clear_pending_tool_ends();
}

#[cfg(test)]
pub(crate) fn pending_tool_ends_fingerprint() -> (usize, usize) {
    history_page::pending_tool_ends_fingerprint()
}

#[cfg(test)]
pub(crate) fn pending_tool_ends_limits_for_tests() -> (usize, usize) {
    history_page::pending_tool_ends_limits_for_tests()
}

#[cfg(test)]
pub(crate) fn pending_tool_end_entry_for_tests(tool_id: &str) -> Option<PersistentEntryKey> {
    history_page::pending_tool_end_entry_for_tests(tool_id)
}

#[cfg(test)]
pub(crate) fn insert_pending_tool_end_for_tests(
    tool_id: &str,
    entry: PersistentEntryKey,
    output: &str,
    is_error: bool,
) {
    history_page::insert_pending_tool_end_for_tests(tool_id, entry, output, is_error);
}

/// 发布后结算待合并页：窗口账本与 consumer 预留同时归还。
pub(in super::super) fn settle_after_publication(state: &mut BridgeState) {
    let Some(request_id) = state.window.pending_merge_request_id() else {
        return;
    };
    state.window.settle_pending_merge();
    history_page_consumer::finish_window_merge(&request_id);
    // The render publication sampled the budget before this reservation was returned.
    crate::kit::atoms::HISTORY_WINDOW_STATS.set(state.window.diagnostics());
}

pub(super) fn projection_items(response: &WindowPageResponse) -> Vec<ProjectionItem> {
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

/// `committed` 中尚未被任何展示账目区间覆盖的指定工具卡槽位。
///
/// 页回填只用于「已归档但仍未归属」的卡片：已进入 LiveSeal/页段的单元必须跳过，
/// 否则 reconcile 会把同一单元再次并入段，重复计数。
pub(super) fn live_tool_slots(state: &BridgeState, tool_ids: &[String]) -> Vec<usize> {
    let wanted: HashSet<&str> = tool_ids.iter().map(String::as_str).collect();
    let covered = state.window.display.slot_ranges();
    state
        .committed
        .iter()
        .enumerate()
        .filter(|(slot, _)| {
            !covered
                .iter()
                .any(|(start, len)| (*start..start.saturating_add(*len)).contains(slot))
        })
        .filter_map(|(slot, unit)| match unit {
            TuiRenderUnit::TuiToolCard(card) if wanted.contains(card.tool_id.as_str()) => {
                Some(slot)
            }
            _ => None,
        })
        .collect()
}

pub(super) fn build_page_segments(
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

pub(super) fn merge_window_units(
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
    let inserted_before_existing = insert_slot < state.committed.len();
    let entries: Vec<(PersistentEntryKey, usize)> = views
        .iter()
        .filter(|(_, _, units, _)| !units.is_empty())
        .map(|(key, _, units, _)| (key.clone(), units.len()))
        .collect();
    if !entries.is_empty() {
        let page = views
            .iter()
            .flat_map(|(_, _, units, _)| units.iter().cloned());
        if insert_slot == 0 {
            let retained = std::mem::take(&mut state.committed);
            state.committed = crate::kit::tui_render_unit::prepend_units(page.collect(), retained);
        } else {
            let tail = state.committed.split_off(insert_slot);
            state.committed.extend(page);
            state.committed.append(tail);
        }
    }
    state.window.mark_display_block(&entries, insert_slot);
    if !entries.is_empty()
        && (context.placement == WindowPagePlacement::BeforeFirst || inserted_before_existing)
    {
        crate::kit::atoms::HISTORY_PREPEND_EPOCH.set(
            crate::kit::atoms::HISTORY_PREPEND_EPOCH
                .get()
                .wrapping_add(1),
        );
    }
    publish_spans_and_pair_tool_ends(state);
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

fn publish_spans_and_pair_tool_ends(state: &mut BridgeState) {
    let spans = crate::kit::history_paging::HistorySpans::from_resident_window(&state.window);
    history_page::apply_pending_tool_ends(state, &spans);
    crate::kit::history_paging::with_spans(|published| *published = spans);
}

/// 旧回放在 `committed` 里的严格前缀末尾。只有「回放区间之后确实还有单元」时
/// 才返回边界；否则无法区分旧回放与后来追加的 live 内容，调用方必须保守处理。
pub(super) fn known_replay_prefix_end(state: &BridgeState) -> Option<usize> {
    let slots = crate::kit::history_paging::with_spans(|spans| spans.slots());
    let end = slots
        .iter()
        .map(|(slot, len)| slot.saturating_add(*len))
        .max()?;
    (end > 0 && end < state.committed.len()).then_some(end)
}

/// 缺口在 `committed` 中的插入槽位：缺口后首个驻留条目的槽位；缺口在尾部时
/// 等于当前展示长度。
pub(super) fn gap_insert_slot(
    state: &BridgeState,
    gap_id: crate::kit::history_window::GapId,
) -> usize {
    let layout = state.window.layout();
    let Some(position) = layout
        .iter()
        .position(|region| *region == crate::kit::history_window::RegionKey::Gap(gap_id))
    else {
        return state.committed.len();
    };
    {
        let iteration_items = layout.iter().skip(position + 1);
        for region in iteration_items {
            let crate::kit::history_window::RegionKey::Resident(key) = region else {
                continue;
            };
            if let Some(slot) = segment_start_slot(state, *key) {
                return slot;
            }
        }
    }
    // 缺口在尾部：插到前一个驻留段之后、live 追加内容之前，而不是盲目
    // `committed.len()`（否则 live 消息会被新页挤到旧历史之前）。
    if let Some(key) = layout
        .iter()
        .take(position)
        .rev()
        .find_map(|region| match region {
            crate::kit::history_window::RegionKey::Resident(key) => Some(*key),
            crate::kit::history_window::RegionKey::Gap(_) => None,
        })
        && let Some(slot) = segment_end_slot(state, key)
    {
        return slot;
    }
    0
}

pub(super) fn segment_start_slot(state: &BridgeState, key: SegmentKey) -> Option<usize> {
    state
        .window
        .segment_display_bounds(key)
        .map(|(start, _)| start)
}

pub(super) fn segment_end_slot(state: &BridgeState, key: SegmentKey) -> Option<usize> {
    state.window.segment_display_bounds(key).map(|(_, end)| end)
}
pub(super) fn live_region_is_empty(state: &BridgeState) -> bool {
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
pub(super) fn sync_window_anchor_from_viewport(state: &mut BridgeState) {
    let handle = crate::kit::atoms::WINDOW_VIEWPORT.state();
    let anchor = handle
        .read()
        .as_ref()
        .and_then(|viewport| viewport.anchor.clone());
    if let Some(anchor) = anchor {
        state.window.set_anchor(Some(anchor));
    }
}

use super::open_display::AdoptScope;
use super::open_display::RetainedLiveEntries;
