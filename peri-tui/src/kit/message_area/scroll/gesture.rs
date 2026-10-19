use crate::kit::message_area::selection::SlotIndex;

use super::GesturePending;
use crate::kit::text_selection::TextSelection;
use ratatui_kit::crossterm::event::{MouseButton, MouseEventKind};
use ratatui_kit::ratatui::layout::Rect;

pub(in crate::kit::message_area) fn selection_captures_pointer(
    pending: Option<GesturePending>,
    dragging: bool,
    kind: MouseEventKind,
) -> bool {
    (pending.is_some() || dragging)
        && matches!(
            kind,
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
        )
}

pub(in crate::kit::message_area) fn selection_position(
    area: Rect,
    screen: (u16, u16),
    scroll_y: usize,
) -> (usize, u16) {
    let row = screen
        .1
        .saturating_sub(area.y)
        .min(area.height.saturating_sub(1));
    let col = screen.0.saturating_sub(area.x).min(area.width);
    (scroll_y.saturating_add(usize::from(row)), col)
}

pub(in crate::kit::message_area) fn selection_bounds_on_release(
    selection: &mut TextSelection,
    visual: (usize, u16),
) -> Option<((usize, u16), (usize, u16))> {
    if !selection.dragging {
        return None;
    }
    selection.update_drag(visual.0, visual.1);
    selection.normalized_bounds()
}

/// 单击判定（**仅用于有单击动作的 entry header**）：Down 与 Drag/Up 屏幕坐标差
/// ≤1 行、≤2 列（手抖容差）。
///
/// [Why 只对 header 生效] header 的单击会折叠/展开 entry，需要防抖；普通正文
/// 没有单击动作，1–2 列的短拖或相邻行同列拖选必须立即建立选区（F1）。
/// 正文的单击判定见 [`is_click_for`]（只有完全未移动才算单击）。
///
/// [Why 屏幕坐标] 只比较屏幕坐标（`(column, row)`）——Down/Drag 天然都是屏幕
/// 坐标，判定不再经过视觉换算，滚动偏移/网格前缀的坐标空间问题在判定路径上
/// 不复存在。判定时机在 Drag 分支（升级判定先于节流闸门）。
pub(in crate::kit::message_area) fn is_click(down: (u16, u16), cur: (u16, u16)) -> bool {
    cur.0.abs_diff(down.0) <= 2 && cur.1.abs_diff(down.1) <= 1
}

/// 按手势类型选择单击容差：entry header 保留防抖，正文任何位移都算拖拽意图。
pub(in crate::kit::message_area) fn is_click_for(
    pending: &GesturePending,
    cur: (u16, u16),
) -> bool {
    if pending.entry_hit.is_some() {
        is_click(pending.screen, cur)
    } else {
        pending.screen == cur
    }
}

/// Up 时按**最终位移**补结算：没有收到中间 Drag 的正文拖选也能建立选区。
/// 仅正文（无 entry 点击动作）生效；header 的单击语义保持不变。
pub(in crate::kit::message_area) fn release_upgrades(
    pending: &GesturePending,
    up_screen: (u16, u16),
) -> bool {
    pending.entry_hit.is_none() && pending.screen != up_screen
}

/// 文本拖选是否接收该鼠标事件。
///
/// - 已捕获指针（Pending/Armed 的 Drag/Up）→ 接收（即使移出消息区）；
/// - Left Down → 接收范围是**收窄前的外框**：居中带左右留白也能建立锚点，
///   列坐标由 `selection_position` 钳到正文边缘（F2）；
/// - 其余 Drag/Up 只有落在文本带内才接收。
pub(in crate::kit::message_area) fn text_selection_accepts_pointer(
    in_text_area: bool,
    in_outer_area: bool,
    captured: bool,
    kind: MouseEventKind,
) -> bool {
    if captured {
        return true;
    }
    match kind {
        MouseEventKind::Down(MouseButton::Left) => in_outer_area,
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => {
            in_text_area
        }
        _ => false,
    }
}

/// entry 单击命中解析：视觉行 → `(slot_index, local_idx)`；仅当该行属于
/// entry 的逻辑首行（`local_idx == 0`，即 header/label 行）时命中。
///
/// [Why 仅首行] 正文行保留给文本选区/复制；展开动作只挂在 header 上，
/// 与键盘 Enter（焦点在 entry 时切换）语义一致。wrap_map 越界
/// （footer 区域无映射）→ `None`。header 换行成多视觉行时，所属视觉行
/// 均反查到同一逻辑行，全部命中。
pub(in crate::kit::message_area) fn entry_click_target_index(
    index: &SlotIndex,
    visual_row: usize,
) -> Option<(usize, usize)> {
    let lookup = index.visual_lookup(visual_row)?;
    (lookup.local_logical == 0).then_some((lookup.slot_index, 0))
}

#[cfg(test)]
pub(in crate::kit::message_area) fn entry_click_target(
    wrap_map: &[crate::kit::message_area::selection::WrappedLineInfo],
    slot_offsets: &[usize],
    visual_row: usize,
) -> Option<(usize, usize)> {
    let logical = crate::kit::message_area::selection::visual_to_logical(visual_row, wrap_map)?;
    let slot = wrap_map.get(logical)?.slot_index;
    let local = logical.saturating_sub(*slot_offsets.get(slot)?);
    (local == 0).then_some((slot, 0))
}

// ── 手势状态机纯函数层 ───────────────────────────────────────────────
// [Why 提取] ratatui-kit 未暴露 `State<T>`（SingleWaker）的构造 API
// （`ReactiveHandle::new` 仅存在于 atom 的 WakerMap impl），handle_event 的
// State 参数无法在测试中构造——状态机转移以纯函数表达，测试直调锁定。

/// Down 冻结：记录 Pending 手势（屏幕坐标 + 一次性换算的内容坐标 +
/// entry header 命中反查）。不改任何可视状态——真实拖动（Drag 超容差）
/// 才升级为拖拽。
pub(in crate::kit::message_area) fn freeze_down_index(
    screen: (u16, u16),
    visual: (usize, u16),
    index: &SlotIndex,
    allow_entry_hit: bool,
) -> GesturePending {
    // [冻结命中] entry 命中在 Down 时反查冻结——Up 结算直接消费，不再二次
    // 换算（滚动/网格前缀的坐标正确性由此时保证）。
    // 居中带左右留白上的 Down 不参与 entry 点击（`allow_entry_hit=false`）：
    // 留白没有文本，不应触发折叠动作。
    GesturePending {
        screen,
        visual,
        entry_hit: allow_entry_hit
            .then(|| entry_click_target_index(index, visual.0))
            .flatten(),
    }
}

#[cfg(test)]
pub(in crate::kit::message_area) fn freeze_down(
    screen: (u16, u16),
    visual: (usize, u16),
    wrap_map: &[crate::kit::message_area::selection::WrappedLineInfo],
    slot_offsets: &[usize],
) -> GesturePending {
    GesturePending {
        screen,
        visual,
        entry_hit: entry_click_target(wrap_map, slot_offsets, visual.0),
    }
}

/// Drag 分支决策结果（纯函数 `drag_step` 的输出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::kit::message_area) enum DragAction {
    /// 越过单击判定 → 升级为拖拽（Armed）：`start_drag(pending.visual)` +
    /// `update_drag(当前视觉坐标)` + gesture → None。**升级瞬间不受节流**。
    Upgrade(GesturePending),
    /// 节流窗口内且未升级：零副作用（Pending 原样保留；事件丢失的
    /// UpdateOnly 也被吞，与现状节流行为一致）。
    Throttled,
    /// 节流通过且无 Down 记录（如事件丢失）：`update_drag` 空转
    /// （dragging=false 时 no-op；已升级后的拖拽延续则跟随鼠标）。
    UpdateOnly,
    /// 节流通过且容差内（手抖）：Pending 原样保留，零副作用。
    KeepPending,
}

/// Drag 分支决策：**升级判定先于节流闸门**。
///
/// [Why 先于节流] 终端按下后任何微移都会报 Drag（无阈值）；若升级放在节流
/// 之后，<50ms 的快速拖拽首个 Drag 被节流吞掉，升级永不发生 → Up 误判单击
/// （误折叠 + 复制丢失）。容差内（手抖）手势保持 Pending，节流照常。
pub(in crate::kit::message_area) fn drag_step(
    pending: Option<GesturePending>,
    drag_screen: (u16, u16),
    within_throttle_window: bool,
) -> DragAction {
    match pending {
        Some(p) if !is_click_for(&p, drag_screen) => DragAction::Upgrade(p),
        Some(_) if within_throttle_window => DragAction::Throttled,
        Some(_) => DragAction::KeepPending,
        None if within_throttle_window => DragAction::Throttled,
        None => DragAction::UpdateOnly,
    }
}

/// Up 结算分流：单击结算分工（依赖 dispatch 注册序——mod.rs 单击 handler
/// 先消费 Pending 命中，未命中才落到 scroll.rs Up 分支）。
/// 返回是否应复位 gesture：Pending 未命中（gesture 仍为 Some）→ 复位；
/// Armed（dragging）→ 复制流程，gesture 已在升级瞬间复位为 None，无需再写。
pub(in crate::kit::message_area) fn settle_up(dragging: bool, gesture_pending: bool) -> bool {
    !dragging && gesture_pending
}

#[cfg(test)]
#[path = "gesture_test.rs"]
mod tests;
