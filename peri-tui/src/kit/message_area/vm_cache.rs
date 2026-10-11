#[cfg(test)]
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;

use super::render;
use super::render::ImageLineInfo;
use super::scroll;
use super::selection::WrappedLineInfo;
pub(super) fn read_render_snapshot() -> (
    crate::kit::atoms::ViewModelsSnapshot,
    crate::kit::atoms::TranscriptPublication,
) {
    let wait_start = peri_time::monotonic_now();
    let view_models = crate::kit::atoms::VIEW_MODELS.state();
    let guard = view_models.read();
    let acquired = peri_time::monotonic_now();
    let snapshot = guard.clone();
    let publication = crate::kit::atoms::TRANSCRIPT_PUBLICATION.get();
    drop(guard);
    let released = peri_time::monotonic_now();
    trace_elapsed("vm-read-wait", acquired.duration_since(wait_start), None);
    trace_elapsed(
        "vm-snapshot",
        released.duration_since(acquired),
        Some("persistent-clone-and-publication"),
    );
    (snapshot, publication)
}

/// 计算 palette 中影响 markdown 渲染的关键字段哈希。
/// 当主题切换时，hash 变化 → 触发 vm_caches 重建 → markdown 色值更新。
#[cfg(test)]
pub(super) fn palette_markdown_key(
    p: &ratatui_kit::prelude::Palette,
    surface_sunken: ratatui_kit::ratatui::style::Color,
    markdown_text: ratatui_kit::ratatui::style::Color,
) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.fg.hash(&mut h);
    p.bg.hash(&mut h);
    p.fg_dim.hash(&mut h);
    p.accent.hash(&mut h);
    p.surface.hash(&mut h);
    surface_sunken.hash(&mut h);
    markdown_text.hash(&mut h);
    p.border.hash(&mut h);
    p.success.hash(&mut h);
    p.warning.hash(&mut h);
    p.error.hash(&mut h);
    p.info.hash(&mut h);
    h.finish()
}

/// 计算可滚动内容的视觉高度。
///
/// 视觉行索引和滚动偏移均为 `usize`；仅终端几何坐标保留 `u16`，避免长消息在
/// 65,535 行处截断而无法滚到底部。
pub(super) fn total_visual_rows(core_rows: usize, footer_rows: usize, is_loading: bool) -> usize {
    if core_rows == 0 && footer_rows == 0 {
        usize::from(is_loading)
    } else {
        core_rows
            .saturating_add(footer_rows)
            .saturating_add(scroll::SCROLL_PADDING)
    }
}

// ── 渲染性能诊断（PERI_RENDER_TIMING=1 启用）──────────────────────────────

pub(super) fn render_timing_enabled() -> bool {
    thread_local! {
        static ENABLED: bool = std::env::var("PERI_RENDER_TIMING")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
    }
    ENABLED.with(|&e| e)
}

/// 如果启用诊断，打印阶段耗时。
#[track_caller]
pub(super) fn trace_phase(phase: &str, start: Instant, detail: Option<&str>) {
    trace_elapsed(phase, peri_time::elapsed_since(start), detail);
}

pub(super) fn trace_elapsed(phase: &str, elapsed: std::time::Duration, detail: Option<&str>) {
    if render_timing_enabled() {
        tracing::info!(target: "perf.render", phase, elapsed_us = elapsed.as_micros() as u64,
            process_id = std::process::id(), thread = ?std::thread::current().id(),
            detail = detail.unwrap_or_default(), "message-render-phase");
    }
}

// ── 按 VM 分片的渲染缓存 ──────────────────────────────────────────────────
//
// [Why] 旧版 lines_cache / wrap_map_cache / total_rows_cache 以 (vm_generation, width)
// 为 key，但 push_view_models 每个 token 都 generation += 1，流式期间缓存永远不命中，
// 每个 token 都触发 O(N×W) 的全量 markdown 解析 + wrap_map 重建 + line_count → CPU 拉满。
//
// 现在按 VM 的 content_hash 分片：只有正在流式（hash 变化）的那个 VM 重新解析 markdown
// + 重建 wrap_map，其余 VM 直接 Arc::clone 复用。流式单次成本从 O(N×W) 降至 O(W)。
//
// content_hash 由 build_view_models / TuiAssistantBubble::recompute_hash 维护，
// 已覆盖 text / reasoning.text / reasoning.collapsed / tool duration(secs) 等可变字段。
pub(crate) use crate::kit::entry_render_cache::MarkdownLineCache;

#[derive(Default)]
pub(super) struct VmCacheSlot {
    pub(super) content_hash: u64,
    pub(super) variant: Option<std::mem::Discriminant<crate::kit::tui_render_unit::TuiRenderUnit>>,
    pub(super) entry: crate::kit::entry_render_cache::EntryRenderCache,
    pub(super) lines: Option<Arc<super::selection::SlotLines>>,
    pub(super) wrap_map: Arc<Vec<WrappedLineInfo>>,
    pub(super) wrap_width: u16,
    pub(super) visual_rows: usize,
    pub(super) copy_button: Option<render::CopyButtonInfo>,
    pub(super) interaction: Option<render::InteractionLayout>,
    pub(super) image_lines: Vec<ImageLineInfo>,
}

impl VmCacheSlot {
    pub(super) fn matches_content(&self, vm: &crate::kit::tui_render_unit::TuiRenderUnit) -> bool {
        self.variant == Some(std::mem::discriminant(vm)) && self.content_hash == vm.content_hash()
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.entry.retained_bytes()
            + self
                .lines
                .as_ref()
                .map_or(0, |lines| lines.retained_metadata_bytes())
            + self.wrap_map.capacity() * std::mem::size_of::<WrappedLineInfo>()
            + self.interaction.as_ref().map_or(0, |layout| {
                layout.option_rows.capacity() * std::mem::size_of::<usize>()
                    + layout.option_cols.capacity() * std::mem::size_of::<Option<(u16, u16)>>()
            })
            + self.image_lines.capacity() * std::mem::size_of::<ImageLineInfo>()
            + self
                .image_lines
                .iter()
                .map(|image| image.path.capacity() + image.size_text.capacity())
                .sum::<usize>()
    }

    pub(super) fn evict(&mut self) {
        self.entry.evict();
        self.lines = None;
        self.wrap_map = Arc::default();
        self.copy_button = None;
        self.interaction = None;
        self.image_lines = Vec::new();
    }
}

#[test]
#[serial_test::serial]
fn test_markdown_line_cache_reuses_stable_wrap_and_rebuilds_tail_only() {
    use crate::kit::acp_bridge::{perf_counters, reset_perf_counters};
    use ratatui_kit::ratatui::text::Line;

    let stable = vec![Line::from("stable 中文 line")];
    let identity = 7usize;
    let mut cache = MarkdownLineCache::default();
    cache.retain_and_wrap(20, vec![(identity, stable.clone())]);
    let stable_lines = Arc::clone(&cache.stable[0].lines);
    cache.stable_start = Some(1);

    let mut slot_lines = vec![Line::from("chrome")];
    slot_lines.extend(stable);
    slot_lines.push(Line::from("mutable tail"));
    reset_perf_counters();
    let (_, map) = cache.build_slot_wrap_map(&slot_lines, 20);
    let counters = perf_counters();

    assert!(Arc::ptr_eq(&stable_lines, &cache.stable[0].lines));
    assert_eq!(map.len(), slot_lines.len());
    assert_eq!(counters.wrap_recalculated_lines, 2);
}

#[test]
fn test_markdown_line_cache_width_invalidates_stable_wrap() {
    use ratatui_kit::ratatui::text::Line;

    let mut cache = MarkdownLineCache::default();
    cache.retain_and_wrap(20, vec![(1, vec![Line::from("long stable line")])]);
    let first = Arc::clone(&cache.stable[0].wrap_map);
    cache.retain_and_wrap(8, vec![(1, vec![Line::from("long stable line")])]);
    assert!(!Arc::ptr_eq(&first, &cache.stable[0].wrap_map));
}

/// 回归：宽度变化清理 stable 后，空 lines 的 chunk 不得越界索引。
///
/// 旧实现先 `truncate(index)` 再读 `self.stable[index]`（len <= index 恒越界）；
/// 终端 resize 会清空 stable 并让全部 chunk 重建，该组合必 panic
/// （`index out of bounds: the len is 0 but the index is 0`）。
#[test]
fn test_markdown_line_cache_empty_chunk_after_width_change_does_not_panic() {
    use ratatui_kit::ratatui::text::Line;

    let mut cache = MarkdownLineCache::default();
    cache.retain_and_wrap(20, vec![(1, vec![Line::from("stable line")])]);

    cache.retain_and_wrap(8, vec![(1, Vec::new())]);

    assert_eq!(cache.stable.len(), 1);
    assert!(cache.stable[0].lines.is_empty());
}

/// 命中占位（同宽度 + 同 identity + 空 lines）保留缓存内容，不重建为空。
#[test]
fn test_markdown_line_cache_hit_with_empty_placeholder_keeps_cached_lines() {
    use ratatui_kit::ratatui::text::Line;

    let mut cache = MarkdownLineCache::default();
    cache.retain_and_wrap(20, vec![(7, vec![Line::from("stable line")])]);

    cache.retain_and_wrap(20, vec![(7, Vec::new())]);

    assert_eq!(cache.stable.len(), 1);
    assert_eq!(cache.stable[0].lines.len(), 1);
}

#[test]
fn test_markdown_key_tracks_text_color_and_code_background() {
    use ratatui_kit::ratatui::style::Color;

    let palette = ratatui_kit::prelude::Palette::default();
    let original = palette_markdown_key(&palette, Color::Black, Color::White);
    assert_ne!(
        original,
        palette_markdown_key(&palette, Color::Black, Color::Green)
    );
    assert_ne!(
        original,
        palette_markdown_key(&palette, Color::Blue, Color::White)
    );
    assert_eq!(
        original,
        palette_markdown_key(&palette, Color::Black, Color::White)
    );
}
