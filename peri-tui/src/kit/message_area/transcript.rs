use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::grid::GridSpec;
use super::selection::{SlotIndex, SlotLines};
use super::vm_cache::VmCacheSlot;
use crate::kit::atoms::{TranscriptPublication, ViewModelsSnapshot};
use crate::kit::entry_render_cache::{EntryInvalidation, EntryRenderContext};
use crate::kit::tui_render_unit::TuiRenderUnit;

const HEAVY_CACHE_BUDGET: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub(super) struct Transcript {
    index: Arc<SlotIndex>,
    generation: Option<u64>,
    reset: u64,
    grid: Option<GridSpec>,
    width: u16,
    context: Option<EntryRenderContext>,
    animations: BTreeMap<usize, u64>,
    anchors: BTreeSet<usize>,
    recency: BTreeSet<(u64, usize)>,
    stamps: Vec<u64>,
    bytes: Vec<usize>,
    retained_bytes: usize,
    clock: u64,
    frame: Option<u64>,
    pub(super) scanned_slots: usize,
    pub(super) updated_slots: usize,
    pub(super) evictions: usize,
    pub(super) invalidation: &'static str,
}

impl Transcript {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare(
        &mut self,
        snapshot: &ViewModelsSnapshot,
        publication: &TranscriptPublication,
        caches: &mut Vec<VmCacheSlot>,
        grid: GridSpec,
        width: u16,
        context: EntryRenderContext,
        frame: u64,
        reset: u64,
        scroll_y: usize,
        height: usize,
    ) -> Arc<SlotIndex> {
        self.scanned_slots = 0;
        self.updated_slots = 0;
        self.evictions = 0;
        let global = self.reset != reset
            || self.grid != Some(grid)
            || self.width != width
            || self.context.as_ref().is_none_or(|previous| {
                previous.language != context.language
                    || !Arc::ptr_eq(&previous.theme, &context.theme)
            });
        let changed = self.generation != Some(snapshot.generation) || self.reset != reset;
        self.invalidation = if self.reset != reset {
            "reset"
        } else if self.generation.is_none() {
            "initial"
        } else if self.grid != Some(grid) {
            "grid"
        } else if self.width != width {
            "width"
        } else if global {
            "theme-or-language"
        } else if changed
            && publication.generation == snapshot.generation
            && self.generation == Some(publication.previous_generation)
        {
            "adjacent-publication"
        } else if changed {
            "generation-gap-or-unmatched-publication"
        } else {
            "none"
        };
        let changed_from = if global || self.generation.is_none() {
            0
        } else if changed
            && publication.generation == snapshot.generation
            && self.generation == Some(publication.previous_generation)
        {
            publication.changed_from.min(snapshot.items.len())
        } else if changed {
            0
        } else {
            snapshot.items.len()
        };
        if global || changed {
            let length = snapshot.items.len();
            let previous_length = caches.len();
            for slot in length..caches.len() {
                self.remove_accounting(slot);
            }
            caches.resize_with(length, VmCacheSlot::default);
            self.stamps.resize(length, 0);
            self.bytes.resize(length, 0);
            self.animations.split_off(&changed_from);
            self.anchors.split_off(&changed_from);
            Arc::make_mut(&mut self.index).truncate(length);
            Arc::make_mut(&mut self.index).set_source(
                snapshot.items.clone(),
                grid,
                context.clone(),
                frame,
            );
            let pinned = self.index.visible_slots(
                scroll_y.min(self.index.total_visual().saturating_sub(height)),
                height,
            );
            for slot in changed_from..length {
                self.scanned_slots += 1;
                let vm = &snapshot.items[slot];
                let period = vm.animation_period_frames();
                if period != 0 {
                    self.animations.insert(slot, period);
                }
                if matches!(vm, TuiRenderUnit::TuiAskUserBlock(block) if block.pending) {
                    self.anchors.insert(slot);
                }
                if global || slot >= previous_length || !caches[slot].matches_content(vm) {
                    self.ensure_slot(slot, vm, caches, grid, width, &context, frame);
                    self.enforce_budget(caches, &pinned);
                }
            }
        }
        let scroll_y = scroll_y.min(self.index.total_visual().saturating_sub(height));
        let viewport = self.index.visible_slots(scroll_y, height);
        let animated: Vec<_> = self
            .animations
            .range(viewport.clone())
            .filter(|(_, period)| {
                self.frame
                    .is_none_or(|previous| previous / **period != frame / **period)
            })
            .map(|(&slot, _)| slot)
            .collect();
        for slot in animated {
            self.ensure_slot(
                slot,
                &snapshot.items[slot],
                caches,
                grid,
                width,
                &context,
                frame,
            );
        }
        for slot in viewport.clone() {
            if caches[slot].lines.is_none() {
                self.ensure_slot(
                    slot,
                    &snapshot.items[slot],
                    caches,
                    grid,
                    width,
                    &context,
                    frame,
                );
            }
            self.touch(slot);
        }
        self.enforce_budget(caches, &viewport);
        self.generation = Some(snapshot.generation);
        self.reset = reset;
        self.grid = Some(grid);
        self.width = width;
        self.context = Some(context);
        self.frame = Some(frame);
        Arc::clone(&self.index)
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_slot(
        &mut self,
        slot: usize,
        vm: &TuiRenderUnit,
        caches: &mut [VmCacheSlot],
        grid: GridSpec,
        width: u16,
        context: &EntryRenderContext,
        frame: u64,
    ) {
        let cache = &mut caches[slot];
        drop(cache.lines.take());
        let invalidation = cache.entry.ensure(vm, &grid, context.clone(), frame);
        if matches!(
            invalidation,
            EntryInvalidation::Content | EntryInvalidation::Layout
        ) || cache.wrap_map.is_empty()
        {
            let (height, map) = cache.entry.build_wrap_map(width);
            cache.visual_rows = height;
            cache.wrap_map = Arc::new(map);
        }
        cache.content_hash = vm.content_hash();
        cache.variant = Some(std::mem::discriminant(vm));
        cache.copy_button = cache.entry.copy_button().cloned();
        cache.interaction = cache.entry.interaction().cloned();
        cache.image_lines = cache.entry.image_lines().to_vec();
        let lines = Arc::new(match cache.entry.markdown_lines().stable_overlay() {
            Some((start, stable)) => {
                SlotLines::composite(Arc::clone(cache.entry.lines()), start, stable)
            }
            None => SlotLines::single(Arc::clone(cache.entry.lines())),
        });
        if matches!(
            invalidation,
            EntryInvalidation::Animation | EntryInvalidation::None
        ) {
            Arc::make_mut(&mut self.index).replace_lines(slot, Arc::clone(&lines));
        } else {
            Arc::make_mut(&mut self.index).set_slot(
                slot,
                Arc::clone(&lines),
                &cache.wrap_map,
                false,
            );
        }
        cache.lines = Some(lines);
        self.updated_slots += 1;
        self.retained_bytes = self.retained_bytes.saturating_sub(self.bytes[slot]);
        self.bytes[slot] = cache.retained_bytes();
        self.retained_bytes += self.bytes[slot];
        self.touch(slot);
    }

    fn touch(&mut self, slot: usize) {
        self.recency.remove(&(self.stamps[slot], slot));
        self.clock = self.clock.wrapping_add(1);
        self.stamps[slot] = self.clock;
        if self.bytes[slot] != 0 {
            self.recency.insert((self.clock, slot));
        }
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    fn remove_accounting(&mut self, slot: usize) {
        self.recency.remove(&(self.stamps[slot], slot));
        self.retained_bytes = self.retained_bytes.saturating_sub(self.bytes[slot]);
        self.bytes[slot] = 0;
    }

    fn enforce_budget(&mut self, caches: &mut [VmCacheSlot], pinned: &std::ops::Range<usize>) {
        self.evict_to_budget(caches, pinned, HEAVY_CACHE_BUDGET);
    }

    fn evict_to_budget(
        &mut self,
        caches: &mut [VmCacheSlot],
        pinned: &std::ops::Range<usize>,
        budget: usize,
    ) {
        let mut visible = Vec::new();
        while self.retained_bytes > budget {
            let Some((stamp, slot)) = self.recency.pop_first() else {
                break;
            };
            if pinned.contains(&slot) {
                visible.push((stamp, slot));
                continue;
            }
            caches[slot].evict();
            self.retained_bytes = self.retained_bytes.saturating_sub(self.bytes[slot]);
            self.bytes[slot] = 0;
            self.evictions += 1;
        }
        self.recency.extend(visible);
    }

    pub(super) fn anchor(&self) -> Option<usize> {
        self.anchors.first().copied()
    }

    pub(super) fn warm_visible(
        &mut self,
        snapshot: &ViewModelsSnapshot,
        caches: &mut [VmCacheSlot],
        scroll_y: usize,
        height: usize,
    ) -> Arc<SlotIndex> {
        let viewport = self.index.visible_slots(scroll_y, height);
        let context = self.context.clone().unwrap();
        for slot in viewport.clone() {
            if caches[slot].lines.is_none() {
                self.ensure_slot(
                    slot,
                    &snapshot.items[slot],
                    caches,
                    self.grid.unwrap(),
                    self.width,
                    &context,
                    self.frame.unwrap(),
                );
            }
            self.touch(slot);
        }
        self.enforce_budget(caches, &viewport);
        Arc::clone(&self.index)
    }
}

#[cfg(test)]
#[path = "transcript_test.rs"]
mod tests;
