//! 面板滚轮仲裁——统一面板区滚轮语义，消除与消息区的 3:1 速度分裂。
//!
//! [Why] 面板的 ratatui-kit ScrollView（框架内置 handler，`Current+Normal`）
//! 滚轮固定 1 行/事件、无节流；消息区为 3 行/格 + 50ms 节流（`SCROLL_LINES` +
//! `scroll_fps`）。同一物理滚轮跨区域滚动速度断层 3 倍，且面板在 ghostty/ssh
//! 高频滚轮事件下每事件一次 draw。
//!
//! 本模块在 `Global+High`（Phase 1，先于 ScrollView 的层内 handler）拦截面板区
//! 滚轮：节流（复用消息区同一 `ScrollThrottle` 与 `scroll_fps` 配置）+
//! `SCROLL_LINES` 步长驱动面板的 `ScrollViewState`，`Consumed` 阻止框架
//! 1 行/事件的默认处理。
//!
//! ## 注册契约
//!
//! 面板渲染体每帧调用 `register_panel_scroll(s)` 覆盖式写入本帧槽位
//! （`kind` + 命中区域 + `State<ScrollViewState>`）。仲裁 handler 在事件分发时
//! （上一帧渲染完成后）读取；通过 `ACTIVE_PANEL == owner.kind` 校验句柄仍有效
//! （面板关闭/切换后 generational box 可能已释放，绝不驱动失效句柄）。
//!
//! ## 仲裁规则
//!
//! - 弹窗打开（`POPUP_KIND` 非空）→ Ignored（让路，与消息区 `is_occluded` 一致）
//! - 鼠标不在任何槽位区域 → Ignored（放行给消息区 / ScrollView 默认处理）
//! - 命中槽位 → 节流累积 + 按 `SCROLL_LINES` 驱动对应 `ScrollViewState` → Consumed
//! - 面板内容区内但未命中具体槽位（border/divider 列）→ Consumed（防双滚）

use std::time::{Duration, Instant};

use ratatui_kit::components::scroll_view::ScrollViewState;
use ratatui_kit::crossterm::event::{Event, MouseEventKind};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Rect;

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{ACTIVE_PANEL, PANEL_SCROLL_OWNER, PANEL_SCROLL_THROTTLE, POPUP_KIND};
use crate::kit::message_area::scroll::{SCROLL_LINES, scroll_frame_ms};

// ── 注册表 ──────────────────────────────────────────────────────────────

/// 面板滚动槽位：命中区域 + 外部滚动状态（面板渲染体注册，`State` 是 Copy）。
#[derive(Debug, Clone, Copy)]
pub struct PanelScrollSlot {
    pub area: Rect,
    pub state: State<ScrollViewState>,
}

/// 当前激活面板的滚动槽位集合（每帧覆盖写入）。
#[derive(Debug, Clone)]
pub struct PanelScrollOwner {
    pub kind: PanelKind,
    pub slots: Vec<PanelScrollSlot>,
}

/// 面板渲染体调用：覆盖式注册本帧滚动槽位。
/// 每帧刷新保证句柄指向最近一次渲染仍存活的面板状态；面板关闭后 `ACTIVE_PANEL`
/// 不再匹配，仲裁会放行且绝不触碰失效句柄。
pub fn register_panel_scrolls(kind: PanelKind, slots: Vec<PanelScrollSlot>) {
    *PANEL_SCROLL_OWNER.state().write_no_update() = Some(PanelScrollOwner { kind, slots });
}

/// 单槽位便捷版（绝大多数面板只有一个 ScrollView）。
pub fn register_panel_scroll(kind: PanelKind, area: Rect, state: State<ScrollViewState>) {
    register_panel_scrolls(kind, vec![PanelScrollSlot { area, state }]);
}

/// 双栏面板辅助：按百分比切分左右区域（divider 列并入右侧，误差 1 列可忽略）。
pub fn split_vertical(area: Rect, left_pct: u16) -> (Rect, Rect) {
    let left_w = area.width.saturating_mul(left_pct.min(100)) / 100;
    let left = Rect::new(area.x, area.y, left_w, area.height);
    let right = Rect::new(
        area.x.saturating_add(left_w),
        area.y,
        area.width.saturating_sub(left_w),
        area.height,
    );
    (left, right)
}

fn mouse_in_area(mouse_row: u16, mouse_col: u16, area: Rect) -> bool {
    let area_bottom = area.y.saturating_add(area.height);
    let area_right = area.x.saturating_add(area.width);
    mouse_row >= area.y && mouse_row < area_bottom && mouse_col >= area.x && mouse_col < area_right
}

// ── 仲裁 handler ────────────────────────────────────────────────────────

/// 面板滚轮仲裁（`Global+High`，由 PanelOverlay 挂载）。
/// 命中面板区滚轮 → 节流 + `SCROLL_LINES` 步长驱动 → Consumed。
pub fn handle_panel_scroll(event: &Event) -> EventResult {
    let Event::Mouse(mouse) = event else {
        return EventResult::Ignored;
    };
    if !matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        return EventResult::Ignored;
    }
    // 弹窗打开时让路（与消息区 is_occluded 一致）
    if POPUP_KIND.state().read().is_some() {
        return EventResult::Ignored;
    }
    let Some(owner) = PANEL_SCROLL_OWNER.state().read().clone() else {
        return EventResult::Ignored; // 无注册（面板未渲染/已关闭）→ 放行
    };
    // 面板必须仍激活——关闭/切换后句柄可能已失效（generational box 释放），绝不触碰
    if *ACTIVE_PANEL.state().read() != Some(owner.kind) {
        return EventResult::Ignored;
    }
    // 鼠标在面板内容区之外 → 放行（消息区或无人消费）
    let in_panel = owner
        .slots
        .iter()
        .any(|s| mouse_in_area(mouse.row, mouse.column, s.area));
    if !in_panel {
        return EventResult::Ignored;
    }
    let delta = match mouse.kind {
        MouseEventKind::ScrollDown => SCROLL_LINES as i32,
        MouseEventKind::ScrollUp => -(SCROLL_LINES as i32),
        _ => unreachable!(),
    };
    // 命中槽位驱动；未命中具体槽位（border/divider 列）仍 Consumed 防双滚
    if let Some(slot) = owner
        .slots
        .iter()
        .find(|s| mouse_in_area(mouse.row, mouse.column, s.area))
    {
        accumulate_and_flush(&slot.state, delta);
    }
    EventResult::Consumed
}

/// 渲染帧兜底 flush（PanelOverlay 渲染 body 调用）：ratatui-kit 无 tick，
/// 停手后残留 pending 在任意后续渲染帧落地（与消息区 flush_scroll_if_due 同语义）。
pub fn flush_panel_scroll_due() {
    let throttle = PANEL_SCROLL_THROTTLE.state();
    let mut st = throttle.write_no_update();
    if Instant::now().duration_since(st.last_flush) < Duration::from_millis(scroll_frame_ms()) {
        return;
    }
    let pending = st.pending_delta;
    st.pending_delta = 0;
    st.last_flush = Instant::now();
    drop(st);
    if let Some(owner) = PANEL_SCROLL_OWNER.state().read().clone()
        && *ACTIVE_PANEL.state().read() == Some(owner.kind)
        && let Some(slot) = owner.slots.first()
    {
        apply_pending_to_view(&mut slot.state.write_no_update(), pending);
    }
}

/// 节流累积 + 窗口到点即 flush（事件驱动路径）。
fn accumulate_and_flush(state: &State<ScrollViewState>, delta: i32) {
    let throttle = PANEL_SCROLL_THROTTLE.state();
    let mut st = throttle.write_no_update();
    st.pending_delta += delta;
    let now = Instant::now();
    if now.duration_since(st.last_flush) < Duration::from_millis(scroll_frame_ms()) {
        return;
    }
    let pending = st.pending_delta;
    st.pending_delta = 0;
    st.last_flush = now;
    drop(st);
    apply_pending_to_view(&mut state.write_no_update(), pending);
}

/// 把 pending 滚动量应用到 ScrollViewState（正=向下，负=向上；边界 clamp）。
/// 独立函数便于单测（构造 ScrollViewState 无需 State 句柄）。
fn apply_pending_to_view(scroll: &mut ScrollViewState, pending: i32) {
    if pending == 0 {
        return;
    }
    if pending > 0 {
        let mut n = pending as u16;
        while n > 0 && !scroll.is_at_bottom() {
            scroll.scroll_down();
            n -= 1;
        }
    } else {
        let mut n = (-pending) as u16;
        while n > 0 && scroll.offset().y > 0 {
            scroll.scroll_up();
            n -= 1;
        }
    }
}

// ── 测试 ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn test_split_vertical_percent() {
        let area = rect(10, 20, 100, 30);
        let (l, r) = split_vertical(area, 45);
        assert_eq!(l, rect(10, 20, 45, 30));
        assert_eq!(r, rect(55, 20, 55, 30));
        // 两半拼回原区域
        assert_eq!(l.width + r.width, area.width);
    }

    #[test]
    fn test_split_vertical_clamps_pct() {
        let area = rect(0, 0, 80, 10);
        let (l, r) = split_vertical(area, 120); // 超 100 → clamp
        assert_eq!(l.width, 80);
        assert_eq!(r.width, 0);
    }

    #[test]
    fn test_apply_pending_up_clamps_at_top() {
        let mut scroll =
            ScrollViewState::with_offset(ratatui_kit::ratatui::layout::Position::new(0, 3));
        apply_pending_to_view(&mut scroll, -99);
        assert_eq!(scroll.offset().y, 0);
        // 已在顶部：再向上 no-op
        apply_pending_to_view(&mut scroll, -1);
        assert_eq!(scroll.offset().y, 0);
    }

    #[test]
    fn test_apply_pending_zero_noop() {
        let mut scroll =
            ScrollViewState::with_offset(ratatui_kit::ratatui::layout::Position::new(0, 4));
        apply_pending_to_view(&mut scroll, 0);
        assert_eq!(scroll.offset().y, 4);
    }

    #[test]
    fn test_apply_pending_down_before_first_render_noop() {
        // 渲染前 size=None → is_at_bottom() 恒 true：向下驱动为 no-op，
        // 绝不越界（防御：句柄就绪但内容几何未知时静默丢弃）。
        let mut scroll = ScrollViewState::default();
        apply_pending_to_view(&mut scroll, 3);
        assert_eq!(scroll.offset().y, 0);
    }

    #[test]
    fn test_apply_pending_up_from_bottom_sentinel() {
        // scroll_to_bottom 在 size 未知时设 u16::MAX 哨兵；向上仍正常回滚
        let mut scroll = ScrollViewState::default();
        scroll.scroll_to_bottom();
        apply_pending_to_view(&mut scroll, -3);
        assert_eq!(scroll.offset().y, u16::MAX - 3);
    }

    #[test]
    fn test_mouse_hit_area() {
        let area = rect(10, 20, 60, 14);
        assert!(mouse_in_area(20, 10, area)); // 左上角
        assert!(mouse_in_area(33, 69, area)); // 右下角（exclusive）
        assert!(!mouse_in_area(19, 10, area)); // 上边界外
        assert!(!mouse_in_area(20, 9, area)); // 左边界外
        assert!(!mouse_in_area(34, 70, area)); // 右下外
    }
}
