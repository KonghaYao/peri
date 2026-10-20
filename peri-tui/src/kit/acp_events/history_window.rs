//! 历史窗口桥接事件：`peri/history_committed` 与窗口页接纳。
//!
//! 更新窗口账目、展示正文并经统一事务释放段，不调用工具完成、Todo、审批、输入或
//! steer handler；也不修改 `phase`/`current_request_id`/`turn_generation`/
//! `last_successful_todos`。

use std::collections::{HashMap, HashSet};

use super::BridgeState;
use super::PublicationIntent;
use super::history_page::{self, ProjectedEntry, ProjectionItem};
use crate::kit::history_page_consumer::{self, WindowPagePlacement, WindowRequestContext};
use crate::kit::history_paging::{HistorySubItem, WindowPageResponse};
use crate::kit::history_window::{
    HistoryEntryView, HistorySource, PageDraft, PagePlacement, PersistentEntryKey,
    PersistentSubItemKey, SegmentDraft, SegmentKey, SegmentOrigin,
};
use crate::kit::tui_render_unit::{TuiNoteLevel, TuiRenderUnit, TuiSystemNote};

#[cfg(test)]
pub(crate) use history_page::ToolResultSource;

/// 用户展开工具卡片时的按需补读入口（`history_page` 是 acp_events 私有模块，
/// 统一从这里暴露给 message_area 的手势/键盘折叠路径）。
pub(crate) fn maybe_request_tool_result(
    unit: &TuiRenderUnit,
) -> Option<history_page::ToolResultSource> {
    history_page::maybe_request_tool_result(unit)
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
    let open_plan = if context.is_open {
        crate::kit::atoms::HISTORY_INHERITED_NOTICE.set(None);
        crate::kit::history_window::clear_detail_targets();
        match prepare_open_display(state, context, response) {
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
    if open_plan.as_ref().is_some_and(|plan| plan.adopt_enabled) {
        // adopt 只认「live 尾部实际持有的持久条目」：从 committed 中的消息身份
        // （user 的 input id / assistant 的 messageId）与工具身份反推。不能用
        // 「位置大于当前展示水位」推断——已被预算淘汰的历史条目不在 committed、
        // 也不在账本，但位置仍高于水位；按位置 adopt 会把它们标记为已展示，
        // 随后被去重逻辑永久跳过（复审 P2-1）。
        let retained = RetainedLiveEntries::collect(state);
        views.retain(|(key, item, units, _)| {
            let adopt = retained.holds(key, units);
            if adopt {
                // 正文已由 live 尾部持有：只登记展示身份与持久位置，不进入
                // segments/index，也不再次插入 committed。
                state
                    .window
                    .mark_entry_displayed(key.clone(), item.position);
            }
            !adopt
        });
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
                .entries_in_slot_order()
                .first()
                .and_then(|(key, _, _)| state.window.displayed_range(key))
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
            .index()
            .entries_in_slot_order()
            .into_iter()
            .find(|(key, _, _)| {
                state
                    .window
                    .displayed_range(key)
                    .is_some_and(|(slot, _)| slot >= insert_slot)
            })
            .map_or(PagePlacement::Tail, |(key, _, _)| {
                PagePlacement::BeforeEntry(key)
            })
    } else {
        match context.placement {
            WindowPagePlacement::Tail => PagePlacement::Tail,
            WindowPagePlacement::BeforeFirst => {
                match state.window.index().entries_in_slot_order().first() {
                    Some((entry, _, _)) => PagePlacement::BeforeEntry(entry.clone()),
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
            let pending = state.window.take_pending_release_plans();
            if !pending.is_empty() {
                crate::kit::history_window::release_window_plans(
                    state,
                    pending,
                    crate::kit::history_window::ReleaseReason::BudgetPressure,
                );
            }
            // 空页已由 consumer 按方向清空游标（EOF/无更多行）；再按驻留边缘回写会
            // 把该方向重新“点亮”，导致对同一 cursor 无限重试。adopt-only 页同样
            // 不建段：此时布局为空，边缘游标是 `(None, None)`，回写会覆盖
            // `settle_window_response` 从 open 响应写入的边界。
            if !response.items.is_empty() && state.window.segment_count() > 0 {
                let (edge_before, edge_after) = state.window.edge_cursors();
                history_page_consumer::note_window_cursors(edge_before, edge_after);
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

/// open 页对 `committed` 与窗口身份的协调计划。
struct OpenDisplayPlan {
    /// `false` 时页面不预留、不接纳，留待后续翻页重新获取。
    allow_display_merge: bool,
    /// `Some` 覆盖 gap/placement 的正文插入槽位。
    insert_slot: Option<usize>,
    /// open 页中已由 live 尾部实际持有的条目：只登记身份，不插入正文。
    adopt_enabled: bool,
}

/// live 尾部实际持有的持久条目身份。
///
/// 只从 `committed` 的**身份字段**取证：user/assistant 气泡的 `message_id`
/// （分别为投递 input id 与 ACP `messageId`），以及工具卡片的 `tool_id`（消息
/// 可能只有工具调用、没有气泡）。位置、内容哈希或到达顺序都不能替代身份。
struct RetainedLiveEntries {
    session_id: String,
    message_ids: HashSet<String>,
    tool_ids: HashSet<String>,
}

impl RetainedLiveEntries {
    fn collect(state: &BridgeState) -> Self {
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
        }
    }

    /// 条目身份（`{session}:{message uuid}`）或它的工具子项是否仍在 committed。
    fn holds(&self, key: &PersistentEntryKey, units: &[TuiRenderUnit]) -> bool {
        if !key.source.is_own() {
            return false;
        }
        let Some((session, message_id)) = key.entry_id.rsplit_once(':') else {
            return false;
        };
        (session == self.session_id && self.message_ids.contains(message_id))
            || units.iter().any(|unit| match unit {
                TuiRenderUnit::TuiToolCard(card) => self.tool_ids.contains(&card.tool_id),
                _ => false,
            })
    }
}

/// 展示账本区间与 `committed` 不一致；拒绝猜测性删除。
#[derive(Clone, Debug, PartialEq, Eq)]
struct DisplayRangeError {
    ranges: Vec<(usize, usize)>,
    committed_len: usize,
}

/// 先快照判定，再统一应用：身份、展示区间与 `committed` 在同一事务内协调。
fn prepare_open_display(
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
    let live_empty = live_region_is_empty(state);
    // 迟到 open：请求发出后本地展示代次已经推进（新气泡或整轮回答已进入
    // committed）。此时不能整体清空 `committed`，否则旧快照会覆盖新内容。
    let display_changed = context.display_generation != state.generation;

    let mut allow_display_merge = true;
    let mut insert_slot = None;
    let mut adopt_enabled = false;
    if snapshot.identity_changed {
        if snapshot.displayed_entry_count > 0 {
            // 按展示账本精确摘除旧窗口页正文；live 尾部（新 turn 已 flush 的
            // 内容）不在区间内，原样保留。
            insert_slot = Some(remove_displayed_ranges(
                &mut state.committed,
                &snapshot.displayed_slot_ranges,
            )?);
            // 摘除后仍有 live 尾部时，open 页中实际仍由 committed 持有的条目
            // 只登记身份，阻止后续翻页重复插入；是否持有由消息身份判定，不用
            // 位置水位推断（已被淘汰的历史条目也高于水位）。
            // adopt 与 live 区域是否为空解耦：`RetainedLiveEntries` 只认
            // committed 中实际持有的身份，仅存在于流式 turn 的条目不会被采纳，
            // 仍照常插入；反之，新一轮正在流式时 committed 里已完成的条目也必须
            // adopt，否则重开页会把它们重复插入。
            adopt_enabled = !state.committed.is_empty();
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
                match known_replay_prefix_end(state) {
                    Some(end) => {
                        state.committed = state.committed.iter().skip(end).cloned().collect();
                        insert_slot = Some(0);
                    }
                    None => allow_display_merge = false,
                }
            } else {
                // 初始 open 采用窗口视图：旧回放内容让位给窗口页，避免同一身份双写。
                // live/未持久区域不为空时绝不清空。
                state.committed.clear();
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

    Ok(OpenDisplayPlan {
        allow_display_merge,
        insert_slot,
        adopt_enabled,
    })
}

/// 从 `committed` 精确移除窗口展示区间；返回 open 页的插入槽位。
///
/// `ranges` 是 `DisplayLedger` 的 (start, len)，必须升序、互不重叠且在边界内。
/// 校验失败时不修改 `committed`（不猜测删除）。
fn remove_displayed_ranges(
    committed: &mut im::Vector<TuiRenderUnit>,
    ranges: &[(usize, usize)],
) -> Result<usize, DisplayRangeError> {
    validate_display_ranges(committed.len(), ranges)?;
    let insert_at = ranges.first().map_or(committed.len(), |(start, _)| *start);
    // 逆序保证前面的槽位不因后面的摘除而失效。
    for (start, len) in ranges.iter().rev() {
        let end = start.saturating_add(*len);
        let tail = committed.split_off(end);
        let removed = committed.split_off(*start);
        debug_assert_eq!(removed.len(), *len);
        committed.append(tail);
    }
    Ok(insert_at.min(committed.len()))
}

fn validate_display_ranges(
    committed_len: usize,
    ranges: &[(usize, usize)],
) -> Result<(), DisplayRangeError> {
    let mut previous_end = 0usize;
    for (index, (start, len)) in ranges.iter().enumerate() {
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
    Ok(())
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
    let inserted_before_existing = insert_slot < state.committed.len();
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
    if inserted > 0
        && (context.placement == WindowPagePlacement::BeforeFirst || inserted_before_existing)
    {
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
/// 旧回放在 `committed` 里的严格前缀末尾。只有「回放区间之后确实还有单元」时
/// 才返回边界；否则无法区分旧回放与后来追加的 live 内容，调用方必须保守处理。
fn known_replay_prefix_end(state: &BridgeState) -> Option<usize> {
    let slots = crate::kit::history_paging::with_spans(|spans| spans.slots());
    let end = slots
        .iter()
        .map(|(slot, len)| slot.saturating_add(*len))
        .max()?;
    (end > 0 && end < state.committed.len()).then_some(end)
}

/// 缺口在 `committed` 中的插入槽位：缺口后首个驻留条目的槽位；缺口在尾部时
/// 等于当前展示长度。
fn gap_insert_slot(state: &BridgeState, gap_id: crate::kit::history_window::GapId) -> usize {
    let layout = state.window.layout();
    let Some(position) = layout
        .iter()
        .position(|region| *region == crate::kit::history_window::RegionKey::Gap(gap_id))
    else {
        return state.committed.len();
    };
    for region in layout.iter().skip(position + 1) {
        let crate::kit::history_window::RegionKey::Resident(key) = region else {
            continue;
        };
        if let Some(slot) = segment_start_slot(state, *key) {
            return slot;
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

fn segment_start_slot(state: &BridgeState, key: SegmentKey) -> Option<usize> {
    state
        .window
        .segment_display_bounds(key)
        .map(|(start, _)| start)
}

fn segment_end_slot(state: &BridgeState, key: SegmentKey) -> Option<usize> {
    state.window.segment_display_bounds(key).map(|(_, end)| end)
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

#[cfg(test)]
#[path = "history_window_test.rs"]
mod tests;
