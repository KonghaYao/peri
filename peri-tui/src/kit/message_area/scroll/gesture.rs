use crate::kit::message_area::selection::{WrappedLineInfo, visual_to_logical};

use super::GesturePending;

/// 单击判定：Down 与 Drag/Up 屏幕坐标差 ≤1 行、≤2 列（手抖容差）。
///
/// [Why] crossterm 按下后移动会发 `Drag` 事件，因此"无 Drag"本身已表明
/// 未移动；容差是对事件丢失/平台差异的防御，超过即视为拖拽意图（升级为
/// 文本拖拽，不触发点击动作）。
/// 只比较屏幕坐标（`(column, row)`）——Down/Drag 天然都是屏幕坐标，判定
/// 不再经过视觉换算，滚动偏移/网格前缀的坐标空间问题在判定路径上不复存在。
/// 判定时机在 Drag 分支（升级判定先于节流闸门）；Up 结算不做坐标比较，
/// 只看手势是否仍为 Pending（是否升级过）。
pub(in crate::kit::message_area) fn is_click(down: (u16, u16), cur: (u16, u16)) -> bool {
    cur.0.abs_diff(down.0) <= 2 && cur.1.abs_diff(down.1) <= 1
}

/// entry 单击命中解析：视觉行 → `(slot_index, local_idx)`；仅当该行属于
/// entry 的逻辑首行（`local_idx == 0`，即 header/label 行）时命中。
///
/// [Why 仅首行] 正文行保留给文本选区/复制；展开动作只挂在 header 上，
/// 与键盘 Enter（焦点在 entry 时切换）语义一致。wrap_map 越界
/// （footer 区域无映射）→ `None`。header 换行成多视觉行时，所属视觉行
/// 均反查到同一逻辑行，全部命中。
pub(in crate::kit::message_area) fn entry_click_target(
    wrap_map: &[WrappedLineInfo],
    slot_offsets: &[usize],
    visual_row: usize,
) -> Option<(usize, usize)> {
    let li = visual_to_logical(visual_row, wrap_map)?;
    let slot = wrap_map.get(li)?.slot_index;
    let local = li.saturating_sub(*slot_offsets.get(slot)?);
    (local == 0).then_some((slot, 0))
}

// ── 手势状态机纯函数层 ───────────────────────────────────────────────
// [Why 提取] ratatui-kit 未暴露 `State<T>`（SingleWaker）的构造 API
// （`ReactiveHandle::new` 仅存在于 atom 的 WakerMap impl），handle_event 的
// State 参数无法在测试中构造——状态机转移以纯函数表达，测试直调锁定。

/// Down 冻结：记录 Pending 手势（屏幕坐标 + 一次性换算的内容坐标 +
/// entry header 命中反查）。不改任何可视状态——真实拖动（Drag 超容差）
/// 才升级为拖拽。
pub(in crate::kit::message_area) fn freeze_down(
    screen: (u16, u16),
    visual: (usize, u16),
    wrap_map: &[WrappedLineInfo],
    slot_offsets: &[usize],
) -> GesturePending {
    // [冻结命中] entry 命中在 Down 时反查冻结——Up 结算直接消费，不再二次
    // 换算（滚动/网格前缀的坐标正确性由此时保证）。
    GesturePending {
        screen,
        visual,
        entry_hit: entry_click_target(wrap_map, slot_offsets, visual.0),
    }
}

/// Drag 分支决策结果（纯函数 `drag_step` 的输出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::kit::message_area) enum DragAction {
    /// 超容差 → 升级为拖拽（Armed）：`start_drag(pending.visual)` +
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
        Some(p) if !is_click(p.screen, drag_screen) => DragAction::Upgrade(p),
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
