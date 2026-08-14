//! 滚动节流 + 鼠标事件处理 + 吸底自动跟随。

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::kit::atoms::TUI_CONFIG_HANDLE;
use crate::kit::focus_router;
use crate::kit::mouse_router;
use crate::kit::text_selection::TextSelection;
use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Rect;

use super::props::{ScrollbarFields, mouse_in_area};
use super::selection::{
    WrappedLineInfo, copy_to_clipboard, extract_visual_range, mark_copy_message, visual_to_logical,
};

// ── 滚动状态 ──────────────────────────────────────────────────────────────

/// 消息区滚动状态——替代 ratatui-kit 的 `ScrollViewState`。
///
/// [Why] `ScrollViewState.offset` 是 ratatui `Position`（u16），`total_visual_rows`
/// 超过 65535 视觉行（100 列终端约 650 万字符，如长代码文件输出/大 diff 累积）时
/// 滚动上限被截断，真实底部（footer/spinner）不可达、scrollbar thumb 到底但内容没到底。
/// 自持 `usize` 偏移彻底解除上限。
///
/// [Why bottom] `scroll_to_bottom()` 设置最大偏移，渲染每帧 clamp 到当帧的
/// `max_scroll`——与旧 `ScrollViewState::scroll_to_bottom`（size 为 None 时设
/// `u16::MAX`）行为一致，但 usize 下无上限。
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct ScrollPos {
    offset_y: usize,
}

impl ScrollPos {
    pub(super) fn offset(&self) -> usize {
        self.offset_y
    }

    pub(super) fn set_offset(&mut self, y: usize) {
        self.offset_y = y;
    }

    pub(super) fn scroll_up(&mut self) {
        self.offset_y = self.offset_y.saturating_sub(1);
    }

    pub(super) fn scroll_down(&mut self) {
        self.offset_y = self.offset_y.saturating_add(1);
    }

    pub(super) fn scroll_to_top(&mut self) {
        self.offset_y = 0;
    }

    /// 滚动到底——偏移设为最大，渲染 clamp 到当帧 `max_scroll`。
    pub(super) fn scroll_to_bottom(&mut self) {
        self.offset_y = usize::MAX;
    }
}

// ── 滚动速度控制 ──────────────────────────────────────────────────────────

/// 鼠标滚轮每格的滚动行数倍数。
/// pub(crate)：面板滚轮仲裁（panel_scroll.rs）复用同一步长，统一跨区域滚动速度。
pub(crate) const SCROLL_LINES: u16 = 3;

/// mod.rs 在 total_visual_rows 上追加的滚动缓冲行数（仅影响 max_scroll /
/// content_length，不计入实际渲染内容）。吸底跟随恢复判定需扣除该缓冲，
/// 见 `should_follow_after_user_scroll` 的 [Fix padding]。
pub(super) const SCROLL_PADDING: usize = 2;

/// scroll_frame_ms() 的默认值。fps=20 → 50ms。
const DEFAULT_SCROLL_FRAME_MS: u64 = 50;

/// fps 值转换为毫秒间隔
fn fps_to_ms(fps: u32) -> u64 {
    match fps {
        60 => 16,
        30 => 33,
        20 => 50,
        _ => 16,
    }
}

/// 优先级：TuiConfig.scroll_fps > PERI_SCROLL_THROTTLE_MS 环境变量 > 默认 50ms（20fps）。
/// 下限 1ms 防止零值导致无节流。
/// TuiConfig 每次读取（try_read 代价 ~5ns，无争用时），因为用户可能运行时切换。
/// pub(crate)：面板滚轮仲裁（panel_scroll.rs）复用同一帧率配置。
pub(crate) fn scroll_frame_ms() -> u64 {
    // 优先读 TuiConfig
    if let Some(handle) = TUI_CONFIG_HANDLE.get()
        && let Some(tui) = handle.try_read()
        && let Some(fps) = tui.scroll_fps
    {
        return fps_to_ms(fps).max(1);
    }
    // fallback: 环境变量
    thread_local! {
        static ENV_VAL: Option<u64> = std::env::var("PERI_SCROLL_THROTTLE_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(|v: u64| v.max(1));
    }
    if let Some(ms) = ENV_VAL.with(|v| *v) {
        return ms;
    }
    DEFAULT_SCROLL_FRAME_MS
}

#[derive(Debug, Clone)]
/// pub(crate)：面板滚轮仲裁（panel_scroll.rs）复用同一节流器。
pub(crate) struct ScrollThrottle {
    pub(crate) last_flush: Instant,
    pub(crate) pending_delta: i32, // positive = scroll_down, negative = scroll_up
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
pub(super) fn is_scrollbar_column(mouse_col: u16, area: Rect) -> bool {
    mouse_col == area.x.saturating_add(area.width).saturating_sub(1)
}

/// 单击判定：Down 与 Drag/Up 屏幕坐标差 ≤1 行、≤2 列（手抖容差）。
///
/// [Why] crossterm 按下后移动会发 `Drag` 事件，因此"无 Drag"本身已表明
/// 未移动；容差是对事件丢失/平台差异的防御，超过即视为拖拽意图（升级为
/// 文本拖拽，不触发点击动作）。
/// 只比较屏幕坐标（`(column, row)`）——Down/Drag 天然都是屏幕坐标，判定
/// 不再经过视觉换算，滚动偏移/网格前缀的坐标空间问题在判定路径上不复存在。
/// 判定时机在 Drag 分支（升级判定先于节流闸门）；Up 结算不做坐标比较，
/// 只看手势是否仍为 Pending（是否升级过）。
pub(super) fn is_click(down: (u16, u16), cur: (u16, u16)) -> bool {
    cur.0.abs_diff(down.0) <= 2 && cur.1.abs_diff(down.1) <= 1
}

/// entry 单击命中解析：视觉行 → `(slot_index, local_idx)`；仅当该行属于
/// entry 的逻辑首行（`local_idx == 0`，即 header/label 行）时命中。
///
/// [Why 仅首行] 正文行保留给文本选区/复制；展开动作只挂在 header 上，
/// 与键盘 Enter（焦点在 entry 时切换）语义一致。wrap_map 越界
/// （footer 区域无映射）→ `None`。header 换行成多视觉行时，所属视觉行
/// 均反查到同一逻辑行，全部命中。
pub(super) fn entry_click_target(
    wrap_map: &[WrappedLineInfo],
    slot_offsets: &[usize],
    visual_row: usize,
) -> Option<(usize, usize)> {
    let li = visual_to_logical(visual_row, wrap_map)?;
    let slot = wrap_map.get(li)?.slot_index;
    let local = li.saturating_sub(*slot_offsets.get(slot)?);
    (local == 0).then_some((slot, 0))
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
    (numerator + denominator / 2)
        .checked_div(denominator)
        .unwrap_or(0)
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
///
/// [Why ceil] 正向映射用的是 floor（整数除法），反推用 ceil 才是正确的逆运算：
/// floor(a*b/c) 的逆运算是 ceil(x*c/b)。用 floor 做反推会导致 thumb 拖到接近底部
/// 时（如 99%）无法滚动到最后一行——只有恰好拖到 100% 位置才能到底。
fn position_to_scroll_y(position: usize, max_position: usize, max_scroll: usize) -> usize {
    if max_position == 0 || max_scroll == 0 {
        0
    } else {
        (position * max_scroll).div_ceil(max_position)
    }
}

// ── 滚动节流（私有）────────────────────────────────────────────────────

/// 纯函数：offset 应用滚动量后的新位置（正=向下，负=向上；越界封顶/封底）。
/// 哨兵归一化（offset > max_scroll 时先落到 max_scroll）由 `apply_pending` 负责。
fn apply_delta_to_offset(offset: usize, delta: i32, max_scroll: usize) -> usize {
    if delta > 0 {
        offset.saturating_add(delta as usize).min(max_scroll)
    } else {
        offset.saturating_sub((-delta) as usize)
    }
}

/// 反向判定：pending 与新 delta 方向相反时需要先落地旧 pending，
/// 避免「先累积后抵消」造成滚动不到位/回弹（ghostty/ssh burst 场景）。
fn is_reverse_direction(pending_delta: i32, delta: i32) -> bool {
    pending_delta != 0 && (pending_delta > 0) != (delta > 0)
}

/// 把一段滚动量（正=向下，负=向上）推入 scroll_state，并同步 follow 状态。
fn apply_pending(
    pending: i32,
    scroll_state: &State<ScrollPos>,
    scrollbar_fields: &State<ScrollbarFields>,
    follow_bottom: &State<bool>,
) {
    if pending == 0 {
        return;
    }
    let fields = *scrollbar_fields.read();
    let max_scroll = fields.content_length.saturating_sub(fields.viewport_length);
    let mut state = scroll_state.write_no_update();
    // 跟随态下 offset 可能是 usize::MAX 哨兵（scroll_to_bottom 设置、渲染 clamp
    // 前）——先归一化到当帧底部，否则滚轮上滚要先"滚空气"。
    if state.offset() > max_scroll {
        state.set_offset(max_scroll);
    }
    let final_offset = apply_delta_to_offset(state.offset(), pending, max_scroll);
    state.set_offset(final_offset);
    drop(state);
    update_follow_on_scroll(follow_bottom, max_scroll, final_offset);
}

/// 节流 flush 核心：把累积的 pending_delta 一次性推入 scroll_state 并同步 follow。
/// 供 `apply_scroll`（事件到达时）与渲染帧兜底（mod.rs 每帧调用）共用。
/// 返回是否实际 flush 了非零滚动量。
pub(super) fn flush_scroll_if_due(
    scroll_throttle: &State<ScrollThrottle>,
    scroll_state: &State<ScrollPos>,
    scrollbar_fields: &State<ScrollbarFields>,
    follow_bottom: &State<bool>,
) -> bool {
    let mut st = scroll_throttle.write_no_update();
    let now = Instant::now();
    if now.duration_since(st.last_flush) < Duration::from_millis(scroll_frame_ms()) {
        return false;
    }
    let pending = st.pending_delta;
    st.pending_delta = 0;
    st.last_flush = now;
    drop(st);
    apply_pending(pending, scroll_state, scrollbar_fields, follow_bottom);
    pending != 0
}

/// 滚动节流：累积 delta，仅在距上次 flush ≥ scroll_frame_ms() 时推入 scroll_state。
/// write_no_update 不触发 notifier.wake()——依赖 dispatch 后 ratatui-kit loop 强制 render。
fn apply_scroll(
    delta: i32,
    scroll_throttle: &State<ScrollThrottle>,
    scroll_state: &State<ScrollPos>,
    scrollbar_fields: &State<ScrollbarFields>,
    follow_bottom: &State<bool>,
) {
    {
        let mut st = scroll_throttle.write_no_update();
        // [Fix 反向落地] 反向滚动时旧方向 pending 立即落地（即使未到节流窗口），
        // 再累积新方向——消除「先动后猛跳」的抵消错位（ghostty/ssh burst 场景）。
        if is_reverse_direction(st.pending_delta, delta) {
            let old = st.pending_delta;
            st.pending_delta = 0;
            st.last_flush = Instant::now();
            drop(st);
            apply_pending(old, scroll_state, scrollbar_fields, follow_bottom);
        } else {
            st.pending_delta += delta;
        }
    }
    flush_scroll_if_due(
        scroll_throttle,
        scroll_state,
        scrollbar_fields,
        follow_bottom,
    );
}

// ── 粘性吸底跟随状态 ───────────────────────────────────────────────────

/// 用户滚动后是否应恢复吸底跟随：只有滚到真正底部（offset ≥ max_scroll，
/// 含 Down 溢出 / End / usize::MAX 哨兵）才恢复。
/// [Why 严格到底] 旧版用 proximity 阈值（视口 1/4）判定：loading 中用户上滚
/// ≤ 阈值会在下一次内容增长时被吸回，反复拉锯；且跟随态下内容跳增超过阈值时
/// 跟随被拒绝，视口停在半空、spinner 消失——体验"底部跳动"。
/// 粘性语义：一向上滚动即退出跟随（浏览模式），滚回底部才恢复。
/// [Fix padding] max_scroll 含 mod.rs 的 +2 滚动缓冲（SCROLL_PADDING）：若按它
/// 判定，用户滚到视觉底部（真实内容底 = max_scroll - 2）时 offset 恒差 2 行，
/// 吸底跟随永不恢复。扣除缓冲后「滚到视觉底部」即恢复；内容不满一屏
/// （max_scroll ≤ padding）时 offset=0 仍视为底部。
fn should_follow_after_user_scroll(max_scroll: usize, offset_y: usize) -> bool {
    offset_y >= max_scroll.saturating_sub(SCROLL_PADDING)
}

/// 用户滚动入口（键盘 / 滚轮 / 滚动条）滚动落定后同步 follow_bottom。
/// write_no_update：事件 dispatch 后 loop 强制 render，无需 wake。
fn update_follow_on_scroll(follow_bottom: &State<bool>, max_scroll: usize, offset_y: usize) {
    *follow_bottom.write_no_update() = should_follow_after_user_scroll(max_scroll, offset_y);
}

/// §8.1 `↓ New output` 指示器判定：浏览态（用户滚离底部，follow=false）且
/// 视口未到**真实内容底**时显示。
///
/// [Why 内容底口径] `content_bottom` = core + footer 视觉行数（**不含**
/// SCROLL_PADDING 缓冲——缓冲行不可见，滚到视觉底部即消失）。与粘性 follow
/// 恢复（`should_follow_after_user_scroll` 同样扣缓冲）口径对齐：滚到底时
/// follow 恢复 true 且指示器消失；浏览态中内容增长不移动 viewport，指示器
/// 出现提示有未看的新输出。
pub(super) fn new_output_indicator_active(
    follow_bottom: bool,
    scroll_y: usize,
    vis_height: usize,
    content_bottom: usize,
) -> bool {
    !follow_bottom && scroll_y + vis_height < content_bottom
}

/// [Slice 4 §6.8] Interaction block 锚定对齐目标：pending block 末行超出视口
/// 时返回对齐偏移（block 底部对齐视口底部），否则 None（不调整）。
///
/// 纯函数——`run_auto_follow` 的 anchor 分支消费；浏览态与跟随态均生效
/// （§6.8「等待时锚定此 block」，不得被新 streaming chunk 滚出视口）。
pub(super) fn anchor_scroll_target(
    scroll_y: usize,
    vis_height: usize,
    anchor_end: usize,
    max_scroll: usize,
) -> Option<usize> {
    if scroll_y.saturating_add(vis_height) < anchor_end {
        Some(anchor_end.saturating_sub(vis_height).min(max_scroll))
    } else {
        None
    }
}

// ── 左键手势状态机（Pending → Armed → settled）────────────────────────

/// 消息区内一次左键手势的中间状态（取代 `selection_down_pos` 的语义）。
///
/// 状态表达：`None` = Idle；`Some` = Pending（Down 已记录、未升级为拖拽）；
/// Drag 超容差升级后置 `None`，由 `text_sel.dragging == true` 表达 Armed。
///
/// [Why 冻结] Down 时一次性换算并冻结内容坐标与 entry 命中——Up 结算只
/// 消费冻结结果，不再二次换算（滚动偏移/网格前缀的坐标正确性由 Down 保证）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GesturePending {
    /// 按下点屏幕坐标 `(column, row)`——唯一参与判定的坐标（`is_click` 比较）。
    pub(super) screen: (u16, u16),
    /// 按下点内容坐标（视觉行/列）——Down 时换算冻结，升级时作 `start_drag`
    /// 起点（视觉行 = `row − area.y + scroll_y`，视觉列 = `column − area.x`）。
    pub(super) visual: (usize, u16),
    /// Down 时命中测试结果：entry header（可折叠行）或 None。
    /// 冻结命中消除 Up 结算对 wrap_map 的二次反查。
    pub(super) entry_hit: Option<(usize, usize)>, // (slot, local_idx)
}

// ── 手势状态机纯函数层 ───────────────────────────────────────────────
// [Why 提取] ratatui-kit 未暴露 `State<T>`（SingleWaker）的构造 API
// （`ReactiveHandle::new` 仅存在于 atom 的 WakerMap impl），handle_event 的
// State 参数无法在测试中构造——状态机转移以纯函数表达，测试直调锁定。

/// Down 冻结：记录 Pending 手势（屏幕坐标 + 一次性换算的内容坐标 +
/// entry header 命中反查）。不改任何可视状态——真实拖动（Drag 超容差）
/// 才升级为拖拽。
pub(super) fn freeze_down(
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
pub(super) enum DragAction {
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
pub(super) fn drag_step(
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
pub(super) fn settle_up(dragging: bool, gesture_pending: bool) -> bool {
    !dragging && gesture_pending
}

// ── 鼠标事件处理 ─────────────────────────────────────────────────────────

/// 从 `use_event_handler` 闭包提取的鼠标/键盘处理逻辑。
/// 包含：鼠标滚轮节流、文本拖拽选中（Down/Drag/Up）、键盘滚动、
/// PERI_DISABLE_DRAG_SELECT 分支、parking_lot 死锁规避。
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_event(
    event: &Event,
    area_rect: Option<Rect>,
    vis_width: u16,
    scroll_state: &State<ScrollPos>,
    scroll_throttle: &State<ScrollThrottle>,
    text_sel: &State<TextSelection>,
    gesture: &State<Option<GesturePending>>,
    drag_throttle: &State<DragThrottle>,
    // 拼接后的全量 wrap_map（按 visual_start 升序）。mod.rs 渲染前已拼接好。
    wrap_map: &Arc<Vec<WrappedLineInfo>>,
    // [Scheme D] slot 行数据 + 累积偏移，按需解析视口行，不再传全量 clone。
    slot_arcs: &Arc<Vec<Arc<Vec<ratatui_kit::ratatui::text::Line<'static>>>>>,
    slot_offsets: &Arc<Vec<usize>>,
    scrollbar_fields: &State<ScrollbarFields>,
    scrollbar_drag: &State<ScrollbarDragState>,
    follow_bottom: &State<bool>,
    // [D3 §9] 语义复制所需的快照 VM 列表与网格——事件时点由 mod.rs 闭包
    // 传入（im::Vector clone O(1)）；None 时复制保持既有行为（无剥离）。
    view_models: Option<&im::Vector<crate::kit::tui_render_unit::TuiRenderUnit>>,
    grid: Option<crate::kit::message_area::grid::GridSpec>,
) -> EventResult {
    if let Event::Key(key) = &event {
        let _ = focus_router::message_accepts_key(key);
    }

    if let Event::Mouse(mouse) = &event {
        // 弹窗或面板激活时不处理鼠标——放行给前景 handler（如模型快速切换弹窗覆盖
        // 消息区时，点击行必须由弹窗消费，否则这里会先 Consumed 吃掉事件）。
        // [Why] 但拖拽残留状态必须在此清理：遮挡时下方 Up 分支（scrollbar_drag /
        // gesture / text_sel 复位）永远不会执行，拖拽中途弹窗打开会
        // 残留 dragging 状态，弹窗关闭后下一次点击被误判为拖拽（误复制/点击错乱）。
        if mouse_router::is_occluded() {
            // render 不依赖 active / gesture，用 write_no_update 避免 wake 噪音
            scrollbar_drag.write_no_update().active = false;
            *gesture.write_no_update() = None;
            // [TRAP] 先 copy 出 dragging 再 write——parking_lot 同 thread read+write
            // 冲突会 panic（与下方 Up 分支同一模式）。
            if text_sel.read().dragging {
                // 与正常 Up 分支一致（复制后 clear() 全清 start/end/dragging）；
                // 用 write() 触发 wake，渲染清除高亮，避免弹窗关闭后选区残留。
                text_sel.write().clear();
            }
            // [弹窗滚轮放行] 居中弹窗（HITL 授权等）只覆盖屏幕中部，弹窗外消息区
            // 仍可见——滚轮事件在弹窗矩形外放行给消息区滚动（审批弹窗打开时
            // 用户仍可滚动 chat 查看上下文）；点击类事件保持遮挡（避免误触发
            // 文本选区起点/按钮）。
            if matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ) && !mouse_router::occludes_scroll(mouse.column, mouse.row)
            {
                // 放行：落入下方区域判定与滚动处理
            } else {
                return EventResult::Ignored;
            }
        }
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
                let max_scroll = fields.content_length.saturating_sub(fields.viewport_length);
                if let Some(geo) = compute_thumb_geometry(&fields, area) {
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) if on_scrollbar_col => {
                            // ▲ 端点（area 顶行）—— 直接跳到顶部
                            if mouse.row == area.y {
                                scroll_state.write_no_update().set_offset(0);
                                update_follow_on_scroll(follow_bottom, max_scroll, 0);
                                return EventResult::Consumed;
                            }
                            // ▼ 端点（area 底行）—— 直接跳到底部
                            let bottom_row = area.y.saturating_add(area.height).saturating_sub(1);
                            if mouse.row == bottom_row {
                                scroll_state.write_no_update().set_offset(max_scroll);
                                update_follow_on_scroll(follow_bottom, max_scroll, max_scroll);
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
                                let target =
                                    position_to_scroll_y(position, geo.max_position, max_scroll);
                                scroll_state.write_no_update().set_offset(target);
                                update_follow_on_scroll(follow_bottom, max_scroll, target);
                            }
                            // 记录拖拽状态——render 不依赖 active，用 write_no_update
                            {
                                let mut s = scrollbar_drag.write_no_update();
                                s.active = true;
                                s.thumb_offset = thumb_offset;
                                s.last_flush = Instant::now();
                            }
                            // 清除手势按下记录，防止 fallthrough 冲突
                            *gesture.write_no_update() = None;
                            return EventResult::Consumed;
                        }
                        MouseEventKind::Drag(MouseButton::Left) if drag_active => {
                            // 16ms 节流——和滚轮 / 文本 Drag 保持一致
                            let now = Instant::now();
                            {
                                let d = scrollbar_drag.read();
                                if now.duration_since(d.last_flush)
                                    < Duration::from_millis(scroll_frame_ms())
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
                            let target =
                                position_to_scroll_y(position, geo.max_position, max_scroll);
                            scroll_state.write_no_update().set_offset(target);
                            update_follow_on_scroll(follow_bottom, max_scroll, target);
                            return EventResult::Consumed;
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            if drag_active {
                                scrollbar_drag.write_no_update().active = false;
                            }
                            // [hygiene] 消息区内按下后拖到滚动条列释放的手势不会
                            // 经过下方 Up 分支——这里收尾复位，避免 Pending 残留。
                            *gesture.write_no_update() = None;
                            return EventResult::Consumed;
                        }
                        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                            // 滚轮在滚动条列也响应——fallthrough 到下面的滚动处理
                        }
                        _ => {
                            // [hygiene] 滚动条列上的其他左键事件（非 active 拖拽的
                            // Drag 等）——复位手势，避免 Pending 残留被下一次 Up 消费。
                            *gesture.write_no_update() = None;
                            return EventResult::Consumed;
                        }
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
                        // 记录手势意图（Pending）：冻结屏幕坐标 + 一次性换算的
                        // 内容坐标 + entry header 命中测试。不改任何可视状态——
                        // 真实拖动（Drag 超容差）才升级为拖拽。
                        // [TRAP] gesture 只在事件处理器内读写，render 不依赖
                        // 它——用 write_no_update 避免 wake 噪音（render 不需要
                        // 因为这个状态变化而重渲染，后续 Drag 升级才是真正的
                        // 渲染触发点）。
                        let scroll_y = scroll_state.read().offset();
                        // [usize 视觉行] 滚动偏移 usize 化后，视觉行直接相加——
                        // 超长内容（>65535 视觉行）下选中中间行不再被 u16 clamp 截断错位。
                        let visual_row = mouse.row.saturating_sub(area.y) as usize + scroll_y;
                        // 视口裁剪后无边框，visual_col 直接 = mouse.column - area.x
                        let visual_col = mouse.column.saturating_sub(area.x);
                        let pending = freeze_down(
                            (mouse.column, mouse.row),
                            (visual_row, visual_col),
                            wrap_map,
                            slot_offsets,
                        );
                        *gesture.write_no_update() = Some(pending);
                        return EventResult::Consumed;
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        // [升级判定先于节流] 决策提取为纯函数 drag_step——
                        // 升级瞬间不受节流：否则 <50ms 的快速拖拽首个 Drag 被
                        // 节流吞掉，升级永不发生，Up 误判单击（误折叠 + 复制
                        // 丢失）。容差内（手抖）手势保持 Pending，节流照常。
                        // [TRAP] 先 copy 出 gesture 值 drop guard 再 write——
                        // parking_lot 同 thread read+write 冲突会 panic。
                        let pending = *gesture.read();
                        let now = Instant::now();
                        let within_throttle_window = {
                            let dt = drag_throttle.read();
                            now.duration_since(dt.last_flush)
                                < Duration::from_millis(scroll_frame_ms())
                        };
                        match drag_step(pending, (mouse.column, mouse.row), within_throttle_window)
                        {
                            DragAction::Upgrade(p) => {
                                // ── 升级为拖拽（Armed）──
                                let scroll_y = scroll_state.read().offset();
                                // [usize 视觉行] 同 Down 分支：不做 u16 clamp，
                                // 超长内容选区不错位。
                                let visual_row =
                                    mouse.row.saturating_sub(area.y) as usize + scroll_y;
                                let visual_col = mouse.column.saturating_sub(area.x);
                                // 单次 write guard，drop 时只 wake 一次（不是两次）
                                // start_drag + update_drag 合并到同一 guard 内
                                {
                                    let mut sel_guard = text_sel.write();
                                    // start 恒为 Down 冻结的 visual——升级判定
                                    // 前移后 start_drag 只在升级瞬间调用一次
                                    // （不受节流）。
                                    sel_guard.start_drag(p.visual.0, p.visual.1);
                                    sel_guard.update_drag(visual_row, visual_col);
                                }
                                // [Armed 表达] 升级后 gesture 复位（None）：
                                // 拖拽状态归 TextSelection 所有（dragging=true
                                // 即 Armed 指示），Up 结算只看 dragging。
                                *gesture.write_no_update() = None;
                                return EventResult::Consumed;
                            }
                            DragAction::Throttled => return EventResult::Consumed,
                            DragAction::UpdateOnly => {
                                // 节流通过（未升级）——刷新节流时间戳
                                drag_throttle.write_no_update().last_flush = now;
                                // gesture 为 None（无 Down 记录，如事件丢失）→
                                // 现状行为：update_drag 空转（dragging=false 时
                                // no-op；已升级后的拖拽延续则跟随鼠标）。
                                let scroll_y = scroll_state.read().offset();
                                let visual_row =
                                    mouse.row.saturating_sub(area.y) as usize + scroll_y;
                                let visual_col = mouse.column.saturating_sub(area.x);
                                text_sel.write().update_drag(visual_row, visual_col);
                                return EventResult::Consumed;
                            }
                            DragAction::KeepPending => {
                                // 容差内（手抖）：Pending 原样保留，零副作用——
                                // 仅刷新节流时间戳（与现状节流逻辑一致）。
                                drag_throttle.write_no_update().last_flush = now;
                                return EventResult::Consumed;
                            }
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        // 单击结算分工（依赖 dispatch 注册序）：mod.rs 单击 handler
                        // 先消费 Pending 命中（Consumed）；未命中（Ignored）落到这里。
                        // Armed（dragging）→ 复制流程；Pending 未命中 → 复位 gesture。
                        // [TRAP] 同 Drag 处理：必须 copy 出 text_sel 状态后再 write，
                        // 否则 read+write 同 thread 冲突 panic。
                        let dragging = text_sel.read().dragging;
                        // 决策提取为纯函数 settle_up（可独立测试）：Pending 未命中
                        // （gesture 仍为 Some）→ 复位，手势生命周期结束；Armed 时
                        // gesture 已在升级瞬间复位为 None，无需再写。
                        if settle_up(dragging, gesture.read().is_some()) {
                            *gesture.write_no_update() = None;
                        }
                        if !dragging {
                            return EventResult::Consumed;
                        }
                        // 先 copy 出 normalized_bounds（owned Option），drop read guard
                        let bounds = text_sel.read().normalized_bounds();
                        let extracted: Option<String> = if let Some(((sr, sc), (er, ec))) = bounds {
                            extract_visual_range(
                                slot_arcs.as_ref(),
                                slot_offsets.as_ref(),
                                wrap_map,
                                (sr, sc),
                                (er, ec),
                                vis_width,
                                view_models,
                                grid,
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
                        // 清除选区和手势按下记录
                        *text_sel.write() = TextSelection::new();
                        *gesture.write_no_update() = None;
                        return EventResult::Ignored;
                    }
                    _ => {
                        // [S4 hygiene] 区域外 Up 顺手清 gesture——消息区内 Down
                        // 拖出区域外释放是常见路径（拖选拖出窗口），gesture 残留
                        // 至下一次 Down 虽必被覆盖，但"Down 事件丢失"场景下残留
                        // entry_hit 会被 Up 误消费（S1 review L1）。
                        if matches!(
                            mouse.kind,
                            MouseEventKind::Up(MouseButton::Left)
                                | MouseEventKind::Drag(MouseButton::Left)
                        ) {
                            *gesture.write_no_update() = None;
                            return EventResult::Ignored;
                        }
                    }
                }
            }

            // ── 滚动处理（区域内外通用）──
            match mouse.kind {
                MouseEventKind::ScrollDown => apply_scroll(
                    SCROLL_LINES as i32,
                    scroll_throttle,
                    scroll_state,
                    scrollbar_fields,
                    follow_bottom,
                ),
                MouseEventKind::ScrollUp => apply_scroll(
                    -(SCROLL_LINES as i32),
                    scroll_throttle,
                    scroll_state,
                    scrollbar_fields,
                    follow_bottom,
                ),
                _ => {}
            }
        }

        // 所有非 Moved/Drag 鼠标事件标记为已消费（防止泄漏到下层组件）
        return EventResult::Consumed;
    }

    // 键盘滚动（替代 ScrollViewState::handle_event——消息区不使用 ScrollView，
    // 其 page_size 永远为 None；自持 ScrollPos 直接滚动）。
    // focus_router::message_accepts_key 仅放行 Ctrl+Up/Down/Home/End。
    // [Why 无翻页分支] 项目约束禁止 PageUp/PageDown 作快捷键
    // （spec/global/domains/tui/tui-index.md：部分终端模拟器将其绑定到终端自身
    // 滚动缓冲、不送达应用，且本消息区 Global/High handler 一旦放行会先于
    // InputArea 消费，破坏输入区行为）。若未来改用 Ctrl+U/Ctrl+D 等组合键实现
    // 翻页，需同时更新 focus_router::message_accepts_key 与 input_accepts_key。
    if let Event::Key(key) = &event
        && key.kind == KeyEventKind::Press
        && focus_router::message_accepts_key(key)
    {
        // 跟随态下 offset 可能是 usize::MAX 哨兵——先归一化到当帧底部，否则
        // Up/Down 要先消耗巨大偏移（"滚空气"）才生效。
        let fields = *scrollbar_fields.read();
        let max_scroll = fields.content_length.saturating_sub(fields.viewport_length);
        let mut st = scroll_state.write();
        if st.offset() > max_scroll {
            st.set_offset(max_scroll);
        }
        match key.code {
            KeyCode::Up => st.scroll_up(),
            KeyCode::Down => st.scroll_down(),
            KeyCode::Home => st.scroll_to_top(),
            KeyCode::End => st.scroll_to_bottom(),
            _ => return EventResult::Ignored,
        }
        drop(st);
        // [TRAP] read+write 同 state 同线程冲突——write guard 已 drop 再 read。
        update_follow_on_scroll(follow_bottom, max_scroll, scroll_state.read().offset());
        return EventResult::Consumed;
    }
    EventResult::Ignored
}

// ── 吸底自动跟随 ─────────────────────────────────────────────────────────

/// `use_effect` 闭包提取的上下文结构体。
/// 所有 `State<T>` 字段在 mod.rs 闭包外构造时用 `.clone()`（State 是 Arc，clone 是廉价引用拷贝）。
pub(super) struct AutoFollowCtx {
    pub total_visual_rows: usize,
    pub vis_height: u16,
    pub scroll_state: State<ScrollPos>,
    pub prev_items_len: State<usize>,
    pub last_scrolled_at: State<usize>,
    pub items_len: usize,
    pub is_loading: bool,
    /// 粘性吸底开关：用户一向上滚动即 false（浏览模式），滚回真正底部才恢复 true。
    /// 跟随态下内容增长无条件滚底；浏览态下不打扰。
    pub follow_bottom: State<bool>,
    /// 用于检测 resize：total_visual_rows 变化后钳制 scroll_state.offset 到有效范围。
    pub prev_total_visual_rows: State<usize>,
    /// 用于检测 resize：vis_height 变化（终端高度变化）后，若处于跟随态则跟随到底。
    /// use_effect 依赖不含 vis_height，此哨兵负责补上这个缺口。
    pub prev_vis_height: State<u16>,
    /// 用于检测 submit（用户主动发送 prompt）→ 强制滚底，不经过 follow_bottom guard。
    pub loading_epoch: u64,
    pub prev_loading_epoch: State<u64>,
    /// 用于检测 history 切换 / /clear → 重置 prev_items_len/last_scrolled_at，
    /// 触发「新会话首次批量加载」的强制滚底路径。
    pub bridge_reset_counter: u64,
    pub prev_reset_counter: State<u64>,
    /// [Slice 4 §6.8]「等待时锚定此 block」：pending interaction block 的视觉
    /// 行范围（core 行，含起点/终点）。有值时视口对齐到 block 底部（浏览态与
    /// 跟随态均生效——§6.8：不得被新 streaming chunk 滚出视口）；block 完成
    /// （结果回写 → 派生扫描不到 → None）后恢复原语义，不强制 follow
    /// （§15「提交后转为只读结果行且不抢回 viewport」）。
    pub anchor_visual_range: Option<(usize, usize)>,
}

/// 从 `use_effect` 闭包提取的吸底逻辑。
/// 注意：use_effect body 不是 render body，所以 `write()` 是正确的（需要 wake 触发后续渲染）。
pub(super) fn run_auto_follow(ctx: &AutoFollowCtx) {
    // [Diagnostic] 记录每次 effect 触发的关键参数——trace 历史/submit 两个滚动问题。
    // [Perf] run_auto_follow 随 vm_generation 每 token 触发，info 级日志在默认
    // filter 下逐 token 同步落盘（RollingFileAppender），多 Agent 并发时放大为每秒
    // 数百次文件写。热路径诊断统一降为 trace 级，按需开启
    // `RUST_LOG=...msg_scroll_diag=trace` 排查；低频事件（submit/thread_load 等
    // consumer）仍保持 info。
    tracing::trace!(
        target: "msg_scroll_diag",
        items_len = ctx.items_len,
        total_rows = ctx.total_visual_rows,
        vis_h = ctx.vis_height,
        is_loading = ctx.is_loading,
        follow = *ctx.follow_bottom.read(),
        scroll_y = ctx.scroll_state.read().offset(),
        prev_lsa = *ctx.last_scrolled_at.read(),
        prev_items = *ctx.prev_items_len.read(),
        "auto_follow: entry",
    );

    // [Fix] resize 后 total_visual_rows 变化时，主动钳制 scroll_state.offset 到有效范围。
    let prev_total = *ctx.prev_total_visual_rows.read();
    *ctx.prev_total_visual_rows.write() = ctx.total_visual_rows;
    if prev_total != ctx.total_visual_rows && ctx.total_visual_rows > 0 && ctx.vis_height > 0 {
        let max_scroll = ctx
            .total_visual_rows
            .saturating_sub(ctx.vis_height as usize);
        let current_y = ctx.scroll_state.read().offset();
        if current_y > max_scroll {
            ctx.scroll_state.write().set_offset(max_scroll);
        }
    }

    // [Fix] resize 高度变化（vis_height 变）后跟随底部。
    // [Why] use_effect 依赖（items_len, vm_generation, is_loading, total_visual_rows）不含
    // vis_height，终端高度变化时 effect 不触发；而渲染侧 clamp 只在 offset > max_scroll
    // 时钳制上限——resize 缩小视口后 max_scroll 变大，offset 停在旧底部不再到底，底部的
    // footer（2 空行 + spinner）被挤出视口。
    // 判定改为 follow_bottom：跟随态（用户没在浏览）resize 后跟随到底；浏览态不打扰。
    // 旧版用 proximity 阈值（视口 1/4）判定，浏览态距底 ≤ 阈值时仍会被误拉。
    let prev_vis = *ctx.prev_vis_height.read();
    *ctx.prev_vis_height.write() = ctx.vis_height;
    if prev_vis != ctx.vis_height
        && *ctx.follow_bottom.read()
        && ctx.total_visual_rows > 0
        && ctx.vis_height > 0
    {
        tracing::trace!(
            target: "msg_scroll_diag",
            prev_vis,
            new_vis = ctx.vis_height,
            "auto_follow: resize (vis_height changed) → follow bottom",
        );
        ctx.scroll_state.write().scroll_to_bottom();
        *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
    }

    // ── [Fix #1] Submit 强制滚底：用户主动发送 prompt 时 LOADING_EPOCH 递增 ──
    // 当前 effect 可能在 user bubble 到达 VIEW_MODELS 之前触发（submit_consumer
    // 先设 is_loading=true，再 call prompt() RPC）。此时 scroll_to_bottom 定位
    // 到当前的底部位置即可——user bubble 到达后 proximity 自然跟随。
    let prev_epoch = *ctx.prev_loading_epoch.read();
    *ctx.prev_loading_epoch.write() = ctx.loading_epoch;
    if ctx.loading_epoch != prev_epoch && ctx.total_visual_rows > 0 && ctx.vis_height > 0 {
        tracing::trace!(
            target: "msg_scroll_diag",
            prev_epoch,
            new_epoch = ctx.loading_epoch,
            "auto_follow: submit detected (LOADING_EPOCH changed) → force scroll_to_bottom",
        );
        ctx.scroll_state.write().scroll_to_bottom();
        *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
        *ctx.follow_bottom.write() = true;
        // 不 return——继续走后续逻辑处理 user bubble / 流式增长
    }

    // ── [Fix #2] History 切换 / /clear 检测：BRIDGE_RESET_COUNTER 递增时重置哨兵 ──
    // prev_items_len←0 和 last_scrolled_at←0 一起作为「新会话首次批量加载」的哨兵：
    // 后续的 prev==0 分支（在所有 proximity guard 之前）强制每批 scroll_to_bottom，
    // 且不消费 prev==0（保持 trigger 活跃至 replay 结束）。
    let prev_ctr = *ctx.prev_reset_counter.read();
    *ctx.prev_reset_counter.write() = ctx.bridge_reset_counter;
    if ctx.bridge_reset_counter != prev_ctr {
        tracing::trace!(
            target: "msg_scroll_diag",
            prev_ctr,
            new_ctr = ctx.bridge_reset_counter,
            "auto_follow: BRIDGE_RESET_COUNTER changed → arming prev==0 force-scroll",
        );
        *ctx.prev_items_len.write() = 0;
        *ctx.last_scrolled_at.write() = 0;
    }

    // [TRAP] parking_lot 同 thread 死锁规避：先 read copy 出 owned，guard 在语句末尾 drop，再 write。
    let prev = *ctx.prev_items_len.read();

    // ── 零内容保护 ──
    if ctx.total_visual_rows == 0 || ctx.vis_height == 0 {
        *ctx.prev_items_len.write() = ctx.items_len;
        tracing::trace!(target: "msg_scroll_diag", "auto_follow: early return (zero total or vis)");
        return;
    }

    // ── [Fix #3] History replay 批量强制滚底（哨兵 prev==0）──
    // 仅在 non-loading 且 「BRIDGE_RESET_COUNTER 递增触发了 prev_items_len 归零」时进入。
    // 每批次都 force scroll + 再次将 prev_items_len 归零——直到 replay 结束，
    // generation 不再增长、effect 停发，prev==0 自然消弭。
    if prev == 0 && !ctx.is_loading && ctx.items_len > 0 {
        tracing::trace!(
            target: "msg_scroll_diag",
            items_len = ctx.items_len,
            "auto_follow: prev==0 force-scroll (history replay batch) → scroll_to_bottom",
        );
        ctx.scroll_state.write().scroll_to_bottom();
        *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
        *ctx.follow_bottom.write() = true;
        // 不消费 prev==0——维持为 0 让后续 batch 也走此路径
        *ctx.prev_items_len.write() = 0;
        return;
    }

    // ── 正常路径：更新 prev_items_len ──
    *ctx.prev_items_len.write() = ctx.items_len;

    // ── [Slice 4 §6.8] Interaction block 锚定 ──
    // pending interaction block 存在时，block 末行超出视口 → 视口对齐到 block
    // 底部。**仅跟随态生效**（§6.8 字面「等待时 follow mode 锚定此 block」）：
    // 浏览态（用户滚离底部）下新内容不得移动 viewport（§8.1），锚定分支在
    // `!follow_bottom` 早退之前——跟随态下锚定优先于粘性跟随判定；resize 后
    // 按新快照重算（prev_vis_height 路径共存，anchor 分支在其后覆盖对齐目标）。
    // block 完成（pending=false → 派生扫描不到 → None）后恢复原语义，不强制
    // follow（§15）。[Fix] 浏览态下每帧被拉回 block 底部会打断用户阅读——
    // 与 §8.1「浏览态新内容不得移动 viewport」相悖，改为浏览态跳过锚定。
    if *ctx.follow_bottom.read()
        && let Some((_anchor_start, anchor_end)) = ctx.anchor_visual_range
    {
        let max_scroll = ctx
            .total_visual_rows
            .saturating_sub(ctx.vis_height as usize);
        let scroll_y = ctx.scroll_state.read().offset();
        if let Some(target) =
            anchor_scroll_target(scroll_y, ctx.vis_height as usize, anchor_end, max_scroll)
        {
            tracing::trace!(
                target: "msg_scroll_diag",
                anchor_end,
                scroll_y,
                target,
                "auto_follow: interaction anchor → align viewport to block bottom",
            );
            ctx.scroll_state.write().set_offset(target);
            *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
        }
        return;
    }

    // ── 粘性跟随 guard ──
    // [Why] 用户一旦向上滚动（offset < max_scroll，update_follow_on_scroll 已置
    // false）即进入浏览模式：内容增长不再吸回，可自由翻看历史。
    // 只有滚回真正底部（或 submit / replay / shrink 等结构性事件）才恢复跟随。
    // 旧版只有 proximity 阈值（视口 1/4）：loading 中上滚 ≤ 阈值会在下一次内容
    // 增长时被吸回，反复拉锯；且内容单帧跳增超过阈值时跟随被拒绝，视口停在
    // 半空、spinner 消失——底部跳动。粘性语义下这两类问题都不存在。
    if !*ctx.follow_bottom.read() {
        tracing::trace!(target: "msg_scroll_diag", "auto_follow: browsing (follow=false) → skip");
        return;
    }

    let prev_lsa = *ctx.last_scrolled_at.read();

    if ctx.is_loading {
        if ctx.total_visual_rows > prev_lsa {
            tracing::trace!(target: "msg_scroll_diag", "auto_follow: loading → scroll_to_bottom");
            ctx.scroll_state.write().scroll_to_bottom();
            *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
        } else {
            tracing::trace!(target: "msg_scroll_diag", total = ctx.total_visual_rows, prev_lsa, "auto_follow: loading → skip (total_rows not greater than prev_lsa)");
        }
        return;
    }

    if ctx.items_len < prev {
        tracing::trace!(target: "msg_scroll_diag", items_len = ctx.items_len, prev, "auto_follow: shrink → scroll_to_bottom");
        ctx.scroll_state.write().scroll_to_bottom();
        *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
        *ctx.follow_bottom.write() = true;
        return;
    }

    if ctx.total_visual_rows > prev_lsa {
        tracing::trace!(target: "msg_scroll_diag", "auto_follow: non-loading growth → scroll_to_bottom");
        ctx.scroll_state.write().scroll_to_bottom();
        *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
    } else {
        tracing::trace!(target: "msg_scroll_diag", total = ctx.total_visual_rows, prev_lsa, "auto_follow: non-loading → skip (total_rows not greater than prev_lsa)");
    }
}

// ── 测试 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "scroll_test.rs"]
mod tests;
