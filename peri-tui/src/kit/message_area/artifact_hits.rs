//! Entry 渲染副产物的命中几何——主时间线与 SubAgent 详情面板共用的唯一实现。
//!
//! 副产物（GenUI 按钮、Mermaid「查看图」入口）由 `vm_to_lines_with_options`
//! 随 entry 行一起产出；这里只把「逻辑行 → 视觉行/列」的换算收敛成纯函数，
//! 两个 surface 各自用同一份 `wrap_map` 生成屏幕命中，不各自复制折行口径。

use super::grid::GridSpec;
use super::selection::{SlotLines, WrappedLineInfo};
use crate::kit::entry_render_cache::EntryRenderCache;
use crate::kit::mermaid::FenceSpan;
use peri_tui_components::genui::GenUiButtonInfo;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

/// 按逻辑行读取 entry 行——主区 `SlotLines`（稳定块复合）与面板
/// `EntryRenderCache`（stable overlay）各自的读取口径由实现承担。
pub(crate) trait LogicalLines {
    fn logical_line(&self, logical: usize) -> Option<&Line<'static>>;
}

impl LogicalLines for SlotLines {
    fn logical_line(&self, logical: usize) -> Option<&Line<'static>> {
        self.line(logical)
    }
}

impl LogicalLines for EntryRenderCache {
    fn logical_line(&self, logical: usize) -> Option<&Line<'static>> {
        self.line(logical)
    }
}

/// GenUI 按钮热区：`(视觉行, 列起, 列止)`，行相对所属 slot 起点。
///
/// 按钮可跨折行；每段续行减去该视觉行在逻辑行内的起始列，与选择器共用
/// `wrap_row_byte_starts` 的折行边界，resize 校正帧不会把热区留在旧列。
pub(crate) fn genui_button_rows(
    button: &GenUiButtonInfo,
    lines: &dyn LogicalLines,
    wrap_map: &[WrappedLineInfo],
    width: u16,
) -> Vec<(usize, u16, u16)> {
    let mut rows = Vec::new();
    for logical in button.line_start..button.line_end {
        let Some(entry) = wrap_map.get(logical) else {
            continue;
        };
        let Some(line) = lines.logical_line(logical) else {
            continue;
        };
        let plain = line.to_string();
        let starts = peri_tui_components::selection::wrap_row_byte_starts(line, &plain, width);
        for (offset, bytes) in starts.windows(2).enumerate() {
            let Some(before) = plain.get(..bytes[0]) else {
                continue;
            };
            let Some(text) = plain.get(bytes[0]..bytes[1]) else {
                continue;
            };
            let col = before.width();
            let start = (button.x_start as usize)
                .saturating_sub(col)
                .min(width as usize);
            let end = (button.x_end as usize)
                .saturating_sub(col)
                .min(text.width())
                .min(width as usize);
            if start < end {
                rows.push((
                    entry.visual_start.saturating_add(offset),
                    start as u16,
                    end as u16,
                ));
            }
        }
    }
    rows
}

/// Mermaid「查看图」动作行的视觉行——围栏逻辑范围的最后一行。
///
/// 动作行由 `display_lines` 追加在围栏末尾；源回退/占位/失败时仍然存在，
/// 因此它是面板与主区共同的唯一打开入口。
pub(crate) fn mermaid_action_visual(
    span: &FenceSpan,
    wrap_map: &[WrappedLineInfo],
) -> Option<usize> {
    let action = span.logical.end.checked_sub(1)?;
    if action < span.logical.start {
        return None;
    }
    wrap_map.get(action).map(|entry| entry.visual_start)
}

/// 动作行的横向命中列——从正文前缀到视口右缘（不含滚动条列）。
pub(crate) fn mermaid_action_columns(grid: GridSpec, area: Rect) -> (u16, u16) {
    let x_start = area
        .x
        .saturating_add(grid.cont_prefix_width().min(u16::MAX as usize) as u16)
        .min(area.x.saturating_add(area.width.saturating_sub(1)));
    let x_end = area.x.saturating_add(area.width.saturating_sub(1));
    (x_start, x_end)
}
