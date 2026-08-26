use std::sync::Arc;
use std::time::{Duration, Instant};

use super::entry_nav::{
    apply_fold_toggle, cycle_interaction_option, entry_click_decision, move_entry_focus,
    pending_interaction_of, set_entry_focus, submit_interaction_option,
};
use super::grid::GridSpec;
use super::hits::{CopyButtonHit, ImageLineHit, InteractionOptionHit};
use super::image_action::{hover_target_for, try_open_image};
use super::props::ScrollbarFields;
use super::scroll::{self, DragThrottle, ScrollThrottle, ScrollbarDragState};
use super::selection::{WrappedLineInfo, copy_to_clipboard, mark_copy_message};
use crate::kit::atoms::{
    FOCUSED_ENTRY, IMAGE_HOVER, KEEPGOING_BLOCKED_UNTIL, RENDER_HEARTBEAT, SUBMIT_TX, VIEW_MODELS,
    ViewModelsSnapshot,
};
use crate::kit::focus_router;
use crate::kit::mouse_router;
use crate::kit::submit_request::SubmitRequest;
use crate::kit::text_selection::TextSelection;
use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit};
use ratatui_kit::crossterm::event::{
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::text::Line;

/// keepgoing 按钮点击防抖时长（连续点击冷却）。
const KEEPGOING_DEBOUNCE: Duration = Duration::from_millis(1500);

// ── keepgoing 按钮点击（footer summary 行右侧）──
// [Why] 必须注册在 scroll handler 之前：两者同 Global+High，同优先级按注册序分发，
// scroll::handle_event 对消息区内 Down(Left) 一律 Consumed（文本选中起点）——
// 若在其后注册，按钮点击会被 scroll handler 截断、永远收不到。
// 命中 → Consumed（scroll handler 不处理该点击，不会误设选区起点）；
// 未命中 → Ignored（滚动/选区逻辑照常）。
// [TRAP] 闭包捕获 State 句柄（keepgoing_rect）而非帧快照——每帧 write_no_update
// 更新 rect，滚动/布局变化后坐标仍准确；事件在上帧渲染完成后分发，读取的
// 是最近一帧的按钮位置。
pub(super) fn register_keepgoing_click(
    hooks: &mut Hooks,
    keepgoing_rect: State<Option<(u16, u16, u16)>>,
) {
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        let Event::Mouse(mouse) = event else {
            return EventResult::Ignored;
        };
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return EventResult::Ignored;
        }
        // 弹窗/面板遮挡时不响应（与 status_bar / scroll handler 一致）
        if mouse_router::is_occluded() {
            return EventResult::Ignored;
        }
        // 命中检测：点击坐标落在按钮屏幕区域内 (y, x_start, width)
        // [Why 先于防抖] 防抖期内点击禁用按钮也应 Consumed——否则事件落到
        // scroll handler 的文本选区逻辑（消息区内 Down(Left) 设置选区锚点）。
        let Some((by, bx, bw)) = *keepgoing_rect.read() else {
            return EventResult::Ignored;
        };
        let (x, y) = (mouse.column, mouse.row);
        if y != by || x < bx || x >= bx.saturating_add(bw) {
            return EventResult::Ignored;
        }
        // 防抖：防抖期内按钮渲染为禁用样式，点击被吞掉但不触发提交
        let now = Instant::now();
        let blocked = KEEPGOING_BLOCKED_UNTIL
            .state()
            .read()
            .is_some_and(|until| now < until);
        if blocked {
            return EventResult::Consumed;
        }
        // 触发 keepgoing 提交：发送空白 user prompt（服务端不插入 user 消息，仅继续 loop）
        if let Some(tx) = SUBMIT_TX.get() {
            let _ = tx.send(SubmitRequest::KeepGoing);
        }
        // 防抖：冷却期内按钮禁用（渲染为 muted 样式，见 build_footer_lines）
        *KEEPGOING_BLOCKED_UNTIL.state().write() = Some(now + KEEPGOING_DEBOUNCE);
        // 防抖到期后清除阻塞并 bump 心跳触发重渲染，恢复可点击样式
        tokio::spawn(async move {
            tokio::time::sleep(KEEPGOING_DEBOUNCE).await;
            *KEEPGOING_BLOCKED_UNTIL.state().write() = None;
            RENDER_HEARTBEAT.set(RENDER_HEARTBEAT.get().wrapping_add(1));
        });
        EventResult::Consumed
    });
}

// ── md 复制按钮点击（复制整条 AI 回复的原始 markdown）──
// [Why] 注册顺序与 keepgoing 相同：必须在 scroll handler 之前——scroll::handle_event
// 对消息区内 Down(Left) 一律 Consumed（文本选中起点），在其后注册收不到点击。
// 命中 → Consumed（滚动/选区逻辑不处理该点击，不会误设选区起点）；
// 未命中 → Ignored（滚动/选区逻辑照常）。
// [Why 每次渲染重建] ratatui-kit 的 use_event_handler 闭包每帧重新注册（当帧值），
// copy_buttons State 由渲染 body 后部 write_no_update 更新——事件分发时读到的
// 是最近一帧的按钮位置（与 keepgoing 一致）。
pub(super) fn register_md_copy_click(
    hooks: &mut Hooks,
    copy_buttons: State<Arc<Vec<CopyButtonHit>>>,
    view_models: AtomState<ViewModelsSnapshot>,
) {
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        let Event::Mouse(mouse) = event else {
            return EventResult::Ignored;
        };
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return EventResult::Ignored;
        }
        // 弹窗/面板遮挡时不响应（与 status_bar / scroll handler 一致）
        if mouse_router::is_occluded() {
            return EventResult::Ignored;
        }
        let (x, y) = (mouse.column, mouse.row);
        let hits = copy_buttons.read();
        let Some(hit) = hits
            .iter()
            .find(|h| y == h.row && x >= h.x_start && x < h.x_end)
        else {
            return EventResult::Ignored;
        };
        // 读取最新 VM 文本：校验 hash 防 Rewind/Reset 后索引错位。
        // [LOW-5] assistant 用稳定身份 hash 比对（排除时变 duration——
        // 运行中 bubble 的 content_hash 每秒漂移，跨秒点击偶发拒绝）；
        // 其余类型沿用 content_hash。
        let snapshot = view_models.read();
        let matched = snapshot
            .items
            .get(hit.slot_index)
            .is_some_and(|vm| match vm {
                TuiRenderUnit::TuiAssistantBubble(b) => {
                    TuiAssistantBubble::stable_identity_hash(&b.text, b.reasoning.as_ref())
                        == hit.vm_hash
                }
                _ => vm.content_hash() == hit.vm_hash,
            });
        let text = if matched {
            match &snapshot.items[hit.slot_index] {
                TuiRenderUnit::TuiAssistantBubble(d) => Some(d.text.clone()),
                _ => None,
            }
        } else {
            None
        };
        drop(snapshot);
        if let Some(text) = text {
            copy_to_clipboard(text.clone());
            mark_copy_message(text.chars().count());
        }
        // 命中按钮（即使 VM 不匹配）也 Consumed——防止点击落到文本选区逻辑
        EventResult::Consumed
    });
}

// ── `↓ New output` 指示器点击（§8.1：滚回底部并恢复跟随）──
// [Why 注册顺序] 必须注册在 scroll handler（下方）之前：scroll::handle_event
// 对消息区内 Down(Left) 一律 Consumed（文本选中起点）——在其后注册收不到点击。
// 命中 → Consumed（滚动/选区逻辑不处理该点击）；未命中 → Ignored。
// [TRAP] 闭包捕获 State 句柄（new_output_rect）而非帧快照——每帧
// write_no_update 更新 rect，滚动/布局变化后坐标仍准确。
pub(super) fn register_new_output_indicator_click(
    hooks: &mut Hooks,
    new_output_rect: State<Option<(u16, u16, u16)>>,
    follow_bottom: State<bool>,
    scroll_state: State<scroll::ScrollPos>,
) {
    let new_output_rect_state = new_output_rect;
    let follow_state = follow_bottom;
    let scroll_state_for_indicator = scroll_state;
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        let Event::Mouse(mouse) = event else {
            return EventResult::Ignored;
        };
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return EventResult::Ignored;
        }
        // 弹窗/面板遮挡时不响应（与 keepgoing / scroll handler 一致）
        if mouse_router::is_occluded() {
            return EventResult::Ignored;
        }
        let Some((ry, rx_start, rx_end)) = *new_output_rect_state.read() else {
            return EventResult::Ignored;
        };
        let (x, y) = (mouse.column, mouse.row);
        if y != ry || x < rx_start || x >= rx_end {
            return EventResult::Ignored;
        }
        // 恢复跟随 + 滚到底（渲染每帧 clamp scroll_to_bottom 的 usize::MAX
        // 哨兵到当帧 max_scroll——与 End 键同一路径）。
        *follow_state.write() = true;
        scroll_state_for_indicator
            .write_no_update()
            .scroll_to_bottom();
        EventResult::Consumed
    });
}

// ── [Slice 4 §6.8] interaction option 点击（提交该选项，按钮语义）──
// [Why 注册顺序] 与 keepgoing/md 复制/new output 一致：必须在 scroll
// handler 之前——scroll::handle_event 对消息区内 Down(Left) 一律 Consumed
// （文本选中起点），在其后注册收不到点击。命中 → Consumed；未命中 → Ignored。
pub(super) fn register_interaction_option_click(
    hooks: &mut Hooks,
    interaction_rects: State<Arc<Vec<InteractionOptionHit>>>,
    view_models: AtomState<ViewModelsSnapshot>,
) {
    let interaction_rects_state = interaction_rects;
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        let Event::Mouse(mouse) = event else {
            return EventResult::Ignored;
        };
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return EventResult::Ignored;
        }
        // 弹窗/面板遮挡时不响应（与 keepgoing / scroll handler 一致）
        if mouse_router::is_occluded() {
            return EventResult::Ignored;
        }
        let (x, y) = (mouse.column, mouse.row);
        let hits = interaction_rects_state.read();
        let Some(hit) = hits
            .iter()
            .find(|h| y == h.row && x >= h.x_start && x < h.x_end)
        else {
            return EventResult::Ignored;
        };
        // 校验 VM 身份（Rewind/Reset 索引错位防御）——与 md 复制按钮同模式。
        let vm_guard = view_models.read();
        let block = vm_guard
            .items
            .get(hit.slot_index)
            .filter(|vm| vm.content_hash() == hit.vm_hash)
            .and_then(pending_interaction_of)
            .cloned();
        drop(vm_guard);
        if let Some(block) = block {
            submit_interaction_option(&block, hit.option_index);
        }
        // 命中（即使 VM 不匹配）也 Consumed——防止点击落到文本选区逻辑
        EventResult::Consumed
    });
}

// ── [T4 §4] @image 行点击（open 图片文件）──
// [Why 注册顺序] 与 keepgoing/md 复制/interaction 一致：必须在 scroll
// handler 之前——scroll::handle_event 对消息区内 Down(Left) 一律 Consumed
// （文本选中起点），在其后注册收不到点击。命中 → Consumed；未命中 → Ignored。
// 打开前过 T5 校验（常规文件 + 扩展名 + 大小上限）；校验失败 → NOTIFICATION
// 提示（paste-truncated 通知模式）。open 用参数化 Command（§6.2-6 禁止 shell
// 拼接），macOS 验证；其他平台未验证前仅记录日志不 spawn（§4.6 安全降级）。
pub(super) fn register_image_click(
    hooks: &mut Hooks,
    image_rects: State<Arc<Vec<ImageLineHit>>>,
    view_models: AtomState<ViewModelsSnapshot>,
) {
    let image_rects_for_click = image_rects;
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        let Event::Mouse(mouse) = event else {
            return EventResult::Ignored;
        };
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return EventResult::Ignored;
        }
        // 弹窗/面板遮挡时不响应（与 keepgoing / scroll handler 一致）
        if mouse_router::is_occluded() {
            return EventResult::Ignored;
        }
        let (x, y) = (mouse.column, mouse.row);
        let hits = image_rects_for_click.read();
        let Some(hit) = hits
            .iter()
            .find(|h| y == h.row && x >= h.x_start && x < h.x_end)
        else {
            return EventResult::Ignored;
        };
        // 校验 VM 身份（Rewind/Reset 索引错位防御）——同 md 复制按钮模式。
        let vm_guard = view_models.read();
        let matched = vm_guard.items.get(hit.slot_index).is_some_and(|vm| {
            matches!(vm, TuiRenderUnit::TuiUserBubble(_)) && vm.content_hash() == hit.vm_hash
        });
        drop(vm_guard);
        if matched {
            try_open_image(&hit.path);
        }
        // 命中（即使 VM 不匹配）也 Consumed——防止点击落到文本选区逻辑
        EventResult::Consumed
    });
}

// ── [T4 §4] @image 行 hover（绝对路径 + accent 高亮）──
// [Why 注册顺序] 注册在 scroll handler 之前（scroll.rs 对 Moved 直接
// Ignored，顺序在其前即可收到）；Moved 恒 Ignored，不消费事件。
// [防风暴 §4.6] 仅当「命中集合变化」时 write（触发重渲染）；命中不变
// 的移动 no-op——防高频 Moved 每帧全量重渲染消息区。
pub(super) fn register_image_hover(hooks: &mut Hooks, image_rects: State<Arc<Vec<ImageLineHit>>>) {
    let image_rects_for_hover = image_rects;
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        let Event::Mouse(mouse) = event else {
            return EventResult::Ignored;
        };
        if mouse.kind != MouseEventKind::Moved {
            return EventResult::Ignored;
        }
        let new_state = hover_target_for(
            &image_rects_for_hover.read(),
            mouse.column,
            mouse.row,
            mouse_router::is_occluded(),
        );
        // [TRAP] 先 copy 出当前值 drop guard 再 write——parking_lot 同线程
        // read+write 冲突会 panic。
        let current = IMAGE_HOVER.state().read().clone();
        if current != new_state {
            *IMAGE_HOVER.state().write() = new_state;
        }
        EventResult::Ignored
    });
}

// ── entry 单击展开（仅首行 header；与键盘 Enter 同语义）──
// [Why 注册顺序] 必须在 scroll handler 之前：scroll::handle_event 对消息区内
// Up(Left) 也会消费（选区复制/清锚点），在其后注册收不到单击。放在
// interaction option（Down）之后即可——两者事件类型不重叠。
// [语义] 单击 = Down 冻结 + 手势从未升级（gesture 保持 Pending 到 Up）：
// Up 只消费 Down 时冻结的结果（entry_hit），不再做坐标换算与反查。命中
// entry 首行 → 设置 entry 焦点 + 折叠切换：tool/reasoning/subagent/completed
// interaction toggle（写 FOLD_OVERRIDES，与键盘 Enter 一致）；subagent
// 打开详情面板；pending interaction 首行仅聚焦不提交（键盘 Enter 的提交
// 是明确按键语义）。未命中（手势已升级/滚动条列/非首行/坐标外）→
// Ignored，选区逻辑照常。
pub(super) fn register_entry_click(
    hooks: &mut Hooks,
    area_rect: Option<Rect>,
    gesture: State<Option<scroll::GesturePending>>,
    interaction_option: State<usize>,
    text_sel: State<TextSelection>,
) {
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        let Event::Mouse(mouse) = event else {
            return EventResult::Ignored;
        };
        if mouse.kind != MouseEventKind::Up(MouseButton::Left) {
            return EventResult::Ignored;
        }
        // 弹窗/面板遮挡时不响应（与 keepgoing / scroll handler 一致）
        if mouse_router::is_occluded() {
            return EventResult::Ignored;
        }
        let Some(area) = area_rect else {
            return EventResult::Ignored;
        };
        // [单击判定] 判定收敛为纯函数 entry_click_decision（mod_test 直调
        // 锁定）：单击 = Down 冻结 + 手势从未升级（gesture 保持 Pending
        // 到 Up）——升级瞬间 scroll.rs Drag 分支已复位 gesture 并置
        // text_sel.dragging（终端按下后任何微移都会报 Drag 事件，判定
        // 前移到 Drag 分支后，手抖保持在容差内即 Pending 原样保留）。
        // Up 只消费 Down 时冻结的 entry_hit，不再比较 Down/Up 坐标、不
        // 读 text_sel.dragging、不做 entry_click_target 反查——滚动
        // （scroll_y > 0）/ 网格前缀（area.x > 0）的坐标正确性由 Down
        // 冻结保证。防御检查（area 行界 / 滚动条列）基于 Up 坐标，见
        // 函数文档 [防御 Up 坐标]。
        // [TRAP] gesture.read() 返回临时 guard，as_ref() 的借用只在语句
        // 内有效——命中路径的写入（FOCUSED_ENTRY / gesture）发生在判定
        // 返回之后（guard 已 drop），parking_lot 同线程 read+write 冲突
        // 安全。
        let Some(slot) = entry_click_decision(
            gesture.read().as_ref(),
            mouse.row,
            area,
            scroll::is_scrollbar_column(mouse.column, area),
        ) else {
            return EventResult::Ignored;
        };
        // ── 命中 entry 首行：设焦点 + 折叠动作 ──
        // 与键盘 Alt+Up/Down 一致：焦点可落在任意 entry，FOCUSED_ENTRY
        // 的 key 仅 foldable 有值（无折叠能力 entry 合法 key: None）；
        // 重置 interaction option 到首项。
        tracing::trace!(target: "frozen_diag", slot, "click: hit entry, setting focus");
        *interaction_option.write() = 0;
        // 持 VIEW_MODELS 写锁期间不再读其他可能被同一帧写入的 atom
        //（FOLD_OVERRIDES / SELECTED_SUBAGENT_ID 是独立锁）——键盘同模式。
        tracing::trace!(target: "frozen_diag", slot, "click: acquiring VIEW_MODELS write lock");
        let vm_state_ref = VIEW_MODELS.state();
        let mut snapshot = vm_state_ref.write();
        tracing::trace!(target: "frozen_diag", slot, "click: got VIEW_MODELS write lock");
        if slot >= snapshot.items.len() {
            // 快照缩短（reset/rewind）——焦点失效，退出导航（键盘同模式）
            *FOCUSED_ENTRY.state().write() = None;
            // 手势已消费：所有命中 return 路径统一先复位 gesture
            //（dispatch 顺序执行，Consumed 后 scroll.rs Up 分支不运行）
            *gesture.write_no_update() = None;
            return EventResult::Consumed;
        }
        // 持写锁内派生 key（桥线程可并发写 VIEW_MODELS——锁外派生会读到
        // 漂移索引；key 与索引一致性由同一快照保证）。
        set_entry_focus(&snapshot, slot);
        // pending interaction：Enter 语义是提交 option（鼠标不承担）；
        // 首行点击仅聚焦，不提交不折叠。
        if pending_interaction_of(&snapshot.items[slot]).is_some() {
            *gesture.write_no_update() = None;
            return EventResult::Consumed;
        }
        // 点击 = 取消选区语义（与 keepgoing / md 复制按钮点击一致）
        text_sel.write().clear();
        *gesture.write_no_update() = None;
        let result = apply_fold_toggle(&mut snapshot, slot, false);
        tracing::trace!(target: "frozen_diag", slot, "click: handler exit");
        result
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn register_scroll_events(
    hooks: &mut Hooks,
    area_rect: Option<Rect>,
    vis_width: u16,
    scroll_state: State<scroll::ScrollPos>,
    scroll_throttle: State<ScrollThrottle>,
    text_sel: State<TextSelection>,
    gesture: State<Option<scroll::GesturePending>>,
    drag_throttle: State<DragThrottle>,
    scrollbar_fields: State<ScrollbarFields>,
    scrollbar_drag: State<ScrollbarDragState>,
    follow_bottom: State<bool>,
    view_models: AtomState<ViewModelsSnapshot>,
    grid: GridSpec,
    concat_wrap_map_arc: Arc<Vec<WrappedLineInfo>>,
    slot_arcs_arc: Arc<Vec<Arc<Vec<Line<'static>>>>>,
    slot_offsets_arc: Arc<Vec<usize>>,
) {
    let wrap_map_for_closure = Arc::clone(&concat_wrap_map_arc);
    let slot_arcs_for_closure = Arc::clone(&slot_arcs_arc);
    let slot_offsets_for_closure = Arc::clone(&slot_offsets_arc);
    let view_models_for_closure = view_models;
    let grid_for_closure = grid;
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        // [D3 §9] 语义复制：事件时点读快照 VM 列表（im::Vector clone O(1)，
        // 只读不改——与 parking_lot 读锁安全共存；选区提取需要 VM 类型
        // 分派语义文本，不能只靠已渲染行）。
        let vms_snapshot = view_models_for_closure.read().items.clone();
        scroll::handle_event(
            &event,
            area_rect,
            vis_width,
            &scroll_state,
            &scroll_throttle,
            &text_sel,
            &gesture,
            &drag_throttle,
            &wrap_map_for_closure,
            &slot_arcs_for_closure,
            &slot_offsets_for_closure,
            &scrollbar_fields,
            &scrollbar_drag,
            &follow_bottom,
            Some(&vms_snapshot),
            Some(grid_for_closure),
        )
    });
}

// ── entry 焦点导航（Slice 2 键盘语义层；selection border 视觉归 Slice 3）──
// Alt+Up/Down 移动 entry 焦点；焦点激活时 Enter 切 Collapsed/Expanded、
// Space 切 Preview（写 FOLD_OVERRIDES + user_modified）；Esc 退出导航。

// 键盘：Alt+Up/Down 移焦点；Enter/Space 切折叠（写覆盖表 + 当帧快照）；
// Tab/←/→ 切换 pending interaction 选项、Enter 提交（§6.8）；
// Esc 单层取消（退出导航）。仲裁见 focus_router::message_nav_accepts。
pub(super) fn register_keyboard_nav(hooks: &mut Hooks, interaction_option: State<usize>) {
    let option_state = interaction_option;
    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return EventResult::Ignored;
        }
        if mouse_router::is_occluded() {
            return EventResult::Ignored;
        }
        // Esc：仅焦点激活时消费（单层取消，退出导航）；未激活时放行给
        // root handler（双击 Esc → Rewind 等既有语义不受影响）。
        // [TRAP] 判定用临时 read guard（语句末 drop）——同线程随后 write
        // 同一 atom，parking_lot read+write 冲突会 panic。
        if key.code == KeyCode::Esc
            && FOCUSED_ENTRY.state().read().is_some()
            && matches!(
                focus_router::active_layer(),
                focus_router::FocusLayer::Input
            )
        {
            // [S2 单一事实源] 清除焦点事实源本身（slot+key 一次清除）；
            // §7 免疫是读者派生行为——读者读 FOCUSED_ENTRY 的 key 消失
            // 即恢复自动合并，无需在此同步清除。
            *FOCUSED_ENTRY.state().write() = None;
            return EventResult::Consumed;
        }
        let focused = FOCUSED_ENTRY.state().read().is_some();
        if !focus_router::message_nav_accepts(&key, focused) {
            return EventResult::Ignored;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Up | KeyCode::Down if alt => {
                // [TRAP] 先 copy 出值 drop guard 再 write——parking_lot
                // 同线程 read+write 冲突会 panic。
                let current = FOCUSED_ENTRY.state().read().as_ref().map(|f| f.slot);
                let items_len = VIEW_MODELS.state().read().items.len();
                let next = move_entry_focus(
                    items_len,
                    current,
                    if key.code == KeyCode::Up { -1 } else { 1 },
                );
                // 持 VIEW_MODELS 写锁内派生 key（桥线程可并发写 VIEW_MODELS
                // ——锁外派生会读到漂移索引；key 与索引一致性由同一快照
                // 保证）。[§7 免疫] 焦点落在无折叠能力 entry（user/
                // assistant/group）时 key 为 None（分组只涉及工具）。
                let vm_state_ref = VIEW_MODELS.state();
                let snapshot = vm_state_ref.write();
                match next {
                    Some(next_slot) => set_entry_focus(&snapshot, next_slot),
                    None => *FOCUSED_ENTRY.state().write() = None,
                }
                // 焦点移动到其他 entry——重置 interaction option 到首项
                *option_state.write() = 0;
                EventResult::Consumed
            }
            // [Slice 4 §6.8] Tab/←/→：焦点在 pending interaction block 时
            // 切换 option（局部状态，不新增 FocusLayer）；非 interaction 时
            // Ignored 放行（Tab 继续传给输入区——消息区不独占）。
            KeyCode::Tab | KeyCode::Left | KeyCode::Right
                if key.modifiers == KeyModifiers::NONE =>
            {
                // 读当前快照判断焦点 entry 类型（只读；无写锁）
                // [TRAP] 先 copy 出值 drop guard 再读 VIEW_MODELS（独立锁，
                // 顺序无冲突；保持先读后用的 guard 最小化）。
                let idx = FOCUSED_ENTRY.state().read().as_ref().map(|f| f.slot);
                let vm_guard = VIEW_MODELS.state();
                let items = &vm_guard.read().items;
                let block = idx
                    .and_then(|i| items.get(i))
                    .and_then(pending_interaction_of);
                let Some(block) = block else {
                    return EventResult::Ignored;
                };
                let opt_count = block.options.len().max(1);
                let opt = *option_state.read();
                // Tab/← 后退、→ 前进；首末回绕（循环语义，浏览器 Tab 直觉）
                let next_opt = cycle_interaction_option(
                    opt,
                    opt_count,
                    matches!(key.code, KeyCode::Left | KeyCode::Tab),
                );
                *option_state.write() = next_opt;
                EventResult::Consumed
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let next_is_preview = key.code == KeyCode::Char(' ');
                // 读当前快照：对焦点 entry 应用切换。持 VIEW_MODELS 写锁期间
                // 不再读其他可能被同一帧写入的 atom（FOLD_OVERRIDES 是独立锁）。
                // [TRAP] state() 必须绑定变量——临时值在语句末释放会导致
                // ReactiveMutRef::Drop 在借用期间运行（E0716）。
                tracing::trace!(target: "frozen_diag", "enter: acquiring VIEW_MODELS write lock");
                let vm_state_ref = VIEW_MODELS.state();
                let mut snapshot = vm_state_ref.write();
                tracing::trace!(target: "frozen_diag", "enter: got VIEW_MODELS write lock");
                // [TRAP] 先 copy 出值 drop guard 再 write——同线程随后写
                // FOCUSED_ENTRY，parking_lot read+write 冲突会 panic。
                let cur_focus = FOCUSED_ENTRY.state().read().as_ref().map(|f| f.slot);
                let Some(idx) = cur_focus else {
                    return EventResult::Consumed;
                };
                if idx >= snapshot.items.len() {
                    // 快照缩短（reset/rewind）——焦点失效，退出导航
                    *FOCUSED_ENTRY.state().write() = None;
                    return EventResult::Consumed;
                }
                // [Slice 4 §6.8] 焦点在 pending interaction block 上：
                // Enter 提交当前 option（双轨：响应 channel + 关闭模态层；
                // owner-aware InteractionTerminal 由 client 发出）；Space 消费但不动作
                // （防止泄漏给输入区插入空格）。提交后退出 entry 焦点。
                if let Some(block) = pending_interaction_of(&snapshot.items[idx]) {
                    if !next_is_preview {
                        let opt = *option_state.read();
                        submit_interaction_option(block, opt);
                    }
                    *FOCUSED_ENTRY.state().write() = None;
                    tracing::trace!(target: "frozen_diag", "enter: interaction submit exit");
                    return EventResult::Consumed;
                }
                let result = apply_fold_toggle(&mut snapshot, idx, next_is_preview);
                tracing::trace!(target: "frozen_diag", "enter: handler exit");
                result
            }
            _ => EventResult::Ignored,
        }
    });
}
