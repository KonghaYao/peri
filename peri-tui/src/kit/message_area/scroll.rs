//! 滚动节流 + 鼠标事件处理 + 吸底自动跟随。

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::kit::focus_router;
use crate::kit::text_selection::TextSelection;
use ratatui_kit::components::ScrollViewState;
use ratatui_kit::crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::{Position, Rect};

use super::props::{ScrollbarFields, mouse_in_area};
use super::selection::{
    WrappedLineInfo, copy_to_clipboard, extract_visual_range, mark_copy_message,
};

// ── 滚动速度控制 ──────────────────────────────────────────────────────────

/// 鼠标滚轮每格的滚动行数倍数。
pub(super) const SCROLL_LINES: u16 = 3;

/// 滚动节流窗口：≥16ms（≈60fps）才把累积 delta 推入 scroll_state。
pub(super) const SCROLL_FRAME_MS: u64 = 16;

#[derive(Debug, Clone)]
pub(super) struct ScrollThrottle {
    pub(super) last_flush: Instant,
    pub(super) pending_delta: i32, // positive = scroll_down, negative = scroll_up
}

impl Default for ScrollThrottle {
    fn default() -> Self {
        Self {
            last_flush: Instant::now(),
            pending_delta: 0,
        }
    }
}

// ── 拖拽选中节流 ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(super) struct DragThrottle {
    pub(super) last_flush: Instant,
}

impl Default for DragThrottle {
    fn default() -> Self {
        Self {
            last_flush: Instant::now(),
        }
    }
}

// ── 滚动条拖拽状态 ────────────────────────────────────────────────────────

/// 滚动条 thumb 拖拽状态。
/// `thumb_offset` 是按下时鼠标 row 相对 thumb 顶部的偏移，拖动期间锁定——
/// 让「点击 thumb 中央 → 拖拽」时 thumb 不跳变。
#[derive(Debug, Clone, Copy)]
pub(super) struct ScrollbarDragState {
    pub(super) active: bool,
    pub(super) thumb_offset: u16,
    pub(super) last_flush: Instant,
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
fn is_scrollbar_column(mouse_col: u16, area: Rect) -> bool {
    mouse_col == area.x.saturating_add(area.width).saturating_sub(1)
}

/// ratatui Scrollbar 渲染所需的几何参数。源自 ratatui-widgets 0.3.2 的公式：
/// - `track_length = area.height - 2`（去掉 ▲▼）
/// - `thumb_length = round(viewport * track / max_viewport_position).clamp(1, track)`
/// - `thumb_start  = round(position * track / max_viewport_position).clamp(0, track - thumb)`
#[derive(Debug, Clone, Copy)]
struct ThumbGeometry {
    track_length: usize,
    thumb_length: usize,
    thumb_start: usize,
    max_position: usize,
    max_viewport_position: usize,
}

/// 四舍五入除法（与 ratatui 内部 `rounding_divide` 一致）。
fn round_divide(numerator: usize, denominator: usize) -> usize {
    if denominator == 0 {
        0
    } else {
        (numerator + denominator / 2) / denominator
    }
}

/// 从 ScrollbarFields + area 计算 thumb 几何。无溢出时返回 None（滚动条不渲染）。
fn compute_thumb_geometry(fields: &ScrollbarFields, area: Rect) -> Option<ThumbGeometry> {
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
fn thumb_start_to_position(thumb_start: usize, geo: &ThumbGeometry) -> usize {
    let clamped = thumb_start.min(geo.track_length.saturating_sub(geo.thumb_length));
    round_divide(clamped * geo.max_viewport_position, geo.track_length).min(geo.max_position)
}

/// 把 ratatui 语义的「scroll position」反推为「scroll offset（scroll_state.offset().y）」。
/// [Why] mod.rs 写入 `scrollbar_fields.position` 时做了线性映射
/// `position = scroll_y * max_position / max_scroll`（修复 ratatui thumb 不到底）。
/// 反推必须用相同公式的逆运算，否则点击位置和实际滚动错位——比如点击底部时
/// set_offset 会超出 max_scroll 范围。
fn position_to_scroll_y(position: usize, max_position: usize, max_scroll: usize) -> usize {
    if max_position == 0 || max_scroll == 0 {
        0
    } else {
        (position * max_scroll) / max_position
    }
}

// ── 滚动节流（私有）────────────────────────────────────────────────────

/// 滚动节流：累积 delta，仅在距上次 flush ≥ SCROLL_FRAME_MS(16ms) 时推入 scroll_state。
/// write_no_update 不触发 notifier.wake()——依赖 dispatch 后 ratatui-kit loop 强制 render。
fn apply_scroll(
    delta: i32,
    scroll_throttle: &State<ScrollThrottle>,
    scroll_state: &State<ScrollViewState>,
) {
    let mut st = scroll_throttle.write_no_update();
    st.pending_delta += delta;
    let now = Instant::now();
    if now.duration_since(st.last_flush) >= Duration::from_millis(SCROLL_FRAME_MS) {
        let pending = st.pending_delta;
        st.pending_delta = 0;
        st.last_flush = now;
        drop(st);
        if pending != 0 {
            let mut state = scroll_state.write_no_update();
            if pending > 0 {
                for _ in 0..(pending as u16) {
                    state.scroll_down();
                }
            } else {
                for _ in 0..((-pending) as u16) {
                    state.scroll_up();
                }
            }
        }
    }
}

// ── 鼠标事件处理 ─────────────────────────────────────────────────────────

/// 从 `use_event_handler` 闭包提取的鼠标/键盘处理逻辑。
/// 包含：鼠标滚轮节流、文本拖拽选中（Down/Drag/Up）、键盘滚动、
/// PERI_DISABLE_DRAG_SELECT 分支、parking_lot 死锁规避。
pub(super) fn handle_event(
    event: &Event,
    area_rect: Option<Rect>,
    vis_width: u16,
    scroll_state: &State<ScrollViewState>,
    scroll_throttle: &State<ScrollThrottle>,
    text_sel: &State<TextSelection>,
    selection_down_pos: &State<Option<(u16, u16)>>,
    drag_throttle: &State<DragThrottle>,
    wrap_map_cache: &State<(u64, u16, Arc<Vec<WrappedLineInfo>>)>,
    lines_cache: &State<(
        u64,
        usize,
        Arc<Vec<ratatui_kit::ratatui::text::Line<'static>>>,
    )>,
    scrollbar_fields: &State<ScrollbarFields>,
    scrollbar_drag: &State<ScrollbarDragState>,
) -> EventResult {
    if let Event::Key(key) = &event {
        let _ = focus_router::message_accepts_key(key);
    }

    if let Event::Mouse(mouse) = &event {
        // 光标移动无操作——提前返回，不触发任何 state 写入或渲染
        if matches!(mouse.kind, MouseEventKind::Moved) {
            return EventResult::Ignored;
        }

        if let Some(area) = area_rect {
            let in_area = mouse_in_area(mouse.row, mouse.column, area);

            // ── 滚动条点击/拖拽（优先于文本选区）──
            // [Why] 滚动条列（drawer.area 最右 1 列）以前 fallthrough 到文本选区，
            // 导致点击轨道触发空复制、▲▼ 无反应、拖拽 thumb 无反应。
            // 拖拽中（drag_active）即使鼠标移出滚动条列也继续滚动条逻辑——
            // 否则会 fallthrough 到文本选区，触发误复制。
            let on_scrollbar_col = in_area && is_scrollbar_column(mouse.column, area);
            let drag_active = scrollbar_drag.read().active;
            if drag_active || on_scrollbar_col {
                let fields = *scrollbar_fields.read();
                if let Some(geo) = compute_thumb_geometry(&fields, area) {
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) if on_scrollbar_col => {
                            // ▲ 端点（area 顶行）—— 直接跳到顶部
                            if mouse.row == area.y {
                                let cur = scroll_state.read().offset();
                                scroll_state
                                    .write_no_update()
                                    .set_offset(Position::new(cur.x, 0));
                                return EventResult::Consumed;
                            }
                            // ▼ 端点（area 底行）—— 直接跳到底部
                            let bottom_row = area.y.saturating_add(area.height).saturating_sub(1);
                            if mouse.row == bottom_row {
                                let max_scroll =
                                    fields.content_length.saturating_sub(fields.viewport_length);
                                let cur = scroll_state.read().offset();
                                scroll_state
                                    .write_no_update()
                                    .set_offset(Position::new(cur.x, max_scroll as u16));
                                return EventResult::Consumed;
                            }
                            // thumb / track
                            let thumb_start_row = area
                                .y
                                .saturating_add(1)
                                .saturating_add(geo.thumb_start as u16);
                            let thumb_end_row =
                                thumb_start_row.saturating_add(geo.thumb_length as u16);
                            let on_thumb =
                                mouse.row >= thumb_start_row && mouse.row < thumb_end_row;
                            // 点击 thumb：锁定 thumb_offset、不跳转；
                            // 点击轨道：thumb 中心对齐鼠标并立即跳转
                            let thumb_offset = if on_thumb {
                                mouse.row.saturating_sub(thumb_start_row)
                            } else {
                                (geo.thumb_length / 2) as u16
                            };
                            if !on_thumb {
                                let track_click =
                                    mouse.row.saturating_sub(area.y).saturating_sub(1) as usize;
                                let target_thumb_start =
                                    track_click.saturating_sub(geo.thumb_length / 2);
                                let position = thumb_start_to_position(target_thumb_start, &geo);
                                let max_scroll =
                                    fields.content_length.saturating_sub(fields.viewport_length);
                                let target =
                                    position_to_scroll_y(position, geo.max_position, max_scroll);
                                let cur = scroll_state.read().offset();
                                scroll_state
                                    .write_no_update()
                                    .set_offset(Position::new(cur.x, target as u16));
                            }
                            // 记录拖拽状态——render 不依赖 active，用 write_no_update
                            {
                                let mut s = scrollbar_drag.write_no_update();
                                s.active = true;
                                s.thumb_offset = thumb_offset;
                                s.last_flush = Instant::now();
                            }
                            // 清除文本选区按下记录，防止 fallthrough 冲突
                            *selection_down_pos.write_no_update() = None;
                            return EventResult::Consumed;
                        }
                        MouseEventKind::Drag(MouseButton::Left) if drag_active => {
                            // 16ms 节流——和滚轮 / 文本 Drag 保持一致
                            let now = Instant::now();
                            {
                                let d = scrollbar_drag.read();
                                if now.duration_since(d.last_flush)
                                    < Duration::from_millis(SCROLL_FRAME_MS)
                                {
                                    return EventResult::Consumed;
                                }
                            }
                            scrollbar_drag.write_no_update().last_flush = now;
                            // 保持 thumb_offset：new_thumb_start_row = mouse.row - thumb_offset
                            let thumb_offset = scrollbar_drag.read().thumb_offset;
                            let new_thumb_start_row = mouse.row.saturating_sub(thumb_offset);
                            let target_track_click =
                                new_thumb_start_row.saturating_sub(area.y).saturating_sub(1)
                                    as usize;
                            let position = thumb_start_to_position(target_track_click, &geo);
                            let max_scroll =
                                fields.content_length.saturating_sub(fields.viewport_length);
                            let target =
                                position_to_scroll_y(position, geo.max_position, max_scroll);
                            let cur = scroll_state.read().offset();
                            scroll_state
                                .write_no_update()
                                .set_offset(Position::new(cur.x, target as u16));
                            return EventResult::Consumed;
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            if drag_active {
                                scrollbar_drag.write_no_update().active = false;
                            }
                            return EventResult::Consumed;
                        }
                        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                            // 滚轮在滚动条列也响应——fallthrough 到下面的滚动处理
                        }
                        _ => return EventResult::Consumed,
                    }
                } else if on_scrollbar_col
                    && matches!(
                        mouse.kind,
                        MouseEventKind::Down(MouseButton::Left)
                            | MouseEventKind::Drag(MouseButton::Left)
                            | MouseEventKind::Up(MouseButton::Left)
                    )
                {
                    // 无溢出（geo=None，ScrollbarHook 也不渲染），但鼠标在滚动条列——
                    // 消费事件避免 fallthrough 到文本选区（否则点击最右列会清空选区）
                    return EventResult::Consumed;
                }
            }

            // [DEBUG] PERI_DISABLE_DRAG_SELECT=1 完全跳过 Drag 选中——验证是否是
            // Drag 处理逻辑引起卡死。
            let drag_select_disabled = std::env::var("PERI_DISABLE_DRAG_SELECT")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);

            // ── 文本选中处理（消息区内 Down/Drag/Up）──
            if in_area && !drag_select_disabled {
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        // 仅记录按下的位置，不启动选区——真实拖动才开始选中
                        // [TRAP] selection_down_pos 只在事件处理器内读写，render
                        // 不依赖它——用 write_no_update 避免 wake 噪音（render 不需要
                        // 因为这个状态变化而重渲染，后续 Drag 才是真正的渲染触发点）。
                        let scroll_y = scroll_state.read().offset().y as u16;
                        let visual_row = mouse.row.saturating_sub(area.y).saturating_add(scroll_y);
                        // 视口裁剪后无边框，visual_col 直接 = mouse.column - area.x
                        let visual_col = mouse.column.saturating_sub(area.x);
                        *selection_down_pos.write_no_update() = Some((visual_row, visual_col));
                        return EventResult::Consumed;
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        // Drag 节流（16ms），write_no_update 避免自激回路
                        let now = Instant::now();
                        {
                            let dt = drag_throttle.read();
                            if dt.last_flush.elapsed() < Duration::from_millis(SCROLL_FRAME_MS) {
                                return EventResult::Consumed;
                            }
                        }
                        drag_throttle.write_no_update().last_flush = now;

                        let scroll_y = scroll_state.read().offset().y as u16;
                        let visual_row = mouse.row.saturating_sub(area.y).saturating_add(scroll_y);
                        let visual_col = mouse.column.saturating_sub(area.x);

                        // 单次 write guard，drop 时只 wake 一次（不是两次）
                        // start_drag + update_drag 合并到同一 guard 内
                        //
                        // [TRAP] ratatui-kit 用 parking_lot::RwLock——同一 thread 同时
                        // 持有 read + write 时 try_write 返回 Err → expect panic。
                        // 必须先把 selection_down_pos.read() 的值 copy 出来 drop guard，
                        // 再 write selection_down_pos。
                        let down_pos = *selection_down_pos.read();
                        {
                            let mut sel_guard = text_sel.write();
                            if let Some((dr, dc)) = down_pos {
                                sel_guard.start_drag(dr, dc);
                                *selection_down_pos.write_no_update() = None;
                            }
                            sel_guard.update_drag(visual_row, visual_col);
                        }
                        return EventResult::Consumed;
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        *selection_down_pos.write_no_update() = None;
                        // [TRAP] 同 Drag 处理：必须 copy 出 text_sel 状态后再 write，
                        // 否则 read+write 同 thread 冲突 panic。
                        let dragging = text_sel.read().dragging;
                        if !dragging {
                            return EventResult::Consumed;
                        }
                        // 先 copy 出 normalized_bounds（owned Option），drop read guard
                        let bounds = text_sel.read().normalized_bounds();
                        let extracted: Option<String> = if let Some(((sr, sc), (er, ec))) = bounds {
                            let wrap_guard = wrap_map_cache.read();
                            let lines_guard = lines_cache.read();
                            extract_visual_range(
                                &lines_guard.2,
                                &wrap_guard.2,
                                (sr, sc),
                                (er, ec),
                                vis_width,
                            )
                        } else {
                            None
                        };

                        // 清除选区（start/end/dragging 全清），wake 触发重渲染清除 highlight
                        {
                            let mut sel = text_sel.write();
                            sel.clear();
                        }

                        // 复制（独立线程，不阻塞）
                        if let Some(text) = extracted {
                            let char_count = text.chars().count();
                            copy_to_clipboard(text);
                            mark_copy_message(char_count);
                        }

                        return EventResult::Consumed;
                    }
                    _ => {}
                }
            } else {
                // 鼠标在消息区外
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        // 清除选区和按下记录
                        *text_sel.write() = TextSelection::new();
                        *selection_down_pos.write_no_update() = None;
                        return EventResult::Ignored;
                    }
                    _ => {
                        if matches!(mouse.kind, MouseEventKind::Drag(_)) {
                            return EventResult::Ignored;
                        }
                    }
                }
            }

            // ── 滚动处理（区域内外通用）──
            match mouse.kind {
                MouseEventKind::ScrollDown => {
                    apply_scroll(SCROLL_LINES as i32, scroll_throttle, scroll_state)
                }
                MouseEventKind::ScrollUp => {
                    apply_scroll(-(SCROLL_LINES as i32), scroll_throttle, scroll_state)
                }
                _ => {}
            }
        }

        // 所有非 Moved/Drag 鼠标事件标记为已消费（防止泄漏到下层组件）
        return EventResult::Consumed;
    }

    if let Event::Key(key) = &event {
        if key.kind == KeyEventKind::Press && focus_router::message_accepts_key(key) {
            scroll_state.write().handle_event(&event);
            return EventResult::Consumed;
        }
    }
    EventResult::Ignored
}

// ── 吸底自动跟随 ─────────────────────────────────────────────────────────

/// `use_effect` 闭包提取的上下文结构体。
/// 所有 `State<T>` 字段在 mod.rs 闭包外构造时用 `.clone()`（State 是 Arc，clone 是廉价引用拷贝）。
pub(super) struct AutoFollowCtx {
    pub total_visual_rows: u16,
    pub vis_height: u16,
    pub scroll_state: State<ScrollViewState>,
    pub prev_items_len: State<usize>,
    pub last_scrolled_at: State<u16>,
    pub items_len: usize,
    pub is_loading: bool,
    /// 用于检测 resize：total_visual_rows 变化后钳制 scroll_state.offset 到有效范围。
    pub prev_total_visual_rows: State<u16>,
}

/// 从 `use_effect` 闭包提取的吸底逻辑。
/// 注意：use_effect body 不是 render body，所以 `write()` 是正确的（需要 wake 触发后续渲染）。
pub(super) fn run_auto_follow(ctx: &AutoFollowCtx) {
    // [Fix] resize 后 total_visual_rows 变化时，主动钳制 scroll_state.offset 到有效范围。
    // 避免 scroll_state.offset.y >> max_scroll 导致用户感知滚动完全卡死。
    let prev_total = *ctx.prev_total_visual_rows.read();
    *ctx.prev_total_visual_rows.write() = ctx.total_visual_rows;

    if prev_total != ctx.total_visual_rows && ctx.total_visual_rows > 0 && ctx.vis_height > 0 {
        let max_scroll = ctx.total_visual_rows.saturating_sub(ctx.vis_height);
        let current_y = ctx.scroll_state.read().offset().y as u16;
        if current_y > max_scroll {
            ctx.scroll_state
                .write()
                .set_offset(ratatui_kit::ratatui::layout::Position::new(0, max_scroll));
        }
    }

    // [TRAP] parking_lot 同 thread 死锁规避：先 read copy 出 owned，guard 在语句末尾 drop，再 write。
    let prev = *ctx.prev_items_len.read();
    *ctx.prev_items_len.write() = ctx.items_len;

    if ctx.total_visual_rows == 0 || ctx.vis_height == 0 {
        return;
    }

    if ctx.is_loading {
        // [TRAP] read+write 同 state 同线程 = parking_lot 死锁——先 copy 出来
        let prev_lsa = *ctx.last_scrolled_at.read();
        if ctx.total_visual_rows > prev_lsa {
            let max_scroll = ctx.total_visual_rows.saturating_sub(ctx.vis_height);
            let scroll_y = ctx.scroll_state.read().offset().y as u16;
            let distance = max_scroll.saturating_sub(scroll_y);
            // [Bug] 仅在用户当前接近底部时跟随——用户主动上滚浏览历史时不应被吸回。
            // 阈值与非 loading 分支保持一致（vis_height/4，至少 5 行）。
            let threshold = (ctx.vis_height / 4).max(5);
            if distance <= threshold {
                ctx.scroll_state.write().scroll_to_bottom();
                *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
            }
            // 用户上滚超过阈值（distance > threshold）时不抢夺滚动位——last_scrolled_at
            // 也不更新，让下次 effect 重新检测；用户回到接近底部后自然恢复跟随。
        }
        return;
    }

    if ctx.items_len < prev {
        ctx.scroll_state.write().scroll_to_bottom();
        *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
        return;
    }

    let max_scroll = ctx.total_visual_rows.saturating_sub(ctx.vis_height);
    let scroll_y = ctx.scroll_state.read().offset().y as u16;
    if scroll_y >= max_scroll {
        return;
    }
    let distance = max_scroll.saturating_sub(scroll_y);
    if distance > (ctx.vis_height / 4).max(5) {
        return;
    }
    let prev_lsa = *ctx.last_scrolled_at.read();
    if ctx.total_visual_rows > prev_lsa {
        ctx.scroll_state.write().scroll_to_bottom();
        *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
    }
}

// ── 测试 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn proximity_check(total: u16, scroll_y: u16, vis_height: u16) -> bool {
        if total == 0 {
            return false;
        }
        let max_scroll = total.saturating_sub(vis_height);
        if scroll_y >= max_scroll {
            return false;
        }
        let distance = max_scroll.saturating_sub(scroll_y);
        let threshold = (vis_height / 2).max(5);
        distance <= threshold
    }

    #[test]
    fn test_proximity_at_bottom_should_not_trigger_scroll() {
        let total = 100;
        let vis_height = 20;
        let scroll_y = total - vis_height;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_within_half_viewport_should_follow() {
        let total = 100;
        let vis_height = 20;
        let scroll_y = total - vis_height - 10;
        assert!(proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_beyond_half_viewport_should_not_follow() {
        let total = 100;
        let vis_height = 20;
        let scroll_y = total - vis_height - 11;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_near_top_should_not_follow() {
        let total = 200;
        let vis_height = 30;
        let scroll_y = 20;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_small_viewport_minimum_threshold() {
        let total = 50;
        let vis_height = 6;
        let scroll_y = total - vis_height - 5;
        assert!(proximity_check(total, scroll_y, vis_height));
        let scroll_y = total - vis_height - 6;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_empty_content_no_follow() {
        assert!(!proximity_check(0, 0, 20));
    }

    #[test]
    fn test_proximity_content_smaller_than_viewport_at_bottom() {
        let total = 10;
        let vis_height = 30;
        assert!(!proximity_check(total, 0, vis_height));
    }

    // ── 滚动条几何 / 反推公式测试 ────────────────────────────────────────

    fn make_rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
        Rect::new(x, y, width, height)
    }

    #[test]
    fn test_is_scrollbar_column_rightmost() {
        // area: x=10, width=80 → scrollbar 列 = 10 + 80 - 1 = 89
        let area = make_rect(10, 0, 80, 24);
        assert!(is_scrollbar_column(89, area));
    }

    #[test]
    fn test_is_scrollbar_column_text_area() {
        let area = make_rect(10, 0, 80, 24);
        assert!(!is_scrollbar_column(10, area));
        assert!(!is_scrollbar_column(50, area));
        assert!(!is_scrollbar_column(88, area));
    }

    #[test]
    fn test_compute_thumb_geometry_no_overflow_returns_none() {
        let fields = ScrollbarFields {
            content_length: 50,
            position: 0,
            viewport_length: 60,
        };
        let area = make_rect(0, 0, 80, 24);
        assert!(compute_thumb_geometry(&fields, area).is_none());
    }

    #[test]
    fn test_compute_thumb_geometry_thumb_length_clamped_to_one() {
        let fields = ScrollbarFields {
            content_length: 1000,
            position: 0,
            viewport_length: 10,
        };
        let area = make_rect(0, 0, 80, 24);
        let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
        assert!(geo.thumb_length >= 1);
        assert!(geo.thumb_length <= geo.track_length);
        // content=1000, viewport=10, track=22 → thumb ≈ round(10*22/1009) = round(0.218) = 0 → clamp 到 1
        assert_eq!(geo.thumb_length, 1);
    }

    #[test]
    fn test_compute_thumb_geometry_track_length_minus_two() {
        let fields = ScrollbarFields {
            content_length: 200,
            position: 100,
            viewport_length: 20,
        };
        let area = make_rect(0, 0, 80, 24);
        let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
        assert_eq!(geo.track_length, 22); // 24 - 2
    }

    #[test]
    fn test_thumb_start_to_position_at_top() {
        let fields = ScrollbarFields {
            content_length: 200,
            position: 100,
            viewport_length: 20,
        };
        let area = make_rect(0, 0, 80, 24);
        let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
        let pos = thumb_start_to_position(0, &geo);
        assert_eq!(pos, 0);
    }

    #[test]
    fn test_thumb_start_to_position_at_bottom() {
        let fields = ScrollbarFields {
            content_length: 200,
            position: 0,
            viewport_length: 20,
        };
        let area = make_rect(0, 0, 80, 24);
        let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
        let max_thumb_start = geo.track_length - geo.thumb_length;
        let pos = thumb_start_to_position(max_thumb_start, &geo);
        // thumb 到底 → position 应接近 max_position（=199），允许 ±2 舍入误差
        assert!(
            pos >= geo.max_position.saturating_sub(2),
            "thumb 到底时 position 应接近 max_position，实际 = {}，max = {}",
            pos,
            geo.max_position
        );
    }

    #[test]
    fn test_thumb_start_to_position_clamps_to_max() {
        let fields = ScrollbarFields {
            content_length: 100,
            position: 0,
            viewport_length: 10,
        };
        let area = make_rect(0, 0, 80, 24);
        let geo = compute_thumb_geometry(&fields, area).expect("应有溢出");
        let pos = thumb_start_to_position(usize::MAX, &geo);
        assert_eq!(pos, geo.max_position);
    }

    #[test]
    fn test_round_divide_basic() {
        assert_eq!(round_divide(10, 4), 3); // (10+2)/4 = 3
        assert_eq!(round_divide(8, 4), 2); // (8+2)/4 = 2
        assert_eq!(round_divide(7, 0), 0);
        assert_eq!(round_divide(0, 5), 0);
    }

    #[test]
    fn test_scrollbar_drag_state_default() {
        let s = ScrollbarDragState::default();
        assert!(!s.active);
        assert_eq!(s.thumb_offset, 0);
    }

    #[test]
    fn test_position_to_scroll_y_linear() {
        // max_position=99, max_scroll=90（content=100, viewport=10）
        // position=0 → scroll_y=0
        assert_eq!(position_to_scroll_y(0, 99, 90), 0);
        // position=99 → scroll_y=99*90/99 = 90
        assert_eq!(position_to_scroll_y(99, 99, 90), 90);
        // position=50 → scroll_y=50*90/99 = 45（整数除法）
        assert_eq!(position_to_scroll_y(50, 99, 90), 45);
    }

    #[test]
    fn test_position_to_scroll_y_zero_max_position() {
        // max_position=0（content=1）→ 总是 0
        assert_eq!(position_to_scroll_y(0, 0, 0), 0);
        assert_eq!(position_to_scroll_y(5, 0, 10), 0);
    }

    #[test]
    fn test_position_to_scroll_y_zero_max_scroll() {
        // max_scroll=0（content <= viewport）→ 总是 0
        assert_eq!(position_to_scroll_y(5, 100, 0), 0);
    }
}
