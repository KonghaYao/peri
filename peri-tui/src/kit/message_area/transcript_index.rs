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
    count: usize,
    source: Option<(im::Vector<TuiRenderUnit>, GridSpec, EntryRenderContext, u64)>,
}

#[derive(Clone)]
struct LayoutSlot {
    lines: Weak<SlotLines>,
    resident: Option<Arc<SlotLines>>,
    visual_prefix: Arc<Vec<usize>>,
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

    fn set(root: &Option<Arc<Self>>, capacity: usize, slot: usize, leaf: Arc<Self>) -> Arc<Self> {
        if capacity == 1 {
            return leaf;
        }
        let (mut left, mut right) = match root.as_ref().map(|node| &node.kind) {
            Some(NodeKind::Branch(left, right)) => (left.clone(), right.clone()),
            _ => (None, None),
        };
        let half = capacity / 2;
        if slot < half {
            left = Some(Self::set(&left, half, slot, leaf));
        } else {
            right = Some(Self::set(&right, half, slot - half, leaf));
        }
        Self::branch(left, right).unwrap()
    }

    fn truncate(root: &Option<Arc<Self>>, capacity: usize, count: usize) -> Option<Arc<Self>> {
        if count == 0 {
            return None;
        }
        if count >= capacity {
            return root.clone();
        }
        let node = root.as_ref()?;
        let NodeKind::Branch(left, right) = &node.kind else {
            return root.clone();
        };
        let half = capacity / 2;
        Self::branch(
            Self::truncate(left, half, count.min(half)),
            Self::truncate(right, half, count.saturating_sub(half)),
        )
    }
}

impl SlotIndex {
    #[cfg(test)]
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
        for (slot, (lines, map)) in slots.into_iter().zip(maps).enumerate() {
            index.set_slot(slot, Arc::new(lines), &map, true);
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
        let mut visual_prefix = Vec::with_capacity(map.len() + 1);
        visual_prefix.push(0);
        visual_prefix.extend(map.iter().map(|entry| entry.visual_end));
        let leaf = Arc::new(Node {
            logical: lines.len(),
            visual: visual_prefix.last().copied().unwrap_or(0),
            kind: NodeKind::Leaf(LayoutSlot {
                lines: Arc::downgrade(&lines),
                resident: retain.then_some(lines),
                visual_prefix: Arc::new(visual_prefix),
            }),
        });
        if self.capacity == 0 {
            self.capacity = 1;
        }
        while slot >= self.capacity {
            self.root = Node::branch(self.root.take(), None);
            self.capacity *= 2;
        }
        self.root = Some(Node::set(&self.root, self.capacity, slot, leaf));
        self.count = self.count.max(slot + 1);
    }

    pub(super) fn truncate(&mut self, count: usize) {
        self.root = Node::truncate(&self.root, self.capacity, count);
        self.count = self.count.min(count);
        if count == 0 {
            self.capacity = 0;
        }
    }

    pub(super) fn replace_lines(&mut self, slot: usize, lines: Arc<SlotLines>) {
        let Some((old, _, _)) = self.slot(slot) else {
            return;
        };
        let leaf = Arc::new(Node {
            logical: old.visual_prefix.len().saturating_sub(1),
            visual: old.visual_prefix.last().copied().unwrap_or(0),
            kind: NodeKind::Leaf(LayoutSlot {
                lines: Arc::downgrade(&lines),
                resident: None,
                visual_prefix: Arc::clone(&old.visual_prefix),
            }),
        });
        self.root = Some(Node::set(&self.root, self.capacity, slot, leaf));
    }

    fn slot(&self, slot: usize) -> Option<(&LayoutSlot, usize, usize)> {
        if slot >= self.count {
            return None;
        }
        let mut node = self.root.as_ref()?;
        let mut local = slot;
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
                        slot_index,
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
        cache.ensure(items.get(slot)?, grid, context.clone(), *frame);
        Some(Arc::new(match cache.markdown_lines().stable_overlay() {
            Some((start, stable)) => SlotLines::composite(Arc::clone(cache.lines()), start, stable),
            None => SlotLines::single(Arc::clone(cache.lines())),
        }))
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
