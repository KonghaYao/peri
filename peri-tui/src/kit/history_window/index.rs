//! 驻留索引：条目/子项身份、段槽位前缀和、跨段工具配对与共享分配计量。

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use super::HistorySource;
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

/// 工具结果引用：身份 + 内容修订，不含正文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultRef {
    pub source: HistorySource,
    pub entry_id: String,
    pub content_revision: String,
}

impl ResultRef {
    pub fn new(
        source: HistorySource,
        entry_id: impl Into<String>,
        content_revision: impl Into<String>,
    ) -> Self {
        Self {
            source,
            entry_id: entry_id.into(),
            content_revision: content_revision.into(),
        }
    }

    fn entry_key(&self) -> PersistentEntryKey {
        PersistentEntryKey::new(self.source.clone(), self.entry_id.clone())
    }
}

/// 工具终态元数据：错误位、输出总长度与**完整输出**。
///
/// `summary()` 只用于诊断/单行展示，绝不能回填到卡片冒充完整结果。跨页暂存与
/// 配对恢复都以这里保存的正文为准。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolTerminalMeta {
    pub is_error: bool,
    pub output_bytes: usize,
    output: Arc<str>,
}

impl ToolTerminalMeta {
    pub const MAX_SUMMARY_BYTES: usize = 1024;

    pub fn new(is_error: bool, output_bytes: usize, output: impl Into<String>) -> Self {
        let output: String = output.into();
        Self {
            is_error,
            output_bytes,
            output: Arc::from(output),
        }
    }

    /// 完整输出正文。
    pub fn output(&self) -> &str {
        &self.output
    }

    /// 单行诊断摘要（最多 1 KiB）；不是可回填的完整结果。
    pub fn summary(&self) -> &str {
        let max = Self::MAX_SUMMARY_BYTES.min(self.output.len());
        let mut end = max;
        while end > 0 && !self.output.is_char_boundary(end) {
            end -= 1;
        }
        &self.output[..end]
    }

    /// 元数据的估算保留字节（完整正文 + 结构体固定开销）。
    pub fn retained_bytes(&self) -> usize {
        self.output.len() + std::mem::size_of::<Self>()
    }
}

/// 工具配对键：只按来源 + `toolCallId`，不把调用条目放进键（跨页配对必须成立）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ToolCallKey {
    pub source: HistorySource,
    pub tool_call_id: String,
}

impl ToolCallKey {
    pub fn new(source: HistorySource, tool_call_id: impl Into<String>) -> Self {
        Self {
            source,
            tool_call_id: tool_call_id.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolCallState {
    Started {
        entry: PersistentEntryKey,
        sub_item: u32,
    },
    ResultOnly {
        result_ref: ResultRef,
        terminal: ToolTerminalMeta,
    },
    Terminal {
        call_ref: PersistentEntryKey,
        result_ref: Option<ResultRef>,
        meta: ToolTerminalMeta,
    },
}

impl ToolCallState {
    fn pending_meta(&self) -> Option<&ToolTerminalMeta> {
        match self {
            Self::ResultOnly { terminal, .. } => Some(terminal),
            Self::Started { .. } | Self::Terminal { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolPairOutcome {
    CallOnly,
    ResultOnly,
    Paired,
    TerminalUpdated,
    DuplicateCall,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ToolPairRejected {
    #[error("unmatched tool result count limit reached")]
    TooManyPending,
    #[error("unmatched tool result byte budget would be exceeded")]
    PendingBudgetExceeded,
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
    tool_calls: HashMap<ToolCallKey, ToolCallState>,
    by_segment: HashMap<SegmentKey, Vec<PersistentEntryKey>>,
    segment_meta: HashMap<SegmentKey, SegmentMeta>,
    slots: HashMap<SegmentKey, SegmentSlot>,
    segment_order: Vec<SegmentKey>,
    total_units: usize,
    body_bytes: usize,
    pinned_bytes: usize,
    pending_tool_bytes: usize,
    allocations: AllocationAccount,
    max_pending_tool_bytes: usize,
    max_pending_tool_results: usize,
}

impl ResidentIndex {
    pub fn new(max_pending_tool_bytes: usize, max_pending_tool_results: usize) -> Self {
        Self {
            entries: HashMap::new(),
            sub_items: HashSet::new(),
            tool_calls: HashMap::new(),
            by_segment: HashMap::new(),
            segment_meta: HashMap::new(),
            slots: HashMap::new(),
            segment_order: Vec::new(),
            total_units: 0,
            body_bytes: 0,
            pinned_bytes: 0,
            pending_tool_bytes: 0,
            allocations: AllocationAccount::default(),
            max_pending_tool_bytes,
            max_pending_tool_results,
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

    pub fn pending_tool_bytes(&self) -> usize {
        self.pending_tool_bytes
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

    pub fn tool_call(&self, key: &ToolCallKey) -> Option<&ToolCallState> {
        self.tool_calls.get(key)
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
        self.tool_calls.retain(|_, state| match state {
            ToolCallState::Started { entry, .. } => self.entries.contains_key(entry),
            ToolCallState::ResultOnly { result_ref, .. } => {
                self.entries.contains_key(&result_ref.entry_key())
            }
            ToolCallState::Terminal {
                call_ref,
                result_ref,
                ..
            } => {
                self.entries.contains_key(call_ref)
                    || result_ref
                        .as_ref()
                        .is_some_and(|result| self.entries.contains_key(&result.entry_key()))
            }
        });
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
        self.pending_tool_bytes = self
            .tool_calls
            .values()
            .filter_map(ToolCallState::pending_meta)
            .map(ToolTerminalMeta::retained_bytes)
            .sum();
    }

    fn recompute_pending_tool_bytes(&mut self) {
        self.pending_tool_bytes = self
            .tool_calls
            .values()
            .filter_map(ToolCallState::pending_meta)
            .map(ToolTerminalMeta::retained_bytes)
            .sum();
    }

    fn pending_result_count(&self) -> usize {
        self.tool_calls
            .values()
            .filter(|state| matches!(state, ToolCallState::ResultOnly { .. }))
            .count()
    }

    pub fn record_tool_started(
        &mut self,
        source: HistorySource,
        tool_call_id: impl Into<String>,
        entry: PersistentEntryKey,
        sub_item: u32,
    ) -> ToolPairOutcome {
        let key = ToolCallKey::new(source, tool_call_id);
        match self.tool_calls.remove(&key) {
            Some(ToolCallState::ResultOnly {
                result_ref,
                terminal,
            }) => {
                self.tool_calls.insert(
                    key,
                    ToolCallState::Terminal {
                        call_ref: entry,
                        result_ref: Some(result_ref),
                        meta: terminal,
                    },
                );
                self.recompute_pending_tool_bytes();
                ToolPairOutcome::Paired
            }
            Some(previous @ ToolCallState::Started { .. }) => {
                // 同一调用重复开始：保留第一次身份，不制造第二张卡。
                self.tool_calls.insert(key, previous);
                ToolPairOutcome::DuplicateCall
            }
            Some(existing @ ToolCallState::Terminal { .. }) => {
                self.tool_calls.insert(key, existing);
                ToolPairOutcome::DuplicateCall
            }
            None => {
                self.tool_calls
                    .insert(key, ToolCallState::Started { entry, sub_item });
                ToolPairOutcome::CallOnly
            }
        }
    }

    pub fn record_tool_result(
        &mut self,
        source: HistorySource,
        tool_call_id: impl Into<String>,
        result_ref: ResultRef,
        terminal: ToolTerminalMeta,
    ) -> Result<ToolPairOutcome, ToolPairRejected> {
        let key = ToolCallKey::new(source, tool_call_id);
        match self.tool_calls.remove(&key) {
            Some(ToolCallState::Started { entry, .. }) => {
                self.tool_calls.insert(
                    key,
                    ToolCallState::Terminal {
                        call_ref: entry,
                        result_ref: Some(result_ref),
                        meta: terminal,
                    },
                );
                self.recompute_pending_tool_bytes();
                Ok(ToolPairOutcome::Paired)
            }
            Some(ToolCallState::Terminal {
                call_ref,
                result_ref: previous,
                ..
            }) => {
                self.tool_calls.insert(
                    key,
                    ToolCallState::Terminal {
                        call_ref,
                        result_ref: Some(result_ref),
                        meta: terminal,
                    },
                );
                let _ = previous;
                Ok(ToolPairOutcome::TerminalUpdated)
            }
            Some(previous @ ToolCallState::ResultOnly { .. }) => {
                let additional = terminal.retained_bytes();
                let previous_bytes = previous
                    .pending_meta()
                    .map(ToolTerminalMeta::retained_bytes)
                    .unwrap_or(0);
                if self
                    .pending_tool_bytes
                    .saturating_sub(previous_bytes)
                    .saturating_add(additional)
                    > self.max_pending_tool_bytes
                {
                    self.tool_calls.insert(key, previous);
                    return Err(ToolPairRejected::PendingBudgetExceeded);
                }
                self.tool_calls.insert(
                    key,
                    ToolCallState::ResultOnly {
                        result_ref,
                        terminal,
                    },
                );
                self.recompute_pending_tool_bytes();
                Ok(ToolPairOutcome::ResultOnly)
            }
            None => {
                if self.pending_result_count() >= self.max_pending_tool_results {
                    return Err(ToolPairRejected::TooManyPending);
                }
                if self
                    .pending_tool_bytes
                    .saturating_add(terminal.retained_bytes())
                    > self.max_pending_tool_bytes
                {
                    return Err(ToolPairRejected::PendingBudgetExceeded);
                }
                self.tool_calls.insert(
                    key,
                    ToolCallState::ResultOnly {
                        result_ref,
                        terminal,
                    },
                );
                self.recompute_pending_tool_bytes();
                Ok(ToolPairOutcome::ResultOnly)
            }
        }
    }
}

impl Default for ResidentIndex {
    fn default() -> Self {
        Self::new(
            super::budget::INITIAL_PENDING_TOOL_BYTES,
            super::budget::INITIAL_MAX_PENDING_TOOL_RESULTS,
        )
    }
}
