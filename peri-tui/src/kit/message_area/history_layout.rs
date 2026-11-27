//! Exact page measurements prepared by the ACP bridge before publishing a prepend. Only row
//! geometry crosses threads. The UI always has a synchronous path and never waits for a worker.
use super::grid::{GridSpec, ZoneTransition};
use super::selection::SlotLines;
use super::slot_index::ColdLayout;
use super::transcript::zone_transition;
use crate::kit::atoms::{HISTORY_PREPEND_EPOCH, VIEW_MODELS};
use crate::kit::entry_render_cache::{EntryRenderCache, EntryRenderContext};
use crate::kit::tui_render_unit::{TuiRenderUnit, history_overlap};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::thread::ThreadId;

// At most one unpublished page of row offsets, on top of the existing transcript cache budget.
const PAGE_LAYOUT_BYTES: usize = 512 * 1024;

#[derive(Clone)]
struct FrameLayout {
    grid: GridSpec,
    width: u16,
    context: EntryRenderContext,
    locale: String,
    ui_thread: ThreadId,
    generation: u64,
    epoch: u64,
}

impl FrameLayout {
    fn matches(&self, grid: GridSpec, width: u16, context: &EntryRenderContext) -> bool {
        self.grid == grid
            && self.width == width
            && self.context.language == context.language
            && self.context.occurrence == context.occurrence
            && self.context.surface == context.surface
            && Arc::ptr_eq(&self.context.theme, &context.theme)
    }
}

struct SlotMeasurement {
    hash: u64,
    transition: ZoneTransition,
    layout: ColdLayout,
}

pub(crate) struct PageMeasurements {
    frame: FrameLayout,
    slots: BTreeMap<usize, SlotMeasurement>,
}

impl PageMeasurements {
    pub(super) fn take(
        &mut self,
        slot: usize,
        vm: &TuiRenderUnit,
        transition: ZoneTransition,
    ) -> Option<ColdLayout> {
        let measured = self.slots.remove(&slot)?;
        (matches!(vm, TuiRenderUnit::TuiAssistantBubble(_))
            && measured.hash == vm.content_hash()
            && measured.transition == transition)
            .then_some(measured.layout)
    }
}

struct Coordinator {
    displayed: Option<FrameLayout>,
    next: Option<(u64, PageMeasurements)>,
}
static COORDINATOR: Mutex<Coordinator> = Mutex::new(Coordinator {
    displayed: None,
    next: None,
});

/// Called by the real viewport. No renderer or atom read occurs while the coordinator is locked.
pub(super) fn begin_frame(
    generation: u64,
    epoch: u64,
    grid: GridSpec,
    width: u16,
    context: &EntryRenderContext,
) -> Option<PageMeasurements> {
    let mut state = COORDINATOR.lock();
    if let Some(frame) = state.displayed.as_mut().filter(|frame| {
        frame.matches(grid, width, context) && frame.ui_thread == std::thread::current().id()
    }) {
        frame.generation = generation;
        frame.epoch = epoch;
    } else {
        state.displayed = Some(FrameLayout {
            grid,
            width,
            context: context.clone(),
            generation,
            epoch,
            locale: crate::i18n::current_language(),
            ui_thread: std::thread::current().id(),
        });
    }
    if state
        .next
        .as_ref()
        .is_some_and(|(next, _)| *next == generation)
    {
        let (_, page) = state.next.take()?;
        return page.frame.matches(grid, width, context).then_some(page);
    }
    if state
        .next
        .as_ref()
        .is_some_and(|(next, _)| *next < generation)
    {
        state.next = None;
    }
    None
}

/// Called outside VIEW_MODELS' write lock, on the existing bridge thread. The previous frame
/// must be the actual displayed source. Mid-draw resizes, folds or session resets fall back.
pub(crate) fn measure_page(
    previous: u64,
    items: &im::Vector<TuiRenderUnit>,
) -> Option<PageMeasurements> {
    let epoch = HISTORY_PREPEND_EPOCH.get();
    let frame = COORDINATOR
        .lock()
        .displayed
        .as_ref()
        .filter(|frame| {
            frame.generation == previous
                && frame.epoch != epoch
                && frame.ui_thread != std::thread::current().id()
        })
        .cloned()?;
    let old = VIEW_MODELS.state().read().clone();
    if old.generation != previous {
        return None;
    }
    let run = history_overlap(&old.items, items);
    if run.len == 0 || run.new_start == 0 {
        return None;
    }
    let reset = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
    let slots = crate::i18n::with_language(&frame.locale, || {
        measure_prefix(items, run.new_start, &frame, reset)
    })?;
    (!slots.is_empty()).then_some(PageMeasurements { frame, slots })
}

fn measure_prefix(
    items: &im::Vector<TuiRenderUnit>,
    end: usize,
    frame: &FrameLayout,
    reset: u64,
) -> BTreeMap<usize, SlotMeasurement> {
    let mut slots = BTreeMap::new();
    let mut bytes = 0;
    for (slot, vm) in items.iter().take(end).enumerate() {
        if crate::kit::atoms::BRIDGE_RESET_COUNTER.get() != reset {
            break;
        }
        let TuiRenderUnit::TuiAssistantBubble(bubble) = vm else {
            continue;
        };
        if bubble.started_at.is_some()
            || bubble.reasoning.is_some()
            || bubble.text.is_empty()
            || bubble.text.contains("gen-ui")
            || crate::kit::mermaid::vm_might_have_mermaid(vm)
            || crate::kit::inline_image::vm_may_have_image(vm)
        {
            continue;
        }
        let transition = zone_transition(items, slot);
        let mut entry = EntryRenderCache::default();
        entry.ensure(vm, &frame.grid, frame.context.clone(), 0, transition);
        if !entry.mermaid_fences().is_empty()
            || !entry.image_lines().is_empty()
            || !entry.markdown_lines().genui_ranges.is_empty()
            || !entry.fold_hits().is_empty()
        {
            continue;
        }
        let (_, map) = entry.build_wrap_map(frame.width);
        let layout = ColdLayout::measure(&SlotLines::from_entry(&entry), &map);
        bytes += layout.bytes() + size_of::<SlotMeasurement>();
        if bytes > PAGE_LAYOUT_BYTES {
            break;
        }
        slots.insert(
            slot,
            SlotMeasurement {
                hash: vm.content_hash(),
                transition,
                layout,
            },
        );
    }
    slots
}

/// The publisher holds the snapshot write lock. Another publication supersedes old hints.
pub(crate) fn publish(generation: u64, page: Option<PageMeasurements>) {
    COORDINATOR.lock().next = page.map(|page| (generation, page));
}

/// A session reset also releases the previous frame context. The UI records the new one on draw.
pub(crate) fn reset() {
    let mut state = COORDINATOR.lock();
    state.displayed = None;
    state.next = None;
}

#[cfg(test)]
pub(super) fn retained_for_test() -> (bool, bool) {
    let state = COORDINATOR.lock();
    (state.displayed.is_some(), state.next.is_some())
}
