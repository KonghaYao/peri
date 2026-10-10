use std::borrow::Cow;
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
use crate::kit::tui_render_unit::{FoldKey, FoldState, TuiRenderUnit, TuiSubAgentGroup};

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
    folds: std::collections::HashMap<FoldKey, FoldState>,
    focused: Option<FoldKey>,
    dirty: bool,
    source_changed: bool,
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
        let source_changed = !self.source.ptr_eq(&group.view_models);
        self.source_changed = source_changed && !switched && !self.dirty;
        if !context_changed && !self.dirty && !source_changed {
            return false;
        }
        if switched {
            self.slots = Vec::new();
            self.folds.clear();
            self.focused = None;
            self.instance_id.clone_from(&group.instance_id);
        }
        self.dirty = false;
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
        for index in 0..self.source.len() {
            let vm = projected(
                &self.source[index],
                fold_key(&self.source[index]).and_then(|key| self.folds.get(&key).copied()),
            );
            let slot = &mut self.slots[index];
            if context_changed
                || slot.hash != Some(vm.content_hash())
                || slot.variant != Some(std::mem::discriminant(&vm))
            {
                slot.ensure(&vm, grid, self.context.as_ref().unwrap().clone(), frame);
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

    pub(super) fn focused(&self) -> Option<usize> {
        self.source
            .iter()
            .position(|vm| fold_key(vm).as_ref() == self.focused.as_ref())
            .filter(|_| self.focused.is_some())
    }

    pub(super) fn source_changed(&self) -> bool {
        self.source_changed
    }

    pub(super) fn focus_next(&mut self, backwards: bool) -> Option<usize> {
        let len = self.source.len();
        if len == 0 {
            return None;
        }
        let start = self
            .focused()
            .unwrap_or(if backwards { 0 } else { len - 1 });
        for step in 1..=len {
            let index = if backwards {
                (start + len - step % len) % len
            } else {
                (start + step) % len
            };
            if let Some(key) = fold_key(&self.source[index]) {
                self.focused = Some(key);
                return self
                    .prefix
                    .get(index)
                    .map(|row| self.header.len().saturating_add(*row));
            }
        }
        None
    }

    pub(super) fn toggle_focused(&mut self, preview: bool) -> bool {
        let Some(index) = self.focused() else {
            return false;
        };
        let Some(key) = fold_key(&self.source[index]) else {
            return false;
        };
        let Some(current) = self
            .folds
            .get(&key)
            .copied()
            .or_else(|| fold_of(&self.source[index]))
        else {
            return false;
        };
        let next = if preview {
            if current == crate::kit::tui_render_unit::FoldState::Preview {
                crate::kit::tui_render_unit::FoldState::Collapsed
            } else {
                crate::kit::tui_render_unit::FoldState::Preview
            }
        } else if current == crate::kit::tui_render_unit::FoldState::Collapsed {
            crate::kit::tui_render_unit::FoldState::Expanded
        } else {
            crate::kit::tui_render_unit::FoldState::Collapsed
        };
        self.folds.insert(key, next);
        self.dirty = true;
        true
    }

    pub(super) fn click_fold_header(&mut self, row: usize) -> bool {
        let Some(content_row) = row.checked_sub(self.header.len()) else {
            return false;
        };
        let index = self
            .prefix
            .partition_point(|start| *start <= content_row)
            .saturating_sub(1);
        if index >= self.source.len() || self.prefix[index] != content_row {
            return false;
        }
        let Some(key) = fold_key(&self.source[index]) else {
            return false;
        };
        self.focused = Some(key);
        self.toggle_focused(false)
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
            let vm = projected(
                &self.source[index],
                fold_key(&self.source[index]).and_then(|key| self.folds.get(&key).copied()),
            );
            slot.ensure(&vm, &self.grid, context.clone(), frame);
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
        let focused_index = self.focused();
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
                let line = if focused_index == Some(index) && mapped.logical_idx == 0 {
                    line.clone().style(
                        ratatui_kit::ratatui::style::Style::default()
                            .add_modifier(ratatui_kit::ratatui::style::Modifier::REVERSED),
                    )
                } else {
                    line.clone()
                };
                let paragraph = Paragraph::new(line).wrap(Wrap { trim: false }).scroll((
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
                        width: self.grid.line_width().min(area.width),
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

fn fold_of(vm: &TuiRenderUnit) -> Option<FoldState> {
    match vm {
        TuiRenderUnit::TuiToolCard(card) => Some(card.fold),
        TuiRenderUnit::TuiAssistantBubble(bubble) => bubble.reasoning.as_ref().map(|r| r.fold),
        TuiRenderUnit::TuiCollapsedGroup(group) => Some(group.fold),
        _ => None,
    }
}

fn fold_key(vm: &TuiRenderUnit) -> Option<FoldKey> {
    match vm {
        TuiRenderUnit::TuiToolCard(card) => Some(FoldKey::Tool(card.tool_id.clone())),
        TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.reasoning.is_some() => {
            bubble.message_id.clone().map(FoldKey::Reasoning)
        }
        TuiRenderUnit::TuiCollapsedGroup(group) => {
            let ids: Vec<_> = group
                .view_models
                .iter()
                .filter_map(|vm| match vm {
                    TuiRenderUnit::TuiToolCard(card) => Some(card.tool_id.clone()),
                    _ => None,
                })
                .collect();
            if ids.is_empty() {
                None
            } else {
                Some(FoldKey::Group(ids))
            }
        }
        _ => None,
    }
}

fn projected(vm: &TuiRenderUnit, fold: Option<FoldState>) -> Cow<'_, TuiRenderUnit> {
    let Some(fold) = fold else {
        return Cow::Borrowed(vm);
    };
    let mut vm = vm.clone();
    match &mut vm {
        TuiRenderUnit::TuiToolCard(card) => {
            card.fold = fold;
            card.recompute_hash();
        }
        TuiRenderUnit::TuiAssistantBubble(bubble) => {
            let bubble = Arc::make_mut(bubble);
            if let Some(reasoning) = &mut bubble.reasoning {
                reasoning.fold = fold;
                bubble.recompute_hash();
            }
        }
        TuiRenderUnit::TuiCollapsedGroup(group) => {
            group.fold = fold;
            group.recompute_hash();
        }
        _ => {}
    }
    Cow::Owned(vm)
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
