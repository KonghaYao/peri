//! 驻留索引：条目/子项身份、段槽位前缀和与共享分配计量。

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use super::model::{
    PersistentEntryKey, PersistentSubItemKey, ResidentSegment, SegmentKey, WindowModelError,
};

/// 条目定位：段 + 段内序 + 当前槽位前缀。槽位只在同一窗口布局内有效。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryLocator {
    pub segment: SegmentKey,
    pub ordinal: usize,
    pub slot_start: usize,
    pub unit_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SegmentSlot {
    start: usize,
    unit_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SegmentMeta {
    body_bytes: usize,
    pinned: bool,
}

/// 共享 `Arc` 分配去重账本。同一个指针只计一次；`forget` 只在最后一份受管理引用
/// 消失后由释放事务调用。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AllocationAccount {
    seen: HashMap<usize, usize>,
    bytes: usize,
    deduplicated: usize,
}

impl AllocationAccount {
    pub fn account_arc<T>(&mut self, arc: &Arc<T>, bytes: usize) -> usize {
        self.account_ptr(Arc::as_ptr(arc) as usize, bytes)
    }

    /// 返回本次实际新计入的字节数；重复指针返回 0。
    pub fn account_ptr(&mut self, ptr: usize, bytes: usize) -> usize {
        if self.seen.contains_key(&ptr) {
            self.deduplicated = self.deduplicated.saturating_add(1);
            return 0;
        }
        self.seen.insert(ptr, bytes);
        self.bytes = self.bytes.saturating_add(bytes);
        bytes
    }

    pub fn forget(&mut self, ptr: usize) -> bool {
        let Some(bytes) = self.seen.remove(&ptr) else {
            return false;
        };
        self.bytes = self.bytes.saturating_sub(bytes);
        true
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn deduplicated_calls(&self) -> usize {
        self.deduplicated
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    pub fn clear(&mut self) {
        self.seen.clear();
        self.bytes = 0;
        self.deduplicated = 0;
    }
}

/// 驻留索引。去重表只覆盖**当前驻留段**；淘汰后同一身份可再次登记（A25）。
#[derive(Clone, Debug)]
pub struct ResidentIndex {
    entries: HashMap<PersistentEntryKey, EntryLocator>,
    sub_items: HashSet<PersistentSubItemKey>,
    by_segment: HashMap<SegmentKey, Vec<PersistentEntryKey>>,
    segment_meta: HashMap<SegmentKey, SegmentMeta>,
    slots: HashMap<SegmentKey, SegmentSlot>,
    segment_order: Vec<SegmentKey>,
    total_units: usize,
    body_bytes: usize,
    pinned_bytes: usize,
    allocations: AllocationAccount,
}

impl ResidentIndex {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            sub_items: HashSet::new(),
            by_segment: HashMap::new(),
            segment_meta: HashMap::new(),
            slots: HashMap::new(),
            segment_order: Vec::new(),
            total_units: 0,
            body_bytes: 0,
            pinned_bytes: 0,
            allocations: AllocationAccount::default(),
        }
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn sub_item_count(&self) -> usize {
        self.sub_items.len()
    }

    pub fn body_bytes(&self) -> usize {
        self.body_bytes
    }

    pub fn pinned_bytes(&self) -> usize {
        self.pinned_bytes
    }

    pub fn total_units(&self) -> usize {
        self.total_units
    }

    pub fn contains_entry(&self, entry: &PersistentEntryKey) -> bool {
        self.entries.contains_key(entry)
    }

    pub fn contains_sub_item(&self, sub_item: &PersistentSubItemKey) -> bool {
        self.sub_items.contains(sub_item)
    }

    pub fn locate_entry(&self, entry: &PersistentEntryKey) -> Option<&EntryLocator> {
        self.entries.get(entry)
    }

    pub fn entries_in_slot_order(&self) -> Vec<(PersistentEntryKey, usize, usize)> {
        let mut entries: Vec<(PersistentEntryKey, usize, usize)> = self
            .entries
            .iter()
            .map(|(key, locator)| (key.clone(), locator.slot_start, locator.unit_count))
            .collect();
        entries.sort_by_key(|(_, slot, _)| *slot);
        entries
    }

    pub fn segment_entries(&self, key: SegmentKey) -> Option<&[PersistentEntryKey]> {
        self.by_segment.get(&key).map(Vec::as_slice)
    }

    pub fn slot_range(&self, key: SegmentKey) -> Option<Range<usize>> {
        self.slots
            .get(&key)
            .map(|slot| slot.start..slot.start + slot.unit_count)
    }

    pub fn allocations(&self) -> &AllocationAccount {
        &self.allocations
    }

    pub fn allocations_mut(&mut self) -> &mut AllocationAccount {
        &mut self.allocations
    }

    pub fn account_shared_arc<T>(&mut self, arc: &Arc<T>, bytes: usize) -> usize {
        self.allocations.account_arc(arc, bytes)
    }

    /// 预检：驻留身份重复必须拒绝整页，不能部分登记。
    pub fn can_register(&self, segment: &ResidentSegment) -> Result<(), WindowModelError> {
        let mut entries = HashSet::new();
        let mut sub_items = HashSet::new();
        for entry in &segment.entries {
            if self.entries.contains_key(&entry.key) || !entries.insert(entry.key.clone()) {
                return Err(WindowModelError::DuplicateEntry(entry.key.clone()));
            }
            for sub_item in &entry.sub_items {
                if self.sub_items.contains(sub_item) || !sub_items.insert(sub_item.clone()) {
                    return Err(WindowModelError::DuplicateSubItem(sub_item.clone()));
                }
            }
        }
        Ok(())
    }

    pub fn register_segment(&mut self, segment: &ResidentSegment) {
        self.body_bytes = self.body_bytes.saturating_add(segment.body_bytes);
        if segment.pins.is_pinned() {
            self.pinned_bytes = self.pinned_bytes.saturating_add(segment.body_bytes);
        }
        self.segment_meta.insert(
            segment.key,
            SegmentMeta {
                body_bytes: segment.body_bytes,
                pinned: segment.pins.is_pinned(),
            },
        );
        let mut keys = Vec::with_capacity(segment.entries.len());
        for (ordinal, entry) in segment.entries.iter().enumerate() {
            self.entries.insert(
                entry.key.clone(),
                EntryLocator {
                    segment: segment.key,
                    ordinal,
                    slot_start: 0,
                    unit_count: entry.unit_count,
                },
            );
            for sub_item in &entry.sub_items {
                self.sub_items.insert(sub_item.clone());
            }
            keys.push(entry.key.clone());
        }
        self.by_segment.insert(segment.key, keys);
    }

    pub fn remove_segment(&mut self, key: SegmentKey) -> usize {
        let removed = self.by_segment.remove(&key).unwrap_or_default();
        for entry in &removed {
            self.entries.remove(entry);
            // 子项集合无法反查条目 → 只能按前缀重建：条目已删，枚举当前子项即可。
        }
        self.sub_items
            .retain(|sub_item| self.entries.contains_key(&sub_item.entry));
        if let Some(meta) = self.segment_meta.remove(&key) {
            self.body_bytes = self.body_bytes.saturating_sub(meta.body_bytes);
            if meta.pinned {
                self.pinned_bytes = self.pinned_bytes.saturating_sub(meta.body_bytes);
            }
        }
        self.slots.remove(&key);
        self.segment_order.retain(|segment| *segment != key);
        self.recompute_totals();
        removed.len()
    }

    pub fn set_segment_pinned(&mut self, key: SegmentKey, pinned: bool) {
        let Some(meta) = self.segment_meta.get_mut(&key) else {
            return;
        };
        if meta.pinned == pinned {
            return;
        }
        meta.pinned = pinned;
        if pinned {
            self.pinned_bytes = self.pinned_bytes.saturating_add(meta.body_bytes);
        } else {
            self.pinned_bytes = self.pinned_bytes.saturating_sub(meta.body_bytes);
        }
    }

    /// 按布局顺序重建槽位前缀和；段顺序必须与 `committed` 一致。
    pub fn rebuild_slots(
        &mut self,
        order: &[SegmentKey],
        segments: &HashMap<SegmentKey, ResidentSegment>,
    ) {
        self.slots.clear();
        self.segment_order.clear();
        let mut cursor = 0usize;
        for key in order {
            let Some(segment) = segments.get(key) else {
                continue;
            };
            self.segment_order.push(*key);
            self.slots.insert(
                *key,
                SegmentSlot {
                    start: cursor,
                    unit_count: segment.unit_count,
                },
            );
            for (ordinal, entry) in segment.entries.iter().enumerate() {
                if let Some(locator) = self.entries.get_mut(&entry.key) {
                    locator.segment = *key;
                    locator.ordinal = ordinal;
                    locator.slot_start = cursor;
                    locator.unit_count = entry.unit_count;
                }
                cursor = cursor.saturating_add(entry.unit_count);
            }
        }
        self.total_units = cursor;
    }

    fn recompute_totals(&mut self) {
        self.total_units = self
            .segment_order
            .iter()
            .filter_map(|key| self.slots.get(key))
            .map(|slot| slot.unit_count)
            .sum();
    }
}

impl Default for ResidentIndex {
    fn default() -> Self {
        Self::new()
    }
}
