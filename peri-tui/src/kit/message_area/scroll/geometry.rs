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
            last_flush: Instant::now(),
        }
    }
}

// ── 滚动条几何 + 反推 ─────────────────────────────────────────────────────

/// 判断鼠标列是否落在滚动条列（drawer.area 最右 1 列）。
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
/// clamp 到合法 thumb 范围，再线性反推 + clamp 到 [0, max_position]。
pub(super) fn thumb_start_to_position(thumb_start: usize, geo: &ThumbGeometry) -> usize {
    let clamped = thumb_start.min(geo.track_length.saturating_sub(geo.thumb_length));
    round_divide(clamped * geo.max_viewport_position, geo.track_length).min(geo.max_position)
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
