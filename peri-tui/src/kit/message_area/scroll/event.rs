use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::kit::focus_router;
use crate::kit::message_area::props::{ScrollbarFields, mouse_in_area};
use crate::kit::message_area::selection::{
    SlotIndex, copy_to_clipboard, extract_visual_range_index, mark_copy_message,
};
use crate::kit::mouse_router;
use crate::kit::text_selection::TextSelection;
use ratatui_kit::crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Rect;

use super::{
    DragAction, DragThrottle, GesturePending, SCROLL_LINES, ScrollPos, ScrollThrottle,
    ScrollbarDragState, apply_scroll, compute_thumb_geometry, drag_step, freeze_down_index,
    is_scrollbar_column, position_to_scroll_y, scroll_frame_ms, settle_up, thumb_start_to_position,
    update_follow_on_scroll,
};

// ── 鼠标事件处理 ─────────────────────────────────────────────────────────

/// 从 `use_event_handler` 闭包提取的鼠标/键盘处理逻辑。
/// 包含：鼠标滚轮节流、文本拖拽选中（Down/Drag/Up）、键盘滚动、
/// PERI_DISABLE_DRAG_SELECT 分支、parking_lot 死锁规避。
#[allow(clippy::too_many_arguments)]
pub(in crate::kit::message_area) fn handle_event(
    event: &Event,
    area_rect: Option<Rect>,
    vis_width: u16,
    scroll_state: &State<ScrollPos>,
    scroll_throttle: &State<ScrollThrottle>,
    text_sel: &State<TextSelection>,
    gesture: &State<Option<GesturePending>>,
    drag_throttle: &State<DragThrottle>,
    slot_index: &Arc<SlotIndex>,
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
                        let pending = freeze_down_index(
                            (mouse.column, mouse.row),
                            (visual_row, visual_col),
                            slot_index,
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
                            extract_visual_range_index(
                                slot_index,
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
