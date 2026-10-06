use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use peri_theme::theme::ThemeDefinition;
use ratatui_kit::prelude::ComponentDrawer;
use ratatui_kit::ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Wrap},
};

use crate::kit::entry_render_cache::{
    EntryInvalidation, EntryRenderCache, EntryRenderContext, EntrySurface,
};
use crate::kit::message_area::grid::GridSpec;
use crate::kit::message_area::selection::WrappedLineInfo;
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiSubAgentGroup};

const DETAIL_CACHE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub(super) struct DetailRenderCache {
    instance_id: String,
    context: Option<EntryRenderContext>,
    grid: GridSpec,
    source: im::Vector<TuiRenderUnit>,
    slots: Vec<DetailSlot>,
    prefix: Vec<usize>,
    resident_bytes: usize,
    resident_slots: BTreeSet<usize>,
    header: Vec<Line<'static>>,
}

#[derive(Default)]
struct DetailSlot {
    entry: EntryRenderCache,
    hash: Option<u64>,
    variant: Option<std::mem::Discriminant<TuiRenderUnit>>,
    wrap_map: Vec<WrappedLineInfo>,
    height: usize,
    resident: bool,
    retained_bytes: usize,
}

impl DetailSlot {
    fn evict(&mut self) {
        self.entry.evict();
        self.wrap_map = Vec::new();
        self.resident = false;
        self.retained_bytes = 0;
    }

    fn bytes(&self) -> usize {
        self.retained_bytes
    }

    fn ensure(
        &mut self,
        vm: &TuiRenderUnit,
        grid: &GridSpec,
        context: EntryRenderContext,
        frame: u64,
    ) {
        let invalidation = self.entry.ensure(vm, grid, context, frame);
        if !self.resident
            || matches!(
                invalidation,
                EntryInvalidation::Content | EntryInvalidation::Layout
            )
        {
            (self.height, self.wrap_map) = self.entry.build_wrap_map(grid.line_width());
        }
        self.resident = true;
        if invalidation != EntryInvalidation::None {
            self.retained_bytes = self.entry.retained_bytes()
                + self.wrap_map.capacity() * std::mem::size_of::<WrappedLineInfo>();
        }
        self.hash = Some(vm.content_hash());
        self.variant = Some(std::mem::discriminant(vm));
    }
}

impl DetailRenderCache {
    pub(super) fn prepare(
        &mut self,
        group: &TuiSubAgentGroup,
        grid: &GridSpec,
        theme: Arc<ThemeDefinition>,
        language: u64,
        header: Vec<Line<'static>>,
    ) -> bool {
        let switched = self.instance_id != group.instance_id;
        let context_changed = switched
            || self.grid != *grid
            || self.context.as_ref().is_none_or(|context| {
                context.language != language || !Arc::ptr_eq(&context.theme, &theme)
            });
        self.header = header;
        if !context_changed && self.source.ptr_eq(&group.view_models) {
            return false;
        }
        if switched {
            self.slots = Vec::new();
            self.instance_id.clone_from(&group.instance_id);
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        group.instance_id.hash(&mut hasher);
        self.context = Some(EntryRenderContext {
            theme,
            language,
            occurrence: hasher.finish(),
            surface: EntrySurface::Detail,
        });
        self.grid = *grid;
        self.source = group.view_models.clone();
        self.slots
            .resize_with(self.source.len(), DetailSlot::default);
        self.prefix.clear();
        self.prefix.push(0);
        self.resident_bytes = 0;
        self.resident_slots.clear();
        let frame = current_frame();
        for (index, (slot, vm)) in self.slots.iter_mut().zip(&self.source).enumerate() {
            if context_changed
                || slot.hash != Some(vm.content_hash())
                || slot.variant != Some(std::mem::discriminant(vm))
            {
                slot.ensure(vm, grid, self.context.as_ref().unwrap().clone(), frame);
            }
            self.prefix.push(
                self.prefix
                    .last()
                    .copied()
                    .unwrap()
                    .saturating_add(slot.height),
            );
            let bytes = slot.bytes();
            if self.resident_bytes.saturating_add(bytes) > DETAIL_CACHE_BYTES {
                slot.evict();
            } else {
                self.resident_bytes += bytes;
                if slot.resident {
                    self.resident_slots.insert(index);
                }
            }
        }
        switched
    }

    pub(super) fn clear(&mut self, header: Vec<Line<'static>>) {
        *self = Self {
            header,
            ..Self::default()
        };
    }

    pub(super) fn height(&self) -> usize {
        self.header
            .len()
            .saturating_add(self.prefix.last().copied().unwrap_or(0))
    }

    fn slots_for(&self, rows: Range<usize>) -> Range<usize> {
        let start = self
            .prefix
            .partition_point(|offset| *offset <= rows.start)
            .saturating_sub(1);
        let end = self
            .prefix
            .partition_point(|offset| *offset < rows.end)
            .min(self.slots.len());
        start.min(self.slots.len())..end.max(start.min(self.slots.len()))
    }

    fn ensure_visible(&mut self, rows: Range<usize>) -> Range<usize> {
        let visible = self.slots_for(rows);
        let Some(context) = self.context.as_ref() else {
            return visible;
        };
        let frame = current_frame();
        for index in visible.clone() {
            let slot = &mut self.slots[index];
            let previous_bytes = slot.bytes();
            slot.ensure(&self.source[index], &self.grid, context.clone(), frame);
            self.resident_bytes = self
                .resident_bytes
                .saturating_sub(previous_bytes)
                .saturating_add(slot.bytes());
            self.resident_slots.insert(index);
        }
        if self.resident_bytes > DETAIL_CACHE_BYTES {
            let cold: Vec<_> = self
                .resident_slots
                .iter()
                .copied()
                .filter(|index| !visible.contains(index))
                .collect();
            for index in cold {
                let slot = &mut self.slots[index];
                self.resident_bytes = self.resident_bytes.saturating_sub(slot.bytes());
                slot.evict();
                self.resident_slots.remove(&index);
                if self.resident_bytes <= DETAIL_CACHE_BYTES {
                    break;
                }
            }
        }
        visible
    }

    pub(super) fn draw(&mut self, drawer: &mut ComponentDrawer, area: Rect, top: usize) {
        let bottom = top.saturating_add(area.height as usize).min(self.height());
        for index in top..bottom.min(self.header.len()) {
            drawer.render_widget(
                Paragraph::new(self.header[index].clone()),
                Rect {
                    y: area.y.saturating_add((index - top) as u16),
                    height: 1,
                    ..area
                },
            );
        }
        let header_height = self.header.len();
        let content_rows = top.saturating_sub(header_height)..bottom.saturating_sub(header_height);
        let visible = self.ensure_visible(content_rows.clone());
        for index in visible {
            let slot = &self.slots[index];
            let offset = self.prefix[index];
            let local_start = content_rows.start.saturating_sub(offset);
            let local_end = content_rows.end.saturating_sub(offset).min(slot.height);
            let start = slot
                .wrap_map
                .partition_point(|line| line.visual_end <= local_start);
            for mapped in slot.wrap_map[start..]
                .iter()
                .take_while(|line| line.visual_start < local_end)
            {
                let row_start = mapped.visual_start.max(local_start);
                let row_end = mapped.visual_end.min(local_end);
                let Some(line) = slot.entry.line(mapped.logical_idx) else {
                    continue;
                };
                let paragraph = Paragraph::new(line.clone())
                    .wrap(Wrap { trim: false })
                    .scroll((
                        (row_start - mapped.visual_start).min(u16::MAX as usize) as u16,
                        0,
                    ));
                drawer.render_widget(
                    paragraph,
                    Rect {
                        y: area
                            .y
                            .saturating_add((header_height + offset + row_start - top) as u16),
                        height: (row_end - row_start) as u16,
                        ..area
                    },
                );
            }
        }
    }

    #[cfg(test)]
    fn visible_lines(&mut self, rows: Range<usize>) -> Vec<Line<'static>> {
        let visible = self.ensure_visible(rows.clone());
        let mut result = Vec::new();
        for index in visible {
            let offset = self.prefix[index];
            for mapped in &self.slots[index].wrap_map {
                if mapped.visual_end + offset > rows.start
                    && mapped.visual_start + offset < rows.end
                {
                    result.push(
                        self.slots[index]
                            .entry
                            .line(mapped.logical_idx)
                            .unwrap()
                            .clone(),
                    );
                }
            }
        }
        result
    }
}

fn current_frame() -> u64 {
    peri_time::now_wall()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64 / 100)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "subagent_detail_cache_test.rs"]
mod tests;
