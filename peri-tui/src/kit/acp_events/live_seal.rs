//! Attribute sealed live units to reloadable history segments.

use std::collections::HashMap;

use super::{BridgeState, TuiRenderUnit, live_tool_links};

/// 一次归档中共享同一持久条目身份的 live 单元组。
struct LiveSealGroup {
    key: crate::kit::history_window::PersistentEntryKey,
    /// open 页给出的持久位置；纯 live 归属时为 `None`，等提交/页回执提升。
    position: Option<u64>,
    slots: Vec<usize>,
    body_bytes: usize,
    reserved_bytes: usize,
    /// 组内工具卡的 `(tool_id, 既有 retained 字节)`，用于归属后回落未归属账目。
    tool_bytes: Vec<(String, usize)>,
}

impl BridgeState {
    /// live 展示单元登记为可淘汰的 LiveSeal 段。
    ///
    /// 工具卡没有 ACP live `messageId`：先按 [`live_tool_links`] 保存的 live 归属
    /// 找所属 assistant 条目；open 页按 `tool_id` 回填的持久条目同样可用。找不到
    /// 归属的单元不伪造身份，继续计入未归属预算。
    ///
    /// 同一消息可能先归档文本、后归档工具卡：身份已驻留在 LiveSeal 段时把新增
    /// 单元并入原段，不登记第二个条目身份（驻留索引要求条目身份唯一）。
    pub(super) fn register_live_seal_segments(&mut self, slots: impl Iterator<Item = usize>) {
        if !self.window.is_enabled() {
            return;
        }
        use crate::kit::history_window::{
            HistoryEntryView, PersistentEntryKey, SegmentDraft, SegmentOrigin, SegmentPlacement,
            WindowUnitMapping, publish_window_unit_mappings,
        };

        let window_id = self.window.instance_id();
        let generation = self.window.generation();
        let mut groups: Vec<LiveSealGroup> = Vec::new();
        let mut group_indices: HashMap<PersistentEntryKey, usize> = HashMap::new();
        let mut unattributed_bytes = 0usize;
        let mut unattributed_tool_delta = 0i64;
        let mut unattributed_seen = crate::kit::retained_bytes::SeenAllocations::default();
        for slot in slots {
            let unit = &self.committed[slot];
            // Child previews have their own bounded disposable cache. They are
            // not canonical rows and cannot borrow/return canonical admission credit.
            if matches!(unit, TuiRenderUnit::TuiSubAgentGroup(_)) {
                continue;
            }
            // Local notes and interactions have an independent disposable budget.
            if crate::kit::display_retention::is_disposable(unit) {
                continue;
            }
            let unit_bytes =
                crate::kit::retained_bytes::retained_unit_bytes(unit, &mut unattributed_seen);
            let Some((key, position)) = self.live_unit_owner(unit) else {
                // 子 Agent 组、无持久身份的正文：不能建可淘汰段，但正文确实驻留，
                // 必须计入同一预算，避免账目低估。
                if let TuiRenderUnit::TuiToolCard(card) = unit {
                    unattributed_tool_delta += live_tool_links::note_unattributed(
                        &self.active_session_id,
                        window_id,
                        generation,
                        &card.tool_id,
                        unit_bytes,
                    );
                } else {
                    unattributed_bytes += unit_bytes;
                }
                continue;
            };
            // 工具卡正文沿用 `retained_unit_bytes` 的既有口径（containers +
            // summaries + presentation 之外的既有范围）。归属后这部分从
            // unattributed 移入可淘汰段，总账不缩小。
            let body_bytes = match unit {
                TuiRenderUnit::TuiUserBubble(bubble) => bubble.text.len(),
                TuiRenderUnit::TuiAssistantBubble(bubble) => {
                    bubble.text.len() + bubble.reasoning.as_ref().map_or(0, |r| r.text.len())
                }
                TuiRenderUnit::TuiToolCard(_) => unit_bytes,
                _ => 0,
            };
            let index = *group_indices.entry(key.clone()).or_insert_with(|| {
                groups.push(LiveSealGroup {
                    key,
                    position,
                    slots: Vec::new(),
                    body_bytes: 0,
                    reserved_bytes: 0,
                    tool_bytes: Vec::new(),
                });
                groups.len() - 1
            });
            let group = &mut groups[index];
            if position.is_some() {
                group.position = position;
            }
            group.slots.push(slot);
            group.body_bytes += body_bytes;
            group.reserved_bytes += unit_bytes;
            if let TuiRenderUnit::TuiToolCard(card) = unit {
                group.tool_bytes.push((card.tool_id.clone(), unit_bytes));
            }
        }
        if unattributed_bytes > 0 {
            self.window.note_unattributed_live_bytes(unattributed_bytes);
        }
        if unattributed_tool_delta > 0 {
            self.window
                .note_unattributed_live_bytes(unattributed_tool_delta as usize);
        } else if unattributed_tool_delta < 0 {
            self.window
                .adjust_unattributed_live_bytes(unattributed_tool_delta);
        }

        // 回填槽位可能早于既有展示区间：按 `committed` 顺序处理，段的布局位置由
        // 第一个更晚的展示区间决定，不能一律追加到尾部（否则 index 槽位顺序与
        // `committed` 不一致，释放会删错区间）。
        groups.sort_by_key(|group| group.slots.first().copied().unwrap_or(usize::MAX));
        let mut mappings = Vec::new();
        for group in groups {
            // 展示账目冲突意味着回填证据不可信：宁可不建段（继续按未归属记账），
            // 也不能让段的单位数与真实槽位错位。必须在并入既有段之前判定。
            if !group
                .slots
                .iter()
                .all(|slot| self.window.display_slot_available(*slot))
            {
                tracing::warn!(
                    key = ?group.key,
                    slots = ?group.slots,
                    "history_window: live seal display slots already covered"
                );
                self.note_group_unattributed(&group, window_id, generation);
                continue;
            }
            let resident = self.window.index().contains_entry(&group.key);
            let attached = resident
                && self.window.extend_live_entry(
                    &group.key,
                    group.slots.len(),
                    group.body_bytes,
                    group.reserved_bytes,
                );
            if resident && !attached {
                // 同一身份已驻留在页段（非 LiveSeal）：不能并入，正文仍是真实占用。
                self.note_group_unattributed(&group, window_id, generation);
                continue;
            }
            if !resident {
                let entry = HistoryEntryView::new(
                    group.key.clone(),
                    group.position,
                    group.slots.len(),
                    group.body_bytes,
                    group.reserved_bytes,
                    Vec::new(),
                );
                let draft = SegmentDraft::new(
                    group.key.source.clone(),
                    vec![entry],
                    group.body_bytes,
                    group.reserved_bytes,
                )
                .durable(false)
                .origin(SegmentOrigin::LiveSeal);
                let placement = self
                    .window
                    .segment_after_display_slot(group.slots[0])
                    .map_or(SegmentPlacement::Tail, SegmentPlacement::BeforeSegment);
                if let Err(error) = self.window.register_segment(draft, placement) {
                    tracing::error!(?error, "history_window: live seal registration failed");
                    self.note_group_unattributed(&group, window_id, generation);
                    continue;
                }
            }
            // 归属成功：把先前计入 unattributed 的工具卡字节移出，避免只增不减。
            let mut returned = 0usize;
            for (tool_id, _) in &group.tool_bytes {
                returned += live_tool_links::take_unattributed(
                    &self.active_session_id,
                    window_id,
                    generation,
                    tool_id,
                );
            }
            if returned > 0 {
                self.window.release_unattributed_live_bytes(returned);
            }
            // 单元已在 `committed` 中：只补展示账目，不得右移其他区间。
            for slot in &group.slots {
                if !self
                    .window
                    .mark_existing_display_slot(group.key.clone(), *slot, 1)
                {
                    tracing::error!(
                        slot,
                        key = ?group.key,
                        "history_window: live seal display slot registration lost race"
                    );
                    continue;
                }
                mappings.push(WindowUnitMapping {
                    token: crate::kit::message_area::window_identity_token(&self.committed[*slot]),
                    entry: group.key.clone(),
                });
            }
        }
        publish_window_unit_mappings(mappings);
    }

    /// live 单元到持久条目的定位。
    pub(super) fn live_unit_owner(
        &self,
        unit: &TuiRenderUnit,
    ) -> Option<(crate::kit::history_window::PersistentEntryKey, Option<u64>)> {
        use crate::kit::history_window::PersistentEntryKey;
        match unit {
            TuiRenderUnit::TuiUserBubble(bubble) => bubble.message_id.as_ref().map(|id| {
                (
                    PersistentEntryKey::own(format!("{}:{id}", self.active_session_id)),
                    None,
                )
            }),
            TuiRenderUnit::TuiAssistantBubble(bubble) => bubble.message_id.as_ref().map(|id| {
                (
                    PersistentEntryKey::own(format!("{}:{id}", self.active_session_id)),
                    None,
                )
            }),
            TuiRenderUnit::TuiToolCard(card) => {
                live_tool_links::owner_for_tool(&self.active_session_id, &card.tool_id)
                    .map(|owner| (owner.entry, owner.position))
            }
            _ => None,
        }
    }

    /// 无法归属的一组单元按未归属正文计入预算。
    fn note_group_unattributed(&mut self, group: &LiveSealGroup, window_id: u64, generation: u64) {
        let mut tool_total = 0usize;
        let mut tool_delta = 0i64;
        for (tool_id, bytes) in &group.tool_bytes {
            tool_total = tool_total.saturating_add(*bytes);
            tool_delta += live_tool_links::note_unattributed(
                &self.active_session_id,
                window_id,
                generation,
                tool_id,
                *bytes,
            );
        }
        let non_tool = group.reserved_bytes.saturating_sub(tool_total);
        if non_tool > 0 {
            self.window.note_unattributed_live_bytes(non_tool);
        }
        if tool_delta > 0 {
            self.window
                .note_unattributed_live_bytes(tool_delta as usize);
        } else if tool_delta < 0 {
            self.window.adjust_unattributed_live_bytes(tool_delta);
        }
    }
}
