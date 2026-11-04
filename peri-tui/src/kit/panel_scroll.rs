//! 面板滚轮仲裁——统一面板区滚轮语义，消除与消息区的速度分裂。
//!
//! [Why] 面板的 ratatui-kit ScrollView（框架内置 handler，`Current+Normal`）
//! 滚轮固定 1 行/事件；消息区为 `SCROLL_LINES` 行/事件。同一物理滚轮跨区域
//! 滚动速度断层 3 倍。
//!
//! 本模块在 `Global+High`（Phase 1，先于 ScrollView 的层内 handler）拦截面板区
//! 滚轮：每个事件立即按 `SCROLL_LINES` 步长驱动 ScrollView 或虚拟视口，`Consumed`
//! 阻止框架 1 行/事件的默认处理。逐事件落地（而非时间窗合并）保证一次物理刻度
//! 即使被终端拆成多个事件，步长也稳定。
//!
//! ## 注册契约
//!
//! 面板渲染体每帧调用 `register_panel_scroll(s)` 覆盖式写入本帧槽位
//! （`kind` + 命中区域 + 滚动状态）。虚拟视口注册完整 usize 高度与偏移，
//! 不使用 ScrollView 内容缓冲。仲裁 handler 在事件分发时
//! （上一帧渲染完成后）读取；通过 `ACTIVE_PANEL == owner.kind` 校验句柄仍有效
//! （面板关闭/切换后 generational box 可能已释放，绝不驱动失效句柄）。
//!
//! ## 仲裁规则
//!
//! - 弹窗打开（`POPUP_KIND` 非空）→ Ignored（让路，与消息区 `is_occluded` 一致）
//! - 鼠标不在任何槽位区域 → Ignored（放行给消息区 / ScrollView 默认处理）
//! - 命中槽位 → 按 `SCROLL_LINES` 立即驱动对应目标 → Consumed
//! - 面板内容区内但未命中具体槽位（border/divider 列）→ Consumed（防双滚）

use std::sync::atomic::{AtomicU64, Ordering};

use ratatui_kit::components::scroll_view::ScrollViewState;
use ratatui_kit::crossterm::event::{Event, MouseEventKind};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Rect;

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{ACTIVE_PANEL, PANEL_SCROLL_OWNER, POPUP_KIND};
use crate::kit::message_area::scroll::SCROLL_LINES;

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
    virtual_slot: Option<VirtualPanelScrollSlot>,
    generation: u64,
}

#[derive(Debug, Clone, Copy)]
struct VirtualPanelScrollSlot {
    area: Rect,
    offset: State<usize>,
    content_height: usize,
    viewport_height: usize,
}

#[derive(Clone, Copy)]
enum ScrollDriver {
    View(State<ScrollViewState>),
    Virtual(VirtualPanelScrollSlot),
}

impl ScrollDriver {
    fn try_apply(self, delta: i32) -> bool {
        match self {
            Self::View(state) => {
                let Some(mut scroll) = state.try_write_no_update() else {
                    return false;
                };
                apply_scroll_to_view(&mut scroll, delta);
            }
            Self::Virtual(slot) => {
                let Some(mut offset) = slot.offset.try_write_no_update() else {
                    return false;
                };
                apply_scroll_to_virtual(
                    &mut offset,
                    delta,
                    slot.content_height,
                    slot.viewport_height,
                );
            }
        }
        true
    }
}

impl PanelScrollOwner {
    fn driver(&self, index: usize) -> Option<ScrollDriver> {
        self.slots
            .get(index)
            .map(|slot| ScrollDriver::View(slot.state))
            .or_else(|| {
                self.virtual_slot
                    .filter(|_| index == self.slots.len())
                    .map(ScrollDriver::Virtual)
            })
    }

    fn same_handles(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.slots.len() == other.slots.len()
            && self.slots.iter().zip(&other.slots).all(|(previous, next)| {
                match (previous.state.try_read(), next.state.try_read()) {
                    (Some(previous), Some(next)) => std::ptr::eq(&*previous, &*next),
                    _ => false,
                }
            })
            && match (&self.virtual_slot, &other.virtual_slot) {
                (None, None) => true,
                (Some(previous), Some(next)) => {
                    match (previous.offset.try_read(), next.offset.try_read()) {
                        (Some(previous), Some(next)) => std::ptr::eq(&*previous, &*next),
                        _ => false,
                    }
                }
                _ => false,
            }
    }
}

/// 滚轮目标的精确归属，避免双栏串滚或面板切换后误投递。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelScrollTarget {
    pub kind: PanelKind,
    pub slot_index: usize,
    generation: u64,
}

/// 面板渲染体调用：覆盖式注册本帧滚动槽位。
/// 每帧刷新保证句柄指向最近一次渲染仍存活的面板状态；面板关闭后 `ACTIVE_PANEL`
/// 不再匹配，仲裁会放行且绝不触碰失效句柄。
pub fn register_panel_scrolls(kind: PanelKind, slots: Vec<PanelScrollSlot>) {
    register_owner(PanelScrollOwner {
        kind,
        slots,
        virtual_slot: None,
        generation: 0,
    });
}

fn register_owner(mut owner: PanelScrollOwner) {
    static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
    let state = PANEL_SCROLL_OWNER.state();
    let mut current = state.write_no_update();
    owner.generation = current
        .as_ref()
        .filter(|previous| previous.same_handles(&owner))
        .map(|previous| previous.generation)
        .unwrap_or_else(|| NEXT_GENERATION.fetch_add(1, Ordering::Relaxed));
    *current = Some(owner);
}

pub(crate) fn register_virtual_panel_scroll(
    kind: PanelKind,
    area: Rect,
    offset: State<usize>,
    content_height: usize,
    viewport_height: usize,
) {
    register_owner(PanelScrollOwner {
        kind,
        slots: Vec::new(),
        generation: 0,
        virtual_slot: Some(VirtualPanelScrollSlot {
            area,
            offset,
            content_height,
            viewport_height,
        }),
    });
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

fn hovered_area(
    areas: impl IntoIterator<Item = Rect>,
    mouse_row: u16,
    mouse_col: u16,
) -> Option<usize> {
    areas
        .into_iter()
        .position(|area| mouse_in_area(mouse_row, mouse_col, area))
}

// ── 仲裁 handler ────────────────────────────────────────────────────────

/// 面板滚轮仲裁（`Global+High`，由 PanelOverlay 挂载）。
/// 命中面板区滚轮 → 按 `SCROLL_LINES` 逐事件立即驱动 → Consumed。
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
    let Some(slot_index) = hovered_area(
        owner
            .slots
            .iter()
            .map(|slot| slot.area)
            .chain(owner.virtual_slot.iter().map(|slot| slot.area)),
        mouse.row,
        mouse.column,
    ) else {
        return EventResult::Ignored;
    };
    let delta = match mouse.kind {
        MouseEventKind::ScrollDown => SCROLL_LINES as i32,
        MouseEventKind::ScrollUp => -(SCROLL_LINES as i32),
        _ => unreachable!(),
    };
    // 只驱动鼠标所在槽位；面板整体区域内的 border/divider 由消息区遮挡判定吞掉。
    apply_panel_scroll(
        PanelScrollTarget {
            kind: owner.kind,
            slot_index,
            generation: owner.generation,
        },
        delta,
    );
    EventResult::Consumed
}

/// 逐事件立即落地：每个滚轮事件固定移动 `delta` 行。
///
/// [Why 再校验] 事件分发与落地之间面板可能关闭/切换，注册表也可能被下一帧覆盖；
/// 只有 kind + generation 与读取时一致才允许驱动目标句柄。
fn apply_panel_scroll(target: PanelScrollTarget, delta: i32) {
    let Some(owner) = PANEL_SCROLL_OWNER.state().read().clone() else {
        return;
    };
    if owner.kind != target.kind
        || owner.generation != target.generation
        || *ACTIVE_PANEL.state().read() != Some(owner.kind)
    {
        return;
    }
    let Some(driver) = owner.driver(target.slot_index) else {
        return;
    };
    // [Fix 竞态崩溃] 组件 update 遍历可能已持有目标 State 的写引用，write_no_update
    // 会 panic；try_write 冲突时丢弃本次——逐事件语义没有可回放的 pending，
    // 累积重试会重新引入时间窗合并。
    let _ = driver.try_apply(delta);
}

fn apply_scroll_to_virtual(
    offset: &mut usize,
    delta: i32,
    content_height: usize,
    viewport_height: usize,
) {
    let next = if delta >= 0 {
        offset.saturating_add(delta as usize)
    } else {
        offset.saturating_sub(delta.unsigned_abs() as usize)
    };
    *offset = next.min(content_height.saturating_sub(viewport_height));
}

/// 把一次滚轮事件的滚动量应用到 ScrollViewState（正=向下，负=向上；边界 clamp）。
/// 独立函数便于单测（构造 ScrollViewState 无需 State 句柄）。
fn apply_scroll_to_view(scroll: &mut ScrollViewState, delta: i32) {
    if delta == 0 {
        return;
    }
    if delta > 0 {
        let mut n = delta as u16;
        while n > 0 && !scroll.is_at_bottom() {
            scroll.scroll_down();
            n -= 1;
        }
    } else {
        let mut n = (-delta) as u16;
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
    fn test_apply_scroll_up_clamps_at_top() {
        let mut scroll =
            ScrollViewState::with_offset(ratatui_kit::ratatui::layout::Position::new(0, 3));
        apply_scroll_to_view(&mut scroll, -99);
        assert_eq!(scroll.offset().y, 0);
        // 已在顶部：再向上 no-op
        apply_scroll_to_view(&mut scroll, -1);
        assert_eq!(scroll.offset().y, 0);
    }

    #[test]
    fn test_apply_scroll_zero_noop() {
        let mut scroll =
            ScrollViewState::with_offset(ratatui_kit::ratatui::layout::Position::new(0, 4));
        apply_scroll_to_view(&mut scroll, 0);
        assert_eq!(scroll.offset().y, 4);
    }

    #[test]
    fn test_apply_scroll_down_before_first_render_noop() {
        // 渲染前 size=None → is_at_bottom() 恒 true：向下驱动为 no-op，
        // 绝不越界（防御：句柄就绪但内容几何未知时静默丢弃）。
        let mut scroll = ScrollViewState::default();
        apply_scroll_to_view(&mut scroll, 3);
        assert_eq!(scroll.offset().y, 0);
    }

    #[test]
    fn test_apply_scroll_up_from_bottom_sentinel() {
        // scroll_to_bottom 在 size 未知时设 u16::MAX 哨兵；向上仍正常回滚
        let mut scroll = ScrollViewState::default();
        scroll.scroll_to_bottom();
        apply_scroll_to_view(&mut scroll, -3);
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

    #[test]
    fn test_hovered_slot_follows_mouse_position() {
        let areas = [rect(0, 10, 40, 20), rect(40, 10, 60, 20)];

        assert_eq!(hovered_area(areas, 15, 20), Some(0));
        assert_eq!(hovered_area(areas, 15, 70), Some(1));
        assert_eq!(hovered_area(areas, 9, 20), None);
        assert_eq!(hovered_area(areas, 30, 70), None);
    }

    #[test]
    fn test_scroll_target_distinguishes_hovered_slot_and_panel() {
        let left = PanelScrollTarget {
            kind: PanelKind::Model,
            slot_index: 0,
            generation: 1,
        };
        let right = PanelScrollTarget {
            kind: PanelKind::Model,
            slot_index: 1,
            generation: 1,
        };
        let subagent = PanelScrollTarget {
            kind: PanelKind::SubAgentDetail,
            slot_index: 0,
            generation: 1,
        };

        assert_ne!(left, right, "左右栏必须有独立滚动归属");
        assert_ne!(left, subagent, "切换面板后不得复用旧 pending 归属");
        let reopened = PanelScrollTarget {
            generation: 2,
            ..left
        };
        assert_ne!(left, reopened);
    }
}

#[cfg(test)]
#[path = "panel_scroll_test.rs"]
mod virtual_tests;
