//! 滚动节流 + 鼠标事件处理 + 吸底自动跟随。

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::kit::focus_router;
use crate::kit::text_selection::TextSelection;
use ratatui_kit::components::ScrollViewState;
use ratatui_kit::crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Rect;

use super::props::mouse_in_area;
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
}

/// 从 `use_effect` 闭包提取的吸底逻辑。
/// 注意：use_effect body 不是 render body，所以 `write()` 是正确的（需要 wake 触发后续渲染）。
pub(super) fn run_auto_follow(ctx: &AutoFollowCtx) {
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
            if scroll_y < max_scroll {
                ctx.scroll_state.write().scroll_to_bottom();
            }
            *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
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
}
