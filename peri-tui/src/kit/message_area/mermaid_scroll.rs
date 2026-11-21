//! Mermaid 帧的横向滚动：底部条拖拽、Shift/横向滚轮与步长换算。
//!
//! 事件仲裁与消息区其它点击一致：注册在 scroll handler 之前，命中条/帧区域
//! 才 Consumed；普通聊天滚动与文本选择不受影响。

use std::sync::Arc;

use peri_tui_components::h_viewport::WHEEL_COLS;
use ratatui_kit::crossterm::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use ratatui_kit::prelude::*;

use super::hits::MermaidHScrollHit;
use crate::kit::markdown::MermaidFenceKey;
use crate::kit::mouse_router;

#[derive(Debug, Clone)]
pub(super) struct MermaidBarDrag {
    key: MermaidFenceKey,
    x_start: u16,
    bar: peri_tui_components::scrollbar::HorizontalBar,
    grab: usize,
    viewport: usize,
    max_offset: usize,
}

/// 定位入口：`set_h` 的末参数是**内容宽**，调用点统一由
/// `viewport + max_offset` 换算——把最大偏移直接当内容宽会让偏移恒被 clamp 到 0。
pub(super) fn set_position(
    key: &MermaidFenceKey,
    offset: usize,
    viewport: usize,
    max_offset: usize,
) -> bool {
    crate::kit::mermaid::set_h(key, offset, viewport, viewport.saturating_add(max_offset))
}

/// 相对平移入口；参数口径同 [`set_position`]。
pub(super) fn scroll_step(
    key: &MermaidFenceKey,
    delta: i32,
    viewport: usize,
    max_offset: usize,
) -> bool {
    crate::kit::mermaid::scroll_h(key, delta, viewport, viewport.saturating_add(max_offset))
}

/// 滚轮 → 横向步长：横向滚轮直接平移；纵向滚轮仅在 Shift 下平移。
pub(super) fn mermaid_wheel_delta(kind: MouseEventKind, shift: bool) -> Option<i32> {
    match kind {
        MouseEventKind::ScrollLeft => Some(-(WHEEL_COLS as i32)),
        MouseEventKind::ScrollRight => Some(WHEEL_COLS as i32),
        MouseEventKind::ScrollUp if shift => Some(-(WHEEL_COLS as i32)),
        MouseEventKind::ScrollDown if shift => Some(WHEEL_COLS as i32),
        _ => None,
    }
}

/// 方向键 → 横向步长；只在「查看图」入口被选中时由键盘 handler 调用。
pub(super) fn mermaid_arrow_delta(code: KeyCode) -> Option<i32> {
    match code {
        KeyCode::Left => Some(-(WHEEL_COLS as i32)),
        KeyCode::Right => Some(WHEEL_COLS as i32),
        _ => None,
    }
}

pub(super) fn register_mermaid_hscroll_events(
    hooks: &mut Hooks,
    hits: State<Arc<Vec<MermaidHScrollHit>>>,
    drag: State<Option<MermaidBarDrag>>,
    tick: State<u64>,
) {
    let hits_state = hits;
    let drag_state = drag;
    let tick_state = tick;
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        let Event::Mouse(mouse) = event else {
            return EventResult::Ignored;
        };
        if mouse_router::is_occluded() {
            return EventResult::Ignored;
        }
        // 终端丢失左键 release 后只剩移动事件：清掉悬空的拖拽态。
        if matches!(mouse.kind, MouseEventKind::Moved) {
            *drag_state.write_no_update() = None;
            return EventResult::Ignored;
        }
        if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
            return if drag_state.write_no_update().take().is_some() {
                EventResult::Consumed
            } else {
                EventResult::Ignored
            };
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            *drag_state.write_no_update() = None;
            let current = hits_state.read();
            let Some(hit) = current
                .iter()
                .find(|hit| hit.bar_contains(mouse.column, mouse.row))
            else {
                return EventResult::Ignored;
            };
            let Some(bar) = hit.bar else {
                return EventResult::Ignored;
            };
            let Some(cell) = hit.track_cell(mouse.column) else {
                return EventResult::Ignored;
            };
            // 按在 thumb 上抓取相对位置；按在轨道上让 thumb 以指针为中心跳过去。
            let (grab, offset) = match bar.grab(cell) {
                Some(grab) => (grab, None),
                None => {
                    let grab = bar.thumb_len / 2;
                    (grab, Some(bar.offset_for(cell.saturating_sub(grab))))
                }
            };
            *drag_state.write_no_update() = Some(MermaidBarDrag {
                key: hit.key.clone(),
                x_start: hit.x_start,
                bar,
                grab,
                viewport: hit.viewport,
                max_offset: hit.max_offset,
            });
            if let Some(offset) = offset
                && set_position(&hit.key, offset, hit.viewport, hit.max_offset)
            {
                // [TRAP] 先读后写：同一 parking_lot 状态的 read guard 不能跨 write。
                let next = tick_state.read().wrapping_add(1);
                *tick_state.write() = next;
            }
            return EventResult::Consumed;
        }
        if matches!(mouse.kind, MouseEventKind::Drag(MouseButton::Left)) {
            let active = drag_state.read().clone();
            let Some(active) = active else {
                return EventResult::Ignored;
            };
            let cell = usize::from(
                mouse
                    .column
                    .saturating_sub(active.x_start.saturating_add(1)),
            );
            let offset = active.bar.offset_for(cell.saturating_sub(active.grab));
            if set_position(&active.key, offset, active.viewport, active.max_offset) {
                let next = tick_state.read().wrapping_add(1);
                *tick_state.write() = next;
            }
            return EventResult::Consumed;
        }
        let Some(delta) =
            mermaid_wheel_delta(mouse.kind, mouse.modifiers.contains(KeyModifiers::SHIFT))
        else {
            return EventResult::Ignored;
        };
        let current = hits_state.read();
        let Some(hit) = current
            .iter()
            .find(|hit| hit.contains(mouse.column, mouse.row))
        else {
            return EventResult::Ignored;
        };
        if scroll_step(&hit.key, delta, hit.viewport, hit.max_offset) {
            let next = tick_state.read().wrapping_add(1);
            *tick_state.write() = next;
        }
        EventResult::Consumed
    });
}
