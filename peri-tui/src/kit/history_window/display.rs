//! 窗口展示身份账本：已进入 `committed` 的持久条目及其槽位区间。

use std::collections::{BTreeMap, HashSet};

use super::{HistoryEntryView, HistoryWindow, PersistentEntryKey};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayLedger {
    entries: HashSet<PersistentEntryKey>,
    /// 起始槽位 → (条目身份, 单元数)。用于释放时按身份移除正文。
    slots: BTreeMap<usize, (PersistentEntryKey, usize)>,
}

impl DisplayLedger {
    pub fn contains(&self, key: &PersistentEntryKey) -> bool {
        self.entries.contains(key)
    }

    pub fn insert(&mut self, key: PersistentEntryKey) -> bool {
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
        }
        ranges
    }

    pub fn remove_entries(&mut self, entries: &[HistoryEntryView]) {
        for entry in entries {
            self.entries.remove(&entry.key);
            self.slots.retain(|_, (key, _)| key != &entry.key);
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.slots.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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

    pub fn mark_entry_displayed(&mut self, key: PersistentEntryKey) -> bool {
        self.display.insert(key)
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
