//! 窗口展示身份账本：已进入 `committed` 的持久条目及其槽位区间。

use std::collections::{BTreeMap, HashMap, HashSet};

use super::{HistoryEntryView, HistoryWindow, PersistentEntryKey, SegmentKey};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayLedger {
    /// 已登记展示身份的条目。可能没有 slot——正文由 live 尾部持有，
    /// 只用于阻止后续翻页重复插入，不参与释放。
    entries: HashSet<PersistentEntryKey>,
    /// 起始槽位 → (条目身份, 单元数)。用于释放时按身份移除正文。
    slots: BTreeMap<usize, (PersistentEntryKey, usize)>,
    /// 无 slot 条目的持久位置。adopt 条目据此在身份切换后延续水位，
    /// 避免下一次 open 把它误判为未展示内容。
    positions: HashMap<PersistentEntryKey, u64>,
}

impl DisplayLedger {
    /// Remove a disposable unit that has no canonical entry, preserving every
    /// later canonical range. Refuse overlap rather than shifting a saved entry.
    pub fn remove_unmapped_slot(&mut self, slot: usize) -> bool {
        if self.overlaps(slot, 1) {
            return false;
        }
        self.slots = std::mem::take(&mut self.slots)
            .into_iter()
            .map(|(start, value)| (if start > slot { start - 1 } else { start }, value))
            .collect();
        true
    }

    #[must_use]
    pub fn contains(&self, key: &PersistentEntryKey) -> bool {
        self.entries.contains(key)
    }

    /// 登记无 slot 的展示身份；`position` 是持久条目位置，继承条目没有位置。
    pub fn insert(&mut self, key: PersistentEntryKey, position: Option<u64>) -> bool {
        if let Some(position) = position {
            self.positions.insert(key.clone(), position);
        }
        self.entries.insert(key)
    }

    /// 一批条目已插入 `at`，总单元数 `total_len`；更新既有区间并登记新条目。
    pub fn insert_block(&mut self, entries: &[(PersistentEntryKey, usize)], at: usize) {
        if entries.is_empty() {
            return;
        }
        let total: usize = entries.iter().map(|(_, len)| *len).sum();
        if total > 0
            && self
                .slots
                .last_key_value()
                .is_some_and(|(last, _)| *last >= at)
        {
            let tail = self.slots.split_off(&at);
            self.slots.extend(
                tail.into_iter()
                    .map(|(start, range)| (start.saturating_add(total), range)),
            );
        }
        let mut cursor = at;
        for (key, len) in entries {
            self.entries.insert(key.clone());
            if *len > 0 {
                self.positions.remove(key);
                self.slots.insert(cursor, (key.clone(), *len));
            }
            cursor = cursor.saturating_add(*len);
        }
    }

    /// 登记 `committed` 中**已存在**的展示单元：槽位保持不变，不移动其他区间。
    ///
    /// 回填（open 页、迟到归属）只补记账，不重排 `committed`，因此不得使用会右移
    /// 既有区间的 [`Self::insert_block`]；否则位于回填槽位之后的条目会整体错位，
    /// 释放时按错误区间删除正文。槽位与其他条目重叠时不写入并返回 `false`。
    pub fn insert_existing_slot(
        &mut self,
        key: PersistentEntryKey,
        start: usize,
        len: usize,
    ) -> bool {
        if len == 0 {
            self.entries.insert(key);
            return true;
        }
        if let Some((existing, existing_len)) = self.slots.get(&start)
            && existing == &key
            && *existing_len == len
        {
            return true;
        }
        if self.overlaps(start, len) {
            return false;
        }
        self.positions.remove(&key);
        self.entries.insert(key.clone());
        self.slots.insert(start, (key, len));
        true
    }

    /// `[start, start + len)` 是否已被其他区间占用。
    pub fn overlaps(&self, start: usize, len: usize) -> bool {
        let end = start.saturating_add(len);
        // Registered ranges never overlap; only the last start before `end`
        // can overlap. Display expiry probes this for every disposable record.
        self.slots
            .range(..end)
            .next_back()
            .is_some_and(|(other_start, (_, other_len))| {
                start < other_start.saturating_add(*other_len)
            })
    }

    pub fn range_for(&self, key: &PersistentEntryKey) -> Option<(usize, usize)> {
        self.slots
            .iter()
            .find(|(_, (entry, _))| entry == key)
            .map(|(start, (_, len))| (*start, *len))
    }

    /// 移除一批身份，返回其槽位区间（升序）。后续区间同步左移。
    pub fn remove_entries_at(&mut self, keys: &HashSet<PersistentEntryKey>) -> Vec<(usize, usize)> {
        let mut ranges = Vec::new();
        if keys.is_empty() {
            return ranges;
        }
        if let Some((&first, _)) = self.slots.iter().find(|(_, (key, _))| keys.contains(key)) {
            // Keep the prefix in place. Each survivor in the ordered tail shifts
            // by the sum of the removed lengths before its original start.
            let mut removed = 0usize;
            let tail = self.slots.split_off(&first);
            self.slots
                .extend(tail.into_iter().filter_map(|(start, (key, len))| {
                    if keys.contains(&key) {
                        ranges.push((start, len));
                        removed = removed.saturating_add(len);
                        None
                    } else {
                        Some((start.saturating_sub(removed), (key, len)))
                    }
                }));
        }
        for key in keys {
            self.entries.remove(key);
            self.positions.remove(key);
        }
        ranges
    }

    pub fn remove_entries(&mut self, entries: &[HistoryEntryView]) {
        if entries.is_empty() {
            return;
        }
        let removed: HashSet<_> = entries.iter().map(|entry| &entry.key).collect();
        for key in &removed {
            self.entries.remove(*key);
            self.positions.remove(*key);
        }
        self.slots.retain(|_, (key, _)| !removed.contains(key));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.slots.clear();
        self.positions.clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `committed` 中窗口展示单元的槽位区间（升序、互不重叠）。
    ///
    /// 只覆盖真正持有正文的条目；adopt 登记的身份没有 slot，不在此列。
    pub fn slot_ranges(&self) -> Vec<(usize, usize)> {
        self.slots
            .iter()
            .map(|(start, (_, len))| (*start, *len))
            .collect()
    }

    /// 不持有 slot 的展示身份及其持久位置（按位置升序）。
    ///
    /// 身份切换会清空账本；调用方须在 `begin_window` 后重新登记这些身份，
    /// 否则 live 尾部持有的条目会被下一次 open 重复插入。
    pub fn slotless_entries(&self) -> Vec<(PersistentEntryKey, Option<u64>)> {
        let slotted: HashSet<&PersistentEntryKey> =
            self.slots.values().map(|(key, _)| key).collect();
        let mut entries: Vec<(PersistentEntryKey, Option<u64>)> = self
            .entries
            .iter()
            .filter(|key| !slotted.contains(key))
            .map(|key| (key.clone(), self.positions.get(key).copied()))
            .collect();
        entries.sort_by(|(left_key, left_position), (right_key, right_position)| {
            left_position
                .cmp(right_position)
                .then_with(|| left_key.cmp(right_key))
        });
        entries
    }

    pub fn debug_ranges(&self) -> Vec<(usize, PersistentEntryKey, usize)> {
        self.slots
            .iter()
            .map(|(start, (key, len))| (*start, key.clone(), *len))
            .collect()
    }
}

impl HistoryWindow {
    pub(crate) fn remove_unmapped_display_slot(&mut self, slot: usize) -> bool {
        self.display.remove_unmapped_slot(slot)
    }

    #[must_use]
    pub fn is_entry_displayed(&self, key: &PersistentEntryKey) -> bool {
        self.display.contains(key)
    }

    pub fn mark_entry_displayed(&mut self, key: PersistentEntryKey, position: Option<u64>) -> bool {
        self.display.insert(key, position)
    }

    pub fn mark_display_block(&mut self, entries: &[(PersistentEntryKey, usize)], at: usize) {
        self.display.insert_block(entries, at);
    }

    /// 登记 `committed` 中已存在的单元：不移动任何既有展示区间。
    ///
    /// 与 [`Self::mark_display_block`] 的区别是**不重排**：调用方保证槽位已经真实
    /// 存在于 `committed`，只缺展示账目（open 页回填、迟到归属）。
    pub fn mark_existing_display_slot(
        &mut self,
        key: PersistentEntryKey,
        slot: usize,
        len: usize,
    ) -> bool {
        self.display.insert_existing_slot(key, slot, len)
    }

    /// 槽位是否尚未被展示账目占用（回填前预检，避免注册段后才发现冲突）。
    pub fn display_slot_available(&self, slot: usize) -> bool {
        !self.display.overlaps(slot, 1)
    }

    /// 第一个展示起点晚于 `slot` 的驻留段。
    ///
    /// 回填既有槽位时用于确定段的布局位置：段的顺序必须与 `committed` 一致
    /// （见 [`super::index::ResidentIndex::rebuild_slots`]），不能一律追加到尾部。
    pub fn segment_after_display_slot(&self, slot: usize) -> Option<SegmentKey> {
        self.display
            .slots
            .range((std::ops::Bound::Excluded(slot), std::ops::Bound::Unbounded))
            .next()
            .and_then(|(_, (key, _))| self.index.locate_entry(key).map(|locator| locator.segment))
    }

    pub(crate) fn resident_entry_at_or_after_slot(
        &self,
        slot: usize,
    ) -> Option<&PersistentEntryKey> {
        // A live message can own multiple separate ranges. Use its first display
        // start, then choose in resident layout order, as when placing a page.
        let mut starts = HashMap::new();
        for (start, (key, _)) in &self.display.slots {
            starts.entry(key).or_insert(*start);
        }
        self.index
            .entry_keys_in_slot_order()
            .find(|key| starts.get(key).is_some_and(|start| *start >= slot))
    }

    pub fn displayed_range(&self, key: &PersistentEntryKey) -> Option<(usize, usize)> {
        self.display.range_for(key)
    }

    pub fn clear_displayed_entries(&mut self) {
        self.display.clear();
    }

    #[must_use]
    pub fn displayed_entry_count(&self) -> usize {
        self.display.len()
    }
}
