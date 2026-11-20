use std::mem::Discriminant;
use std::sync::Arc;

use peri_theme::theme::ThemeDefinition;
use ratatui_kit::ratatui::text::Line;

use crate::kit::genui::GenUiButtonInfo;
use crate::kit::markdown::MarkdownRenderCache;
use crate::kit::message_area::grid::{GridSpec, ZoneTransition};
use crate::kit::message_area::render::CopyButtonInfo;
use crate::kit::message_area::render::ImageLineInfo;
use crate::kit::message_area::render::InteractionLayout;
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
    transition: Option<ZoneTransition>,
    context: Option<EntryRenderContext>,
    frame: u64,
    markdown: MarkdownRenderCache,
    markdown_lines: MarkdownLineCache,
    lines: Arc<Vec<Line<'static>>>,
    copy_button: Option<CopyButtonInfo>,
    interaction: Option<InteractionLayout>,
    image_lines: Vec<ImageLineInfo>,
    genui_buttons: Vec<GenUiButtonInfo>,
    genui_revisions: Vec<(crate::kit::genui::GenUiBlockId, u64)>,
    /// 上次渲染时的 Mermaid 预览版本；只有含 Mermaid 的消息需要据此重算。
    preview_revision: Option<u64>,
    has_mermaid: Option<bool>,
    /// 本消息内每处围栏的逻辑行范围；调度与阅读锚点共用。
    mermaid_fences: Vec<crate::kit::mermaid::FenceSpan>,
    /// 展开的正文空档组：成员标题逻辑行 → 成员 FoldKey（单击热区复用）。
    fold_hits: Arc<Vec<(usize, crate::kit::tui_render_unit::FoldKey)>>,
}

impl EntryRenderCache {
    pub(crate) fn ensure(
        &mut self,
        vm: &TuiRenderUnit,
        grid: &GridSpec,
        context: EntryRenderContext,
        frame: u64,
        transition: ZoneTransition,
    ) -> EntryInvalidation {
        let variant = std::mem::discriminant(vm);
        let identity_changed = self.variant != Some(variant)
            || self.context.as_ref().is_some_and(|previous| {
                previous.occurrence != context.occurrence || previous.surface != context.surface
            });
        let content_changed = identity_changed || self.hash != Some(vm.content_hash());
        if content_changed || self.has_mermaid.is_none() {
            self.has_mermaid = Some(crate::kit::mermaid::vm_might_have_mermaid(vm));
        }
        let preview_revision = crate::kit::mermaid::message_revision_for_vm(vm);
        let preview_changed =
            self.has_mermaid == Some(true) && self.preview_revision != preview_revision;
        // 转场翻转（相邻 entry 的起止 zone 变化，§6）改变本 slot 的入口台阶行——
        // 按 Layout 级重渲染本 slot，不牵动其他 slot 的缓存。
        let layout_changed = self.grid != Some(*grid)
            || self.transition != Some(transition)
            || self.context.as_ref().is_none_or(|previous| {
                previous.language != context.language
                    || !Arc::ptr_eq(&previous.theme, &context.theme)
            })
            || self.genui_state_changed()
            || preview_changed;
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
                    // 入口台阶（§6）占 lines[0] 时 running header 在 lines[1]——
                    // 按台阶字形判定，替换目标不因转场形态漂移。
                    && let Some(header_index) = crate::kit::message_area::render::running_header_index(
                        &self.lines,
                    )
                    && let Some(header) = Arc::make_mut(&mut self.lines).get_mut(header_index)
                {
                    *header = crate::kit::message_area::render::render_running_reasoning_header(
                        reasoning, grid,
                    );
                }
            } else {
                let (
                    lines,
                    copy_button,
                    interaction,
                    image_lines,
                    genui_buttons,
                    mermaid_fences,
                    fold_hits,
                ) = if context.surface == EntrySurface::Message {
                    crate::kit::message_area::render::vm_to_lines_cached_with_layout_gated(
                        vm,
                        grid,
                        &mut self.markdown,
                        None,
                        true,
                        transition,
                    )
                } else {
                    crate::kit::message_area::render::vm_to_lines_cached_with_layout_scheduled(
                        vm,
                        grid,
                        &mut self.markdown,
                        None,
                        false,
                        transition,
                    )
                };
                self.lines = Arc::new(lines);
                self.copy_button = copy_button;
                self.interaction = interaction;
                self.image_lines = image_lines;
                self.genui_buttons = genui_buttons;
                self.mermaid_fences = mermaid_fences;
                self.fold_hits = Arc::new(fold_hits);
            }
        } else {
            self.markdown_lines.stable_start = None;
            let (
                lines,
                copy_button,
                interaction,
                image_lines,
                genui_buttons,
                mermaid_fences,
                fold_hits,
            ) = if context.surface == EntrySurface::Message {
                crate::kit::message_area::render::vm_to_lines_cached_with_layout_gated(
                    vm,
                    grid,
                    &mut self.markdown,
                    Some(&mut self.markdown_lines),
                    true,
                    transition,
                )
            } else {
                crate::kit::message_area::render::vm_to_lines_cached_with_layout_scheduled(
                    vm,
                    grid,
                    &mut self.markdown,
                    Some(&mut self.markdown_lines),
                    false,
                    transition,
                )
            };
            self.lines = Arc::new(lines);
            self.copy_button = copy_button;
            self.interaction = interaction;
            self.image_lines = image_lines;
            self.genui_buttons = genui_buttons;
            self.mermaid_fences = mermaid_fences;
            self.fold_hits = Arc::new(fold_hits);
        }
        self.genui_revisions = self
            .markdown_lines
            .genui_blocks
            .iter()
            .copied()
            .chain(self.genui_buttons.iter().filter_map(|button| button.block))
            .map(|id| (id, crate::kit::genui::view_state::block_revision(id)))
            .collect();
        self.hash = Some(vm.content_hash());
        self.variant = Some(variant);
        self.grid = Some(*grid);
        self.transition = Some(transition);
        self.context = Some(context);
        self.frame = frame;
        self.preview_revision = preview_revision;
        invalidation
    }

    #[must_use]
    pub(crate) fn preview_revision(&self) -> Option<u64> {
        self.preview_revision
    }

    #[must_use]
    pub(crate) fn has_mermaid(&self) -> Option<bool> {
        self.has_mermaid
    }

    #[must_use]
    pub(crate) fn mermaid_fences(&self) -> &[crate::kit::mermaid::FenceSpan] {
        &self.mermaid_fences
    }

    /// 展开组的成员标题命中表（非组槽位恒为空）。
    #[must_use]
    pub(crate) fn fold_hits(&self) -> &Arc<Vec<(usize, crate::kit::tui_render_unit::FoldKey)>> {
        &self.fold_hits
    }

    #[must_use]
    pub(crate) fn lines(&self) -> &Arc<Vec<Line<'static>>> {
        &self.lines
    }

    /// 上次渲染所用的过程区转场形态——transcript 据此判断转场翻转（§6）。
    pub(crate) fn transition(&self) -> Option<ZoneTransition> {
        self.transition
    }

    #[must_use]
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

    #[must_use]
    pub(crate) fn image_lines(&self) -> &[ImageLineInfo] {
        &self.image_lines
    }

    pub(crate) fn genui_state_changed(&self) -> bool {
        self.genui_revisions
            .iter()
            .any(|&(id, revision)| crate::kit::genui::view_state::block_revision(id) != revision)
    }

    pub(crate) fn genui_buttons(&self) -> &[GenUiButtonInfo] {
        &self.genui_buttons
    }

    pub(crate) fn build_wrap_map(&self, width: u16) -> (usize, Vec<WrappedLineInfo>) {
        self.markdown_lines.build_slot_wrap_map(&self.lines, width)
    }

    pub(crate) fn evict(&mut self) {
        // 冷历史仍保留小型版本键，状态改变后才能修正其高度，避免滚动索引沿用旧布局。
        let genui_revisions = std::mem::take(&mut self.genui_revisions);
        *self = Self::default();
        self.genui_revisions = genui_revisions;
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        let mut seen = crate::kit::retained_bytes::SeenAllocations::default();
        self.retained_bytes_with(&mut seen)
    }

    pub(crate) fn retained_bytes_with(
        &self,
        seen: &mut crate::kit::retained_bytes::SeenAllocations,
    ) -> usize {
        self.markdown.retained_bytes()
            + crate::kit::retained_bytes::retained_arc_lines_bytes(&self.lines, seen)
            + self.markdown_lines.retained_bytes_with(seen)
            + self.genui_revisions.capacity()
                * std::mem::size_of::<(crate::kit::genui::GenUiBlockId, u64)>()
            + self.markdown_lines.genui_blocks.capacity()
                * std::mem::size_of::<crate::kit::genui::GenUiBlockId>()
            + self.genui_buttons.capacity() * std::mem::size_of::<GenUiButtonInfo>()
            + self
                .genui_buttons
                .iter()
                .map(|button| {
                    button.label.capacity()
                        + button.action.retained_bytes()
                        + button.parsed.as_ref().map_or(0, |parsed| {
                            seen.account_arc_with(parsed, || parsed.retained_bytes())
                        })
                })
                .sum::<usize>()
            + self.image_lines.capacity() * std::mem::size_of::<ImageLineInfo>()
            + self
                .image_lines
                .iter()
                .map(|image| image.path.capacity() + image.size_text.capacity())
                .sum::<usize>()
            + self.fold_hits.capacity()
                * std::mem::size_of::<(usize, crate::kit::tui_render_unit::FoldKey)>()
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

#[derive(Clone)]
pub(crate) struct MarkdownLineChunk {
    pub(crate) identity: usize,
    pub(crate) lines: Arc<Vec<Line<'static>>>,
    pub(crate) wrap_map: Arc<Vec<WrappedLineInfo>>,
    pub(crate) visual_rows: usize,
    /// 本 chunk 行内的 Mermaid 围栏（局部逻辑行范围）。
    fences: Vec<crate::kit::mermaid::FenceSpan>,
}

#[derive(Clone, Default)]
pub(crate) struct MarkdownLineCache {
    pub(crate) width: u16,
    pub(crate) stable_start: Option<usize>,
    pub(crate) stable: Vec<MarkdownLineChunk>,
    pub(crate) genui_ranges: Vec<std::ops::Range<usize>>,
    pub(crate) genui_blocks: Vec<crate::kit::genui::GenUiBlockId>,
    prefix: Vec<usize>,
}

impl MarkdownLineCache {
    #[cfg(test)]
    pub(crate) fn retain_and_wrap(&mut self, width: u16, chunks: Vec<(usize, Vec<Line<'static>>)>) {
        let chunks = chunks
            .into_iter()
            .map(|(identity, lines)| (identity, lines, Vec::new()))
            .collect();
        self.retain_and_wrap_with_fences(width, chunks);
    }

    /// 稳定块重建时同时保留其中围栏的局部行范围；块命中缓存时范围随块复用。
    pub(crate) fn retain_and_wrap_with_fences(
        &mut self,
        width: u16,
        chunks: Vec<(
            usize,
            Vec<Line<'static>>,
            Vec<crate::kit::mermaid::FenceSpan>,
        )>,
    ) {
        if self.width != width {
            self.stable.clear();
            self.width = width;
        }
        let chunk_count = chunks.len();
        {
            let iteration_items = chunks.into_iter().enumerate();
            for (index, (identity, lines, fences)) in iteration_items {
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
                    fences,
                });
            }
        }
        self.stable.truncate(chunk_count);
        self.prefix.clear();
        self.prefix.push(0);
        for chunk in &self.stable {
            self.prefix.push(
                self.prefix
                    .last()
                    .copied()
                    .expect("the prefix table starts with zero before chunk lengths are appended")
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

    /// 稳定块里的围栏，逻辑行范围换算到整条消息。
    #[must_use]
    pub(crate) fn stable_fences(&self) -> Vec<crate::kit::mermaid::FenceSpan> {
        let Some(stable_start) = self.stable_start else {
            return Vec::new();
        };
        let mut fences = Vec::new();
        for (index, chunk) in self.stable.iter().enumerate() {
            let Some(chunk_start) = self.prefix.get(index) else {
                continue;
            };
            let offset = stable_start.saturating_add(*chunk_start);
            fences.extend(chunk.fences.iter().map(|fence| fence.shifted(offset)));
        }
        fences
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

    fn retained_bytes_with(&self, seen: &mut crate::kit::retained_bytes::SeenAllocations) -> usize {
        self.stable.capacity() * std::mem::size_of::<MarkdownLineChunk>()
            + self.prefix.capacity() * std::mem::size_of::<usize>()
            + self.genui_ranges.capacity() * std::mem::size_of::<std::ops::Range<usize>>()
            + self
                .stable
                .iter()
                .map(|chunk| {
                    crate::kit::retained_bytes::retained_arc_lines_bytes(&chunk.lines, seen)
                        + chunk.wrap_map.capacity() * std::mem::size_of::<WrappedLineInfo>()
                })
                .sum::<usize>()
    }
}

#[cfg(test)]
#[path = "entry_render_cache/test.rs"]
mod tests;
