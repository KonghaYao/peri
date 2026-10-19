//! 窗口展示身份账本：已进入 `committed` 的持久条目及其槽位区间。

use std::collections::{BTreeMap, HashMap, HashSet};

use super::{HistoryEntryView, HistoryWindow, PersistentEntryKey};

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
        let shifted: BTreeMap<usize, (PersistentEntryKey, usize)> = self
            .slots
            .iter()
            .map(|(start, value)| {
                let start = if *start >= at {
                    start.saturating_add(total)
                } else {
                    *start
                };
                (start, value.clone())
            })
            .collect();
        self.slots = shifted;
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

    pub fn range_for(&self, key: &PersistentEntryKey) -> Option<(usize, usize)> {
        self.slots
            .iter()
            .find(|(_, (entry, _))| entry == key)
            .map(|(start, (_, len))| (*start, *len))
    }

    /// 移除一批身份，返回其槽位区间（升序）。后续区间同步左移。
    pub fn remove_entries_at(&mut self, keys: &HashSet<PersistentEntryKey>) -> Vec<(usize, usize)> {
        let ranges: Vec<(usize, usize)> = self
            .slots
            .iter()
            .filter(|(_, (key, _))| keys.contains(key))
            .map(|(start, (_, len))| (*start, *len))
            .collect();
        for (start, len) in ranges.iter().rev() {
            self.slots.remove(start);
            let shifted: BTreeMap<usize, (PersistentEntryKey, usize)> = self
                .slots
                .iter()
                .map(|(other, value)| {
                    let other = if *other > *start {
                        other.saturating_sub(*len)
                    } else {
                        *other
                    };
                    (other, value.clone())
                })
                .collect();
            self.slots = shifted;
        }
        for key in keys {
            self.entries.remove(key);
            self.positions.remove(key);
        }
        ranges
    }

    pub fn remove_entries(&mut self, entries: &[HistoryEntryView]) {
        for entry in entries {
            self.entries.remove(&entry.key);
            self.positions.remove(&entry.key);
            self.slots.retain(|_, (key, _)| key != &entry.key);
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.slots.clear();
        self.positions.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

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
    pub fn is_entry_displayed(&self, key: &PersistentEntryKey) -> bool {
        self.display.contains(key)
    }

    pub fn mark_entry_displayed(&mut self, key: PersistentEntryKey, position: Option<u64>) -> bool {
        self.display.insert(key, position)
    }

    pub fn mark_display_block(&mut self, entries: &[(PersistentEntryKey, usize)], at: usize) {
        self.display.insert_block(entries, at);
    }

    pub fn displayed_range(&self, key: &PersistentEntryKey) -> Option<(usize, usize)> {
        self.display.range_for(key)
    }

    pub fn clear_displayed_entries(&mut self) {
        self.display.clear();
    }

    pub fn displayed_entry_count(&self) -> usize {
        self.display.len()
    }
}
