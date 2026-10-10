use std::mem::Discriminant;
use std::sync::Arc;

use peri_theme::theme::ThemeDefinition;
use ratatui_kit::ratatui::text::Line;

use crate::kit::markdown::MarkdownRenderCache;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::message_area::render::{
    CopyButtonInfo, ImageLineInfo, InteractionLayout, vm_to_lines_cached_with_layout,
};
use crate::kit::message_area::selection::{WrappedLineInfo, build_wrap_map};
use crate::kit::tui_render_unit::TuiRenderUnit;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntrySurface {
    Message,
    Detail,
}

#[derive(Clone)]
pub(crate) struct EntryRenderContext {
    pub(crate) theme: Arc<ThemeDefinition>,
    pub(crate) language: u64,
    pub(crate) surface: EntrySurface,
    pub(crate) occurrence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryInvalidation {
    None,
    Content,
    Layout,
    Animation,
}

#[derive(Default)]
pub(crate) struct EntryRenderCache {
    hash: Option<u64>,
    variant: Option<Discriminant<TuiRenderUnit>>,
    grid: Option<GridSpec>,
    context: Option<EntryRenderContext>,
    frame: u64,
    markdown: MarkdownRenderCache,
    markdown_lines: MarkdownLineCache,
    lines: Arc<Vec<Line<'static>>>,
    copy_button: Option<CopyButtonInfo>,
    interaction: Option<InteractionLayout>,
    image_lines: Vec<ImageLineInfo>,
}

impl EntryRenderCache {
    pub(crate) fn ensure(
        &mut self,
        vm: &TuiRenderUnit,
        grid: &GridSpec,
        context: EntryRenderContext,
        frame: u64,
    ) -> EntryInvalidation {
        let variant = std::mem::discriminant(vm);
        let identity_changed = self.variant != Some(variant)
            || self.context.as_ref().is_some_and(|previous| {
                previous.occurrence != context.occurrence || previous.surface != context.surface
            });
        let content_changed = identity_changed || self.hash != Some(vm.content_hash());
        let layout_changed = self.grid != Some(*grid)
            || self.context.as_ref().is_none_or(|previous| {
                previous.language != context.language
                    || !Arc::ptr_eq(&previous.theme, &context.theme)
            });
        let period = vm.animation_period_frames();
        let animated = period != 0 && self.frame / period != frame / period;
        let invalidation = if content_changed {
            EntryInvalidation::Content
        } else if layout_changed {
            EntryInvalidation::Layout
        } else if animated {
            EntryInvalidation::Animation
        } else {
            return EntryInvalidation::None;
        };
        if identity_changed {
            self.markdown = MarkdownRenderCache::default();
        }
        if layout_changed || identity_changed {
            self.markdown_lines = MarkdownLineCache::default();
        }
        if invalidation == EntryInvalidation::Animation {
            if let TuiRenderUnit::TuiAssistantBubble(bubble) = vm {
                if let Some(reasoning) = bubble.reasoning.as_ref().filter(|entry| entry.is_running)
                    && let Some(first) = Arc::make_mut(&mut self.lines).first_mut()
                {
                    *first = crate::kit::message_area::render::render_running_reasoning_header(
                        reasoning, grid,
                    );
                }
            } else {
                let (lines, copy_button, interaction, image_lines) = vm_to_lines_cached_with_layout(
                    vm,
                    grid,
                    &mut self.markdown,
                    None,
                    context.surface == EntrySurface::Message,
                );
                self.lines = Arc::new(lines);
                self.copy_button = copy_button;
                self.interaction = interaction;
                self.image_lines = image_lines;
            }
        } else {
            self.markdown_lines.stable_start = None;
            let (lines, copy_button, interaction, image_lines) = vm_to_lines_cached_with_layout(
                vm,
                grid,
                &mut self.markdown,
                Some(&mut self.markdown_lines),
                context.surface == EntrySurface::Message,
            );
            self.lines = Arc::new(lines);
            self.copy_button = copy_button;
            self.interaction = interaction;
            self.image_lines = image_lines;
        }
        self.hash = Some(vm.content_hash());
        self.variant = Some(variant);
        self.grid = Some(*grid);
        self.context = Some(context);
        self.frame = frame;
        invalidation
    }

    pub(crate) fn lines(&self) -> &Arc<Vec<Line<'static>>> {
        &self.lines
    }

    pub(crate) fn markdown_lines(&self) -> &MarkdownLineCache {
        &self.markdown_lines
    }

    pub(crate) fn line(&self, index: usize) -> Option<&Line<'static>> {
        if let Some(start) = self.markdown_lines.stable_start
            && index >= start
        {
            let local = index - start;
            let chunk = self
                .markdown_lines
                .prefix
                .partition_point(|offset| *offset <= local)
                .saturating_sub(1);
            if let Some(lines) = self.markdown_lines.stable.get(chunk) {
                return lines.lines.get(local - self.markdown_lines.prefix[chunk]);
            }
        }
        self.lines.get(index)
    }

    pub(crate) fn copy_button(&self) -> Option<&CopyButtonInfo> {
        self.copy_button.as_ref()
    }

    pub(crate) fn interaction(&self) -> Option<&InteractionLayout> {
        self.interaction.as_ref()
    }

    pub(crate) fn image_lines(&self) -> &[ImageLineInfo] {
        &self.image_lines
    }

    pub(crate) fn build_wrap_map(&self, width: u16) -> (usize, Vec<WrappedLineInfo>) {
        self.markdown_lines.build_slot_wrap_map(&self.lines, width)
    }

    pub(crate) fn evict(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.markdown.retained_bytes()
            + retained_lines_bytes(&self.lines)
            + self.markdown_lines.retained_bytes()
            + self.image_lines.capacity() * std::mem::size_of::<ImageLineInfo>()
            + self
                .image_lines
                .iter()
                .map(|image| image.path.capacity() + image.size_text.capacity())
                .sum::<usize>()
            + self
                .interaction
                .as_ref()
                .map(|layout| {
                    layout.option_rows.capacity() * std::mem::size_of::<usize>()
                        + layout.option_cols.capacity() * std::mem::size_of::<Option<(u16, u16)>>()
                })
                .unwrap_or(0)
    }
}

pub(crate) fn retained_lines_bytes(lines: &Vec<Line<'static>>) -> usize {
    lines.capacity() * std::mem::size_of::<Line<'static>>()
        + lines
            .iter()
            .map(|line| {
                line.spans.capacity()
                    * std::mem::size_of::<ratatui_kit::ratatui::text::Span<'static>>()
                    + line
                        .spans
                        .iter()
                        .map(|span| match &span.content {
                            std::borrow::Cow::Owned(text) => text.capacity(),
                            std::borrow::Cow::Borrowed(_) => 0,
                        })
                        .sum::<usize>()
            })
            .sum::<usize>()
}

#[derive(Clone)]
pub(crate) struct MarkdownLineChunk {
    pub(crate) identity: usize,
    pub(crate) lines: Arc<Vec<Line<'static>>>,
    pub(crate) wrap_map: Arc<Vec<WrappedLineInfo>>,
    pub(crate) visual_rows: usize,
}

#[derive(Clone, Default)]
pub(crate) struct MarkdownLineCache {
    pub(crate) width: u16,
    pub(crate) stable_start: Option<usize>,
    pub(crate) stable: Vec<MarkdownLineChunk>,
    prefix: Vec<usize>,
}

impl MarkdownLineCache {
    pub(crate) fn retain_and_wrap(&mut self, width: u16, chunks: Vec<(usize, Vec<Line<'static>>)>) {
        if self.width != width {
            self.stable.clear();
            self.width = width;
        }
        let chunk_count = chunks.len();
        for (index, (identity, lines)) in chunks.into_iter().enumerate() {
            if self
                .stable
                .get(index)
                .is_some_and(|cached| cached.identity == identity)
            {
                continue;
            }
            self.stable.truncate(index);
            let lines = Arc::new(lines);
            let (_, wrap_map) = build_wrap_map(&lines, width);
            let visual_rows = wrap_map.last().map(|entry| entry.visual_end).unwrap_or(0);
            self.stable.push(MarkdownLineChunk {
                identity,
                lines,
                wrap_map: Arc::new(wrap_map),
                visual_rows,
            });
        }
        self.stable.truncate(chunk_count);
        self.prefix.clear();
        self.prefix.push(0);
        for chunk in &self.stable {
            self.prefix.push(
                self.prefix
                    .last()
                    .copied()
                    .unwrap()
                    .saturating_add(chunk.lines.len()),
            );
        }
    }

    pub(crate) fn stable_overlay(&self) -> Option<(usize, Vec<Arc<Vec<Line<'static>>>>)> {
        Some((
            self.stable_start?,
            self.stable
                .iter()
                .map(|chunk| Arc::clone(&chunk.lines))
                .collect(),
        ))
    }

    pub(crate) fn build_slot_wrap_map(
        &self,
        lines: &[Line<'static>],
        width: u16,
    ) -> (usize, Vec<WrappedLineInfo>) {
        let Some(stable_start) = self.stable_start.filter(|_| !self.stable.is_empty()) else {
            return build_wrap_map(lines, width);
        };
        let stable_len = self
            .stable
            .iter()
            .map(|chunk| chunk.lines.len())
            .sum::<usize>();
        if stable_start.saturating_add(stable_len) > lines.len() {
            return build_wrap_map(lines, width);
        }
        let (mut visual_rows, mut result) = build_wrap_map(&lines[..stable_start], width);
        let mut logical_offset = stable_start;
        for chunk in &self.stable {
            // 实际绘制宽度可能在 tracker 收敛帧与 grid 宽度不同。
            // 稳定区的主 lines 只有占位行，必须从 chunk 正文重新测量。
            let measured;
            let (chunk_rows, chunk_map) = if self.width == width {
                (chunk.visual_rows, chunk.wrap_map.as_slice())
            } else {
                measured = build_wrap_map(&chunk.lines, width);
                (measured.0, measured.1.as_slice())
            };
            result.extend(chunk_map.iter().cloned().map(|mut entry| {
                entry.logical_idx += logical_offset;
                entry.visual_start += visual_rows;
                entry.visual_end += visual_rows;
                entry
            }));
            logical_offset += chunk.lines.len();
            visual_rows += chunk_rows;
        }
        let (tail_rows, tail_map) = build_wrap_map(&lines[logical_offset..], width);
        result.extend(tail_map.into_iter().map(|mut entry| {
            entry.logical_idx += logical_offset;
            entry.visual_start += visual_rows;
            entry.visual_end += visual_rows;
            entry
        }));
        (visual_rows + tail_rows, result)
    }

    fn retained_bytes(&self) -> usize {
        self.stable.capacity() * std::mem::size_of::<MarkdownLineChunk>()
            + self.prefix.capacity() * std::mem::size_of::<usize>()
            + self
                .stable
                .iter()
                .map(|chunk| {
                    retained_lines_bytes(&chunk.lines)
                        + chunk.wrap_map.capacity() * std::mem::size_of::<WrappedLineInfo>()
                })
                .sum::<usize>()
    }
}

#[cfg(test)]
#[path = "entry_render_cache/test.rs"]
mod tests;
