use std::sync::{Arc, Weak};

use super::grid::GridSpec;
use super::selection::{SlotLines, SlotLookup, WrappedLineInfo};
use crate::kit::entry_render_cache::{EntryRenderCache, EntryRenderContext};
use crate::kit::tui_render_unit::TuiRenderUnit;
#[cfg(test)]
use ratatui_kit::ratatui::text::Line;

#[derive(Clone, Default)]
pub(super) struct SlotIndex {
    root: Option<Arc<Node>>,
    capacity: usize,
    // Physical leaf for logical slot zero. Prepending can attach a left subtree without
    // rewriting retained wrap geometry (including the geometry of evicted rows).
    first: usize,
    count: usize,
    source: Option<(im::Vector<TuiRenderUnit>, GridSpec, EntryRenderContext, u64)>,
}

#[derive(Clone)]
struct LayoutSlot {
    lines: Weak<SlotLines>,
    resident: Option<Arc<SlotLines>>,
    visual_prefix: Arc<Vec<usize>>,
    /// entry 首行在逻辑行中的下标——过程区转场台阶（` ╰─╮` / ` ╭─╯`）会占掉
    /// 第 0 行，此时可点击的 header 是第 1 行。点击命中只认这一行（正文行留给
    /// 文本选区）。
    header_local: u8,
    /// 展开的正文空档组：成员标题逻辑行 → 成员 FoldKey。与行生存期解耦
    /// （行被淘汰后命中表仍可判定）。
    fold_hits: Arc<Vec<(usize, crate::kit::tui_render_unit::FoldKey)>>,
}

/// A page worker sends only measured rows, never rendered text or a Markdown cache.
pub(super) struct ColdLayout {
    visual_prefix: Vec<usize>,
    header_local: u8,
}

impl ColdLayout {
    pub(super) fn measure(lines: &SlotLines, map: &[WrappedLineInfo]) -> Self {
        let mut visual_prefix = Vec::with_capacity(map.len() + 1);
        visual_prefix.push(0);
        visual_prefix.extend(map.iter().map(|entry| entry.visual_end));
        Self {
            visual_prefix,
            header_local: header_local_of(lines),
        }
    }

    pub(super) fn bytes(&self) -> usize {
        self.visual_prefix.capacity() * size_of::<usize>()
    }
    pub(super) fn visual_rows(&self) -> usize {
        self.visual_prefix.last().copied().unwrap_or(0)
    }
}

/// entry 的可点击 header 逻辑行下标：首行是过程区转场台阶时为 1，否则 0。
fn header_local_of(lines: &SlotLines) -> u8 {
    match lines.line(0) {
        Some(line) if crate::kit::message_area::render::is_step_row(line) => 1,
        _ => 0,
    }
}

struct Node {
    logical: usize,
    visual: usize,
    kind: NodeKind,
}

enum NodeKind {
    Leaf(LayoutSlot),
    Branch(Option<Arc<Node>>, Option<Arc<Node>>),
}

impl Node {
    fn branch(left: Option<Arc<Node>>, right: Option<Arc<Node>>) -> Option<Arc<Self>> {
        if left.is_none() && right.is_none() {
            return None;
        }
        Some(Arc::new(Self {
            logical: left
                .as_ref()
                .map_or(0, |node| node.logical)
                .saturating_add(right.as_ref().map_or(0, |node| node.logical)),
            visual: left
                .as_ref()
                .map_or(0, |node| node.visual)
                .saturating_add(right.as_ref().map_or(0, |node| node.visual)),
            kind: NodeKind::Branch(left, right),
        }))
    }

    fn set(root: Option<&Arc<Self>>, capacity: usize, slot: usize, leaf: Arc<Self>) -> Arc<Self> {
        if capacity == 1 {
            return leaf;
        }
        let (mut left, mut right) = match root.map(|node| &node.kind) {
            Some(NodeKind::Branch(left, right)) => (left.clone(), right.clone()),
            _ => (None, None),
        };
        let half = capacity / 2;
        if slot < half {
            left = Some(Self::set(left.as_ref(), half, slot, leaf));
        } else {
            right = Some(Self::set(right.as_ref(), half, slot - half, leaf));
        }
        Self::branch(left, right).expect("setting a leaf leaves at least one populated branch")
    }

    fn retain(
        root: Option<&Arc<Self>>,
        capacity: usize,
        start: usize,
        end: usize,
    ) -> Option<Arc<Self>> {
        if start >= end || start >= capacity {
            return None;
        }
        if start == 0 && end >= capacity {
            return root.cloned();
        }
        let node = root?;
        let NodeKind::Branch(left, right) = &node.kind else {
            return root.cloned();
        };
        let half = capacity / 2;
        Self::branch(
            Self::retain(left.as_ref(), half, start, end.min(half)),
            Self::retain(
                right.as_ref(),
                half,
                start.saturating_sub(half),
                end.saturating_sub(half),
            ),
        )
    }
}

impl SlotIndex {
    #[cfg(test)]
    #[must_use]
    pub(super) fn new<T: Into<SlotLines>>(
        slots: Vec<T>,
        maps: Vec<Arc<Vec<WrappedLineInfo>>>,
    ) -> Self {
        Self::new_with_overlays(slots.into_iter().map(Into::into).collect(), maps)
    }

    #[cfg(test)]
    pub(super) fn new_with_overlays(
        slots: Vec<SlotLines>,
        maps: Vec<Arc<Vec<WrappedLineInfo>>>,
    ) -> Self {
        assert_eq!(slots.len(), maps.len());
        let mut index = Self::default();
        {
            let iteration_items = slots.into_iter().zip(maps).enumerate();
            for (slot, (lines, map)) in iteration_items {
                index.set_slot(slot, Arc::new(lines), &map, true);
            }
        }
        index
    }

    pub(super) fn set_source(
        &mut self,
        items: im::Vector<TuiRenderUnit>,
        grid: GridSpec,
        context: EntryRenderContext,
        frame: u64,
    ) {
        self.source = Some((items, grid, context, frame));
    }

    pub(super) fn set_slot(
        &mut self,
        slot: usize,
        lines: Arc<SlotLines>,
        map: &[WrappedLineInfo],
        retain: bool,
    ) {
        let measured = ColdLayout::measure(&lines, map);
        let leaf = Arc::new(Node {
            logical: lines.len(),
            visual: measured.visual_rows(),
            kind: NodeKind::Leaf(LayoutSlot {
                header_local: measured.header_local,
                fold_hits: Arc::clone(lines.fold_hits()),
                lines: Arc::downgrade(&lines),
                resident: retain.then_some(lines),
                visual_prefix: Arc::new(measured.visual_prefix),
            }),
        });
        self.set_leaf(slot, leaf);
    }

    pub(super) fn set_cold(&mut self, slot: usize, measured: ColdLayout) {
        let leaf = Arc::new(Node {
            logical: measured.visual_prefix.len() - 1,
            visual: measured.visual_rows(),
            kind: NodeKind::Leaf(LayoutSlot {
                lines: Weak::new(),
                resident: None,
                fold_hits: Arc::default(),
                header_local: measured.header_local,
                visual_prefix: Arc::new(measured.visual_prefix),
            }),
        });
        self.set_leaf(slot, leaf);
    }

    fn set_leaf(&mut self, slot: usize, leaf: Arc<Node>) {
        if self.capacity == 0 {
            self.capacity = 1;
        }
        let physical = self.first + slot;
        while physical >= self.capacity {
            self.root = Node::branch(self.root.take(), None);
            self.capacity *= 2;
        }
        self.root = Some(Node::set(self.root.as_ref(), self.capacity, physical, leaf));
        self.count = self.count.max(slot + 1);
    }

    pub(super) fn truncate(&mut self, count: usize) {
        if count == 0 {
            self.root = None;
            self.capacity = 0;
            self.first = 0;
            self.count = 0;
        } else if count < self.count {
            self.root = Node::retain(
                self.root.as_ref(),
                self.capacity,
                self.first,
                self.first + count,
            );
            self.count = count;
        }
    }

    /// Keep an unchanged range, releasing both sides. Whole subtrees are shared with the
    /// previous snapshot. Holes before the range get their measurements from the new page.
    pub(super) fn move_history(&mut self, run: crate::kit::tui_render_unit::HistoryOverlap) {
        debug_assert!(run.len > 0 && run.old_start + run.len <= self.count);
        let mut start = self.first + run.old_start;
        self.root = Node::retain(self.root.as_ref(), self.capacity, start, start + run.len);
        // Drop empty outer levels when alternating page directions over a long-lived window.
        while self.capacity > 1 {
            let half = self.capacity / 2;
            let Some(NodeKind::Branch(left, right)) = self.root.as_ref().map(|node| &node.kind)
            else {
                break;
            };
            if start >= half {
                self.root = right.clone();
                start -= half;
            } else if start + run.len <= half {
                self.root = left.clone();
            } else {
                break;
            }
            self.capacity = half;
        }
        while start < run.new_start {
            self.root = Node::branch(None, self.root.take());
            start += self.capacity;
            self.capacity *= 2;
        }
        self.first = start - run.new_start;
        self.count = run.new_start + run.len;
    }

    pub(super) fn replace_lines(&mut self, slot: usize, lines: Arc<SlotLines>) {
        let Some((old, _, _)) = self.slot(slot) else {
            return;
        };
        let leaf = Arc::new(Node {
            logical: old.visual_prefix.len().saturating_sub(1),
            visual: old.visual_prefix.last().copied().unwrap_or(0),
            kind: NodeKind::Leaf(LayoutSlot {
                header_local: header_local_of(&lines),
                fold_hits: Arc::clone(lines.fold_hits()),
                lines: Arc::downgrade(&lines),
                resident: None,
                visual_prefix: Arc::clone(&old.visual_prefix),
            }),
        });
        self.root = Some(Node::set(
            self.root.as_ref(),
            self.capacity,
            self.first + slot,
            leaf,
        ));
    }

    /// entry 可点击 header 的逻辑行下标（过程区台阶行占第 0 行时为 1）。
    pub(super) fn header_local(&self, slot: usize) -> usize {
        self.slot(slot)
            .map_or(0, |(leaf, _, _)| usize::from(leaf.header_local))
    }

    /// slot 内该逻辑行是否是一张展开组的成员标题行——是则返回该成员的 FoldKey。
    pub(super) fn member_fold_hit(
        &self,
        slot: usize,
        local_logical: usize,
    ) -> Option<crate::kit::tui_render_unit::FoldKey> {
        let (leaf, _, _) = self.slot(slot)?;
        leaf.fold_hits
            .iter()
            .find(|(row, _)| *row == local_logical)
            .map(|(_, key)| key.clone())
    }

    fn slot(&self, slot: usize) -> Option<(&LayoutSlot, usize, usize)> {
        if slot >= self.count {
            return None;
        }
        let mut node = self.root.as_ref()?;
        let mut local = self.first + slot;
        let mut capacity = self.capacity;
        let mut logical = 0usize;
        let mut visual = 0usize;
        loop {
            match &node.kind {
                NodeKind::Leaf(leaf) => return Some((leaf, logical, visual)),
                NodeKind::Branch(left, right) => {
                    capacity /= 2;
                    if local < capacity {
                        node = left.as_ref()?;
                    } else {
                        local -= capacity;
                        logical =
                            logical.saturating_add(left.as_ref().map_or(0, |node| node.logical));
                        visual = visual.saturating_add(left.as_ref().map_or(0, |node| node.visual));
                        node = right.as_ref()?;
                    }
                }
            }
        }
    }

    fn lookup(&self, value: usize, by_visual: bool) -> Option<SlotLookup> {
        if value
            >= if by_visual {
                self.total_visual()
            } else {
                self.total_logical()
            }
        {
            return None;
        }
        let mut node = self.root.as_ref()?;
        let mut local = value;
        let mut capacity = self.capacity;
        let mut slot_index = 0;
        let mut logical = 0usize;
        let mut visual = 0usize;
        loop {
            match &node.kind {
                NodeKind::Leaf(leaf) => {
                    let local_logical = if by_visual {
                        leaf.visual_prefix.partition_point(|&start| start <= local) - 1
                    } else {
                        local
                    };
                    return Some(SlotLookup {
                        slot_index: slot_index - self.first,
                        local_logical,
                        global_logical: logical.saturating_add(local_logical),
                        global_visual_start: visual
                            .saturating_add(leaf.visual_prefix[local_logical]),
                        global_visual_end: visual
                            .saturating_add(leaf.visual_prefix[local_logical + 1]),
                    });
                }
                NodeKind::Branch(left, right) => {
                    capacity /= 2;
                    let left_size = left
                        .as_ref()
                        .map_or(0, |node| if by_visual { node.visual } else { node.logical });
                    if local < left_size {
                        node = left.as_ref()?;
                    } else {
                        local -= left_size;
                        slot_index += capacity;
                        logical =
                            logical.saturating_add(left.as_ref().map_or(0, |node| node.logical));
                        visual = visual.saturating_add(left.as_ref().map_or(0, |node| node.visual));
                        node = right.as_ref()?;
                    }
                }
            }
        }
    }

    #[must_use]
    pub(super) fn slot_count(&self) -> usize {
        self.count
    }
    pub(super) fn total_logical(&self) -> usize {
        self.root.as_ref().map_or(0, |node| node.logical)
    }
    pub(super) fn total_visual(&self) -> usize {
        self.root.as_ref().map_or(0, |node| node.visual)
    }
    pub(super) fn slot_visual_start(&self, slot: usize) -> Option<usize> {
        self.slot(slot).map(|(_, _, visual)| visual)
    }
    pub(super) fn slot_visual_range(&self, slot: usize) -> Option<(usize, usize)> {
        let (leaf, _, start) = self.slot(slot)?;
        let end = start.saturating_add(leaf.visual_prefix.last().copied().unwrap_or(0));
        (start < end).then_some((start, end))
    }
    pub(super) fn visual_lookup(&self, visual: usize) -> Option<SlotLookup> {
        self.lookup(visual, true)
    }
    pub(super) fn logical_lookup(&self, logical: usize) -> Option<SlotLookup> {
        self.lookup(logical, false)
    }
    pub(super) fn materialize(&self, slot: usize) -> Option<Arc<SlotLines>> {
        let (leaf, _, _) = self.slot(slot)?;
        if let Some(lines) = leaf.lines.upgrade().or_else(|| leaf.resident.clone()) {
            return Some(lines);
        }
        let (items, grid, context, frame) = self.source.as_ref()?;
        let mut cache = EntryRenderCache::default();
        // 与常驻渲染同一转场判定：相邻 entry 决定过程区进入/退出台阶（§6）。
        let transition = super::transcript::zone_transition(items, slot);
        cache.ensure(items.get(slot)?, grid, context.clone(), *frame, transition);
        Some(Arc::new(SlotLines::from_entry(&cache)))
    }
    #[cfg(test)]
    pub(super) fn line(&self, slot: usize, local: usize) -> Option<Line<'static>> {
        self.materialize(slot)?.line(local).cloned()
    }
    pub(super) fn canonical_items(&self) -> Option<&im::Vector<TuiRenderUnit>> {
        self.source.as_ref().map(|(items, _, _, _)| items)
    }
    pub(super) fn viewport_logical_range(
        &self,
        scroll_y: usize,
        height: usize,
    ) -> Option<(usize, usize, usize)> {
        if height == 0 {
            return None;
        }
        let start = self.visual_lookup(scroll_y)?;
        let end = self.visual_lookup(
            scroll_y
                .saturating_add(height.saturating_sub(1))
                .min(self.total_visual().saturating_sub(1)),
        )?;
        Some((
            start.global_logical,
            end.global_logical,
            scroll_y - start.global_visual_start,
        ))
    }
    pub(super) fn visible_slots(&self, scroll_y: usize, height: usize) -> std::ops::Range<usize> {
        let Some(start) = self.visual_lookup(scroll_y) else {
            return 0..0;
        };
        let last = scroll_y
            .saturating_add(height.saturating_sub(1))
            .min(self.total_visual().saturating_sub(1));
        let end = self
            .visual_lookup(last)
            .map_or(start.slot_index, |entry| entry.slot_index);
        start.slot_index..end + 1
    }
}

#[cfg(test)]
#[path = "transcript_index_test.rs"]
mod tests;
