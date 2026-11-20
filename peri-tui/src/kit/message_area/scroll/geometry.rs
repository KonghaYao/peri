use std::time::Instant;

use crate::kit::message_area::props::ScrollbarFields;
use ratatui_kit::ratatui::layout::Rect;

// ── 滚动条拖拽状态 ────────────────────────────────────────────────────────

/// 滚动条 thumb 拖拽状态。
/// `thumb_offset` 是按下时鼠标 row 相对 thumb 顶部的偏移，拖动期间锁定——
/// 让「点击 thumb 中央 → 拖拽」时 thumb 不跳变。
#[derive(Debug, Clone, Copy)]
pub(in crate::kit::message_area) struct ScrollbarDragState {
    pub(in crate::kit::message_area) active: bool,
    pub(in crate::kit::message_area) thumb_offset: u16,
    pub(in crate::kit::message_area) last_flush: Instant,
}

impl Default for ScrollbarDragState {
    fn default() -> Self {
        Self {
            active: false,
            thumb_offset: 0,
            last_flush: peri_time::monotonic_now(),
        }
    }
}

// ── 滚动条几何 + 反推 ─────────────────────────────────────────────────────

/// 判断鼠标列是否落在滚动条列（drawer.area 最右 1 列）。
#[must_use]
pub(in crate::kit::message_area) fn is_scrollbar_column(mouse_col: u16, area: Rect) -> bool {
    mouse_col == area.x.saturating_add(area.width).saturating_sub(1)
}

/// ratatui Scrollbar 渲染所需的几何参数。源自 ratatui-widgets 0.3.2 的公式：
/// - `track_length = area.height - 2`（去掉 ▲▼）
/// - `thumb_length = round(viewport * track / max_viewport_position).clamp(1, track)`
/// - `thumb_start  = round(position * track / max_viewport_position).clamp(0, track - thumb)`
#[derive(Debug, Clone, Copy)]
pub(super) struct ThumbGeometry {
    pub(super) track_length: usize,
    pub(super) thumb_length: usize,
    pub(super) thumb_start: usize,
    pub(super) max_position: usize,
    pub(super) max_viewport_position: usize,
}

/// 四舍五入除法（与 ratatui 内部 `rounding_divide` 一致）。
pub(super) fn round_divide(numerator: usize, denominator: usize) -> usize {
    (numerator + denominator / 2)
        .checked_div(denominator)
        .unwrap_or(0)
}

/// 从 ScrollbarFields + area 计算 thumb 几何。无溢出时返回 None（滚动条不渲染）。
pub(super) fn compute_thumb_geometry(
    fields: &ScrollbarFields,
    area: Rect,
) -> Option<ThumbGeometry> {
    if fields.content_length <= fields.viewport_length {
        return None;
    }
    let track_length = area.height.saturating_sub(2) as usize;
    if track_length == 0 {
        return None;
    }
    let max_position = fields.content_length.saturating_sub(1);
    let max_viewport_position = max_position + fields.viewport_length;
    let thumb_length = round_divide(fields.viewport_length * track_length, max_viewport_position)
        .clamp(1, track_length);
    let thumb_start = round_divide(fields.position * track_length, max_viewport_position)
        .min(track_length.saturating_sub(thumb_length));
    Some(ThumbGeometry {
        track_length,
        thumb_length,
        thumb_start,
        max_position,
        max_viewport_position,
    })
}

/// 把「目标 thumb_start（track 内偏移）」反推为「scroll position」。
///
/// 可移动轨道两端显式映射到 0 / max_position：ratatui 的正向公式在末端饱和——
/// position=883 与 999 都画在最后一格，线性反推只能得到 883（→ 偏移 876）。
/// 中间格仍用正向公式的逆运算（ceil 由 `position_to_scroll_y` 负责）。
pub(super) fn thumb_start_to_position(thumb_start: usize, geo: &ThumbGeometry) -> usize {
    let span = geo.track_length.saturating_sub(geo.thumb_length);
    if span == 0 {
        return 0;
    }
    let clamped = thumb_start.min(span);
    if clamped == 0 {
        return 0;
    }
    if clamped == span {
        return geo.max_position;
    }
    round_divide(clamped * geo.max_viewport_position, geo.track_length).min(geo.max_position)
}

/// 滚动条按下命中的语义：端点跳转、拖块抓取或空轨跳转。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrollbarPress {
    /// ▲（area 顶行）：跳到顶部。
    Top,
    /// ▼（area 底行）：跳到底部（调用方写入 max_scroll）。
    Bottom,
    /// 拖块：抓取偏移，按下时不跳转。
    Thumb { grab: u16 },
    /// 空轨：thumb 中心对齐鼠标后的目标偏移。
    Track { offset: usize },
}

/// 按下滚动条的命中判定（渲染、事件与测试共用同一份几何）。
pub(super) fn scrollbar_press(
    mouse_row: u16,
    area: Rect,
    geo: &ThumbGeometry,
    max_scroll: usize,
) -> ScrollbarPress {
    let bottom_row = area.y.saturating_add(area.height).saturating_sub(1);
    if mouse_row == area.y {
        return ScrollbarPress::Top;
    }
    if mouse_row == bottom_row {
        return ScrollbarPress::Bottom;
    }
    let thumb_start_row = area
        .y
        .saturating_add(1)
        .saturating_add(geo.thumb_start as u16);
    let thumb_end_row = thumb_start_row.saturating_add(geo.thumb_length as u16);
    if mouse_row >= thumb_start_row && mouse_row < thumb_end_row {
        return ScrollbarPress::Thumb {
            grab: mouse_row.saturating_sub(thumb_start_row),
        };
    }
    let track_click = mouse_row.saturating_sub(area.y).saturating_sub(1) as usize;
    let target_thumb_start = track_click.saturating_sub(geo.thumb_length / 2);
    let position = thumb_start_to_position(target_thumb_start, geo);
    ScrollbarPress::Track {
        offset: position_to_scroll_y(position, geo.max_position, max_scroll),
    }
}

/// 拖拽中的目标偏移：`grab` 是按下时鼠标相对 thumb 顶部的偏移（锁定后不跳变）。
/// 鼠标移出滚动条列只影响命中归属，坐标换算仍以轨道 row 为准。
pub(super) fn scrollbar_drag_offset(
    mouse_row: u16,
    grab: u16,
    area: Rect,
    geo: &ThumbGeometry,
    max_scroll: usize,
) -> usize {
    let new_thumb_start_row = mouse_row.saturating_sub(grab);
    let target_track_click = new_thumb_start_row.saturating_sub(area.y).saturating_sub(1) as usize;
    let position = thumb_start_to_position(target_track_click, geo);
    position_to_scroll_y(position, geo.max_position, max_scroll)
}

/// 把 ratatui 语义的「scroll position」反推为「scroll offset（scroll_state.offset().y）」。
/// [Why] mod.rs 写入 `scrollbar_fields.position` 时做了线性映射
/// `position = scroll_y * max_position / max_scroll`（修复 ratatui thumb 不到底）。
/// 反推必须用相同公式的逆运算，否则点击位置和实际滚动错位——比如点击底部时
/// set_offset 会超出 max_scroll 范围。
///
/// [Why ceil] 正向映射用的是 floor（整数除法），反推用 ceil 才是正确的逆运算：
/// floor(a*b/c) 的逆运算是 ceil(x*c/b)。用 floor 做反推会导致 thumb 拖到接近底部
/// 时（如 99%）无法滚动到最后一行——只有恰好拖到 100% 位置才能到底。
pub(super) fn position_to_scroll_y(
    position: usize,
    max_position: usize,
    max_scroll: usize,
) -> usize {
    if max_position == 0 || max_scroll == 0 {
        0
    } else {
        (position * max_scroll).div_ceil(max_position)
    }
}
