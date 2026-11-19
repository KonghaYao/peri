use super::plain;

use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use peri_theme::theme::ThemeDefinition;
use ratatui_kit::prelude::ComponentDrawer;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::text::Line;
use ratatui_kit::ratatui::widgets::Paragraph;
use ratatui_kit::ratatui::widgets::Wrap;

use crate::kit::entry_render_cache::{
    EntryInvalidation, EntryRenderCache, EntryRenderContext, EntrySurface,
};
use crate::kit::message_area::grid::{GridSpec, ZoneTransition};
use crate::kit::message_area::selection::WrappedLineInfo;
use crate::kit::tui_render_unit::{FoldKey, FoldState, TuiRenderUnit, TuiSubAgentGroup};

pub(super) const DETAIL_CACHE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub(in super::super) struct DetailRenderCache {
    pub(super) instance_id: String,
    pub(super) context: Option<EntryRenderContext>,
    pub(super) grid: GridSpec,
    pub(super) source: im::Vector<TuiRenderUnit>,
    pub(super) slots: Vec<DetailSlot>,
    pub(super) prefix: Vec<usize>,
    pub(super) resident_bytes: usize,
    pub(super) resident_slots: BTreeSet<usize>,
    pub(super) header: Vec<Line<'static>>,
    pub(super) plain: Option<plain::TextLayout>,
    pub(super) folds: std::collections::HashMap<FoldKey, FoldState>,
    pub(super) focused: Option<FoldKey>,
    pub(super) dirty: bool,
    pub(super) source_changed: bool,
}

#[derive(Default)]
pub(super) struct DetailSlot {
    pub(super) entry: EntryRenderCache,
    pub(super) hash: Option<u64>,
    pub(super) variant: Option<std::mem::Discriminant<TuiRenderUnit>>,
    pub(super) wrap_map: Vec<WrappedLineInfo>,
    pub(super) height: usize,
    pub(super) resident: bool,
    pub(super) retained_bytes: usize,
}

impl DetailSlot {
    pub(super) fn evict(&mut self) {
        self.entry.evict();
        self.wrap_map = Vec::new();
        self.resident = false;
        self.retained_bytes = 0;
    }

    #[must_use]
    pub(super) fn bytes(&self) -> usize {
        self.retained_bytes
    }

    pub(super) fn ensure(
        &mut self,
        vm: &TuiRenderUnit,
        grid: &GridSpec,
        context: EntryRenderContext,
        frame: u64,
    ) {
        // 面板沿用基准列（`indent = 0`），无过程区转场台阶形态。
        let invalidation = self
            .entry
            .ensure(vm, grid, context, frame, ZoneTransition::None);
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
    pub(in super::super) fn prepare(
        &mut self,
        group: &TuiSubAgentGroup,
        grid: &GridSpec,
        theme: Arc<ThemeDefinition>,
        language: u64,
        header: Vec<Line<'static>>,
    ) -> bool {
        let switched = self.instance_id != group.instance_id || self.plain.is_some();
        self.plain = None;
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
            self.folds = std::collections::HashMap::new();
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
        if source_changed {
            let keys: HashSet<_> = self.source.iter().filter_map(fold_key).collect();
            self.folds.retain(|key, _| keys.contains(key));
            if self.focused.as_ref().is_some_and(|key| !keys.contains(key)) {
                self.focused = None;
            }
            if self.folds.capacity() > self.folds.len().saturating_mul(2) {
                self.folds.shrink_to_fit();
            }
        }
        self.slots
            .resize_with(self.source.len(), DetailSlot::default);
        if self.slots.capacity() > self.slots.len().saturating_mul(2) {
            self.slots.shrink_to_fit();
        }
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
                slot.ensure(
                    &vm,
                    grid,
                    self.context
                        .as_ref()
                        .expect("detail cache preparation installs context before materialization")
                        .clone(),
                    frame,
                );
            }
            self.prefix.push(
                self.prefix
                    .last()
                    .copied()
                    .expect("the prefix table starts with zero before chunk lengths are appended")
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
        if self.prefix.capacity() > self.prefix.len().saturating_mul(2) {
            self.prefix.shrink_to_fit();
        }
        switched
    }

    pub(in super::super) fn clear(&mut self, header: Vec<Line<'static>>) {
        *self = Self {
            header,
            ..Self::default()
        };
    }

    pub(in super::super) fn prepare_text(
        &mut self,
        header: Vec<Line<'static>>,
        text: Arc<str>,
        width: usize,
    ) {
        if self
            .plain
            .as_ref()
            .is_some_and(|layout| layout.matches(&text, width))
        {
            self.header = header;
            return;
        }
        // Replacing the mode drops all preview body references and fold metadata.
        *self = Self {
            header,
            plain: Some(plain::TextLayout::new(text, width)),
            ..Self::default()
        };
    }

    pub(in super::super) fn height(&self) -> usize {
        self.header
            .len()
            .saturating_add(self.plain.as_ref().map_or_else(
                || self.prefix.last().copied().unwrap_or(0),
                plain::TextLayout::height,
            ))
    }

    pub(in super::super) fn focused(&self) -> Option<usize> {
        self.source
            .iter()
            .position(|vm| fold_key(vm).as_ref() == self.focused.as_ref())
            .filter(|_| self.focused.is_some())
    }

    #[must_use]
    pub(in super::super) fn source_changed(&self) -> bool {
        self.source_changed
    }

    pub(in super::super) fn focus_next(&mut self, backwards: bool) -> Option<usize> {
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

    pub(in super::super) fn toggle_focused(&mut self, preview: bool) -> bool {
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

    pub(in super::super) fn click_fold_header(&mut self, row: usize) -> bool {
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

    pub(super) fn slots_for(&self, rows: Range<usize>) -> Range<usize> {
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

    pub(super) fn ensure_visible(&mut self, rows: Range<usize>) -> Range<usize> {
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

    pub(in super::super) fn draw(&mut self, drawer: &mut ComponentDrawer, area: Rect, top: usize) {
        let bottom = top.saturating_add(area.height as usize).min(self.height());
        {
            let iteration_items = top..bottom.min(self.header.len());
            for index in iteration_items {
                drawer.render_widget(
                    Paragraph::new(self.header[index].clone()),
                    Rect {
                        y: area.y.saturating_add((index - top) as u16),
                        height: 1,
                        ..area
                    },
                );
            }
        }
        let header_height = self.header.len();
        let content_rows = top.saturating_sub(header_height)..bottom.saturating_sub(header_height);
        if let Some(plain) = &self.plain {
            {
                let iteration_items = plain.lines(content_rows.clone()).into_iter().enumerate();
                for (index, line) in iteration_items {
                    drawer.render_widget(
                        Paragraph::new(line),
                        Rect {
                            y: area.y.saturating_add(
                                (header_height + content_rows.start + index - top) as u16,
                            ),
                            height: 1,
                            ..area
                        },
                    );
                }
            }
            return;
        }
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
            {
                let iteration_items = slot.wrap_map[start..]
                    .iter()
                    .take_while(|line| line.visual_start < local_end);
                for mapped in iteration_items {
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
    }

    #[cfg(test)]
    pub(super) fn visible_lines(&mut self, rows: Range<usize>) -> Vec<Line<'static>> {
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
                            .expect("the visual index maps into a materialized cache entry")
                            .clone(),
                    );
                }
            }
        }
        result
    }
}

pub(super) fn fold_of(vm: &TuiRenderUnit) -> Option<FoldState> {
    match vm {
        TuiRenderUnit::TuiSystemReminder(reminder) => Some(reminder.fold),
        TuiRenderUnit::TuiToolCard(card) => Some(card.fold),
        TuiRenderUnit::TuiAssistantBubble(bubble) => bubble.reasoning.as_ref().map(|r| r.fold),
        TuiRenderUnit::TuiCollapsedGroup(group) => Some(group.fold),
        _ => None,
    }
}

pub(super) fn fold_key(vm: &TuiRenderUnit) -> Option<FoldKey> {
    match vm {
        TuiRenderUnit::TuiSystemReminder(reminder) => {
            Some(FoldKey::SystemReminder(reminder.reminder_id))
        }
        TuiRenderUnit::TuiToolCard(card) => Some(FoldKey::Tool(card.tool_id.clone())),
        TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.reasoning.is_some() => {
            bubble.message_id.clone().map(FoldKey::Reasoning)
        }
        TuiRenderUnit::TuiCollapsedGroup(group) => Some(FoldKey::Group(group.key.clone())),
        _ => None,
    }
}

pub(super) fn projected(vm: &TuiRenderUnit, fold: Option<FoldState>) -> Cow<'_, TuiRenderUnit> {
    let Some(fold) = fold else {
        return Cow::Borrowed(vm);
    };
    let mut vm = vm.clone();
    match &mut vm {
        TuiRenderUnit::TuiSystemReminder(reminder) => {
            reminder.fold = fold;
            reminder.recompute_hash();
        }
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

pub(super) fn current_frame() -> u64 {
    peri_time::now_wall()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64 / 100)
        .unwrap_or(0)
}
