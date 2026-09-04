use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;

use super::render;
use super::render::ImageLineInfo;
use super::scroll;
use super::selection::{WrappedLineInfo, build_wrap_map};
use ratatui_kit::ratatui::text::Line;

/// 计算 palette 中影响 markdown 渲染的关键字段哈希。
/// 当主题切换时，hash 变化 → 触发 vm_caches 重建 → markdown 色值更新。
pub(super) fn palette_markdown_key(
    p: &ratatui_kit::prelude::Palette,
    surface_sunken: ratatui_kit::ratatui::style::Color,
) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.fg.hash(&mut h);
    p.bg.hash(&mut h);
    p.fg_dim.hash(&mut h);
    p.accent.hash(&mut h);
    p.surface.hash(&mut h);
    surface_sunken.hash(&mut h);
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
    if render_timing_enabled() {
        let elapsed_us = start.elapsed().as_micros();
        let extra = detail.map(|d| format!(" | {d}")).unwrap_or_default();
        tracing::info!(target: "perf.render", "[{phase}] {elapsed_us}μs{extra}");
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
#[derive(Clone)]
pub(super) struct MarkdownLineChunk {
    pub(super) identity: usize,
    pub(super) lines: Arc<Vec<Line<'static>>>,
    pub(super) wrap_map: Arc<Vec<WrappedLineInfo>>,
    pub(super) visual_rows: usize,
}

#[derive(Clone, Default)]
pub(super) struct MarkdownLineCache {
    pub(super) width: u16,
    pub(super) stable_start: Option<usize>,
    pub(super) stable: Vec<MarkdownLineChunk>,
}

impl MarkdownLineCache {
    pub(super) fn retain_and_wrap(&mut self, width: u16, chunks: &[(usize, Vec<Line<'static>>)]) {
        if self.width != width {
            self.stable.clear();
            self.width = width;
        }
        for (index, (identity, lines)) in chunks.iter().enumerate() {
            if self
                .stable
                .get(index)
                .is_some_and(|cached| cached.identity == *identity)
            {
                continue;
            }
            self.stable.truncate(index);
            let lines = if lines.is_empty() {
                Arc::clone(&self.stable[index].lines)
            } else {
                Arc::new(lines.clone())
            };
            let (_, wrap_map) = build_wrap_map(&lines, width);
            let visual_rows = wrap_map.last().map(|entry| entry.visual_end).unwrap_or(0);
            self.stable.push(MarkdownLineChunk {
                identity: *identity,
                lines,
                wrap_map: Arc::new(wrap_map),
                visual_rows,
            });
        }
        self.stable.truncate(chunks.len());
    }

    pub(super) fn stable_overlay(&self) -> Option<(usize, Vec<Arc<Vec<Line<'static>>>>)> {
        Some((
            self.stable_start?,
            self.stable
                .iter()
                .map(|chunk| Arc::clone(&chunk.lines))
                .collect(),
        ))
    }

    pub(super) fn build_slot_wrap_map(
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
            result.extend(chunk.wrap_map.iter().cloned().map(|mut entry| {
                entry.logical_idx += logical_offset;
                entry.visual_start += visual_rows;
                entry.visual_end += visual_rows;
                entry
            }));
            logical_offset += chunk.lines.len();
            visual_rows += chunk.visual_rows;
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
}

#[derive(Clone, Default)]
pub(super) struct VmCacheSlot {
    /// 上次渲染时 VM 的 content_hash。变化时（流式追加 text、折叠/展开 reasoning、
    /// tool duration 跨秒）触发 markdown 重新解析 + wrap_map 重建。
    pub(super) content_hash: u64,
    /// 上次渲染时的视宽。width 变化（窗口 resize）时 wrap 规则改变，必须重建。
    pub(super) width: u16,
    /// 上次渲染时的 palette 关键字段哈希。主题切换时 hash 变化 → 强制重建所有 VM 的 markdown 渲染。
    pub(super) palette_key: u64,
    /// 上次渲染时的 LANG_VERSION。语言切换时递增 → 强制重建（md 复制按钮文本依赖 i18n）。
    pub(super) lang_key: u64,
    /// 该 VM 解析后的所有 Line（markdown + reasoning + tool card 渲染结果）。
    pub(super) lines: Arc<Vec<Line<'static>>>,
    /// 该 VM 内部 wrap_map（visual_row 从 0 起）。拼接时累加 visual_offset 和 logical_idx 偏移。
    pub(super) wrap_map: Arc<Vec<WrappedLineInfo>>,
    /// 该 VM 占据的视觉行数（= wrap_map 末项 visual_end）。
    pub(super) visual_rows: usize,
    /// [Phase 2] markdown 增量渲染缓存——按文本前缀复用 stable_state，仅处理新增 block。
    /// 仅 AssistantBubble / UserBubble 实际使用；其他 VM 类型保留默认值不消耗资源。
    pub(super) markdown_cache: crate::kit::markdown::MarkdownRenderCache,
    /// Phase D：stable rendered chunk 对应的 Line/wrap identity cache。
    pub(super) markdown_lines: MarkdownLineCache,
    /// md 复制按钮布局（slot 内逻辑索引 + 列范围）。None = 该 VM 无按钮
    /// （非 AssistantBubble / 空文本 / 宽度不足）。rebuild 时随 lines 重建。
    pub(super) copy_button: Option<render::CopyButtonInfo>,
    /// [Slice 4 §6.8] pending interaction block 的选项行布局（slot 内逻辑行
    /// 与列区间）。None = 非 pending interaction。rebuild 时随 lines 重建，
    /// 供视口 post-pass 应用「当前项」高亮与点击热区。
    pub(super) interaction: Option<render::InteractionLayout>,
    /// [T4 §4] @image 行渲染期信息（slot 内逻辑索引 + 展示路径 + 受管理标志）。
    /// rebuild 时随 lines 重建，供点击/hover 屏幕命中映射。
    pub(super) image_lines: Vec<ImageLineInfo>,
    /// 上次渲染时的动画帧（§8.2 壁钟 tick，100ms 粒度）。running 类 VM
    /// （tool/subagent/reasoning）帧变化时强制重建——braille 动画随帧推进。
    pub(super) anim_frame: u64,
}

#[test]
#[serial_test::serial]
fn test_markdown_line_cache_reuses_stable_wrap_and_rebuilds_tail_only() {
    use crate::kit::acp_bridge::{perf_counters, reset_perf_counters};
    use ratatui_kit::ratatui::text::Line;

    let stable = vec![Line::from("stable 中文 line")];
    let identity = 7usize;
    let mut cache = MarkdownLineCache::default();
    cache.retain_and_wrap(20, &[(identity, stable.clone())]);
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
    cache.retain_and_wrap(20, &[(1, vec![Line::from("long stable line")])]);
    let first = Arc::clone(&cache.stable[0].wrap_map);
    cache.retain_and_wrap(8, &[(1, vec![Line::from("long stable line")])]);
    assert!(!Arc::ptr_eq(&first, &cache.stable[0].wrap_map));
}
