//! MessageArea：仅依赖 RENDER_CACHE 渲染消息，不再订阅完整 ViewStore。
//!
//! RENDER_CACHE atom 变化时，LineCache 根据 (len, ch, loading) key 重建 lines。
//! 滚动/terminal resize 不触发重建——仅重建 Vec<Line>，不做 markdown 解析。
//!
//! - 滚动：由 ScrollViewState 处理键盘/鼠标事件
//! - 智能跟随：use_effect 检测 CurrentTurn 出现
//! - 鼠标文本选中：Down 开始拖拽 → Drag 更新选区 → Up 提取文本并复制到剪贴板
//! - 视口裁剪：基于 RENDER_CACHE.wrap_map 二分查找，每帧只传递可见行给 Paragraph

#![allow(clippy::needless_update)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::kit::atoms::{COPY_CHAR_COUNT, COPY_MESSAGE_UNTIL, RENDER_CACHE};
use crate::kit::focus_router;
use crate::kit::panel_registry::clean_scrollbars;
use crate::kit::render_bridge::WrappedLineInfo;
use crate::kit::text_selection::{self, TextSelection};
use crate::kit::theme;
use crate::kit::welcome::Welcome;
use peri_widgets::spinner::{SpinnerMode, SpinnerState};
use ratatui_kit::{
    components::ScrollViewState,
    crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Rect},
        style::{Modifier, Style},
        text::{Line, Span, Text as RatText},
        widgets::Paragraph,
    },
};

// ── 滚动速度控制 ──────────────────────────────────────────────────────────

/// 鼠标滚轮每格的滚动行数倍数。
///
/// ratatui-kit `ScrollViewState::handle_event` 每个 `ScrollDown`/`ScrollUp` 只移 1 行，
/// 对于长对话来说太慢了。这自己接管鼠标滚动，乘以本倍数调用 `scroll_up`/`scroll_down`。
/// 调大改滚轮速度，调整不需要重新编译其他模块（仅重编译本文件）。
const SCROLL_MULTIPLIER: u16 = 3;

// ── 本地行缓存（仅 RENDER_CACHE 内容变化时重建，滚动不触发）─────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoStatus {
    InProgress,
    Completed,
    Pending,
}

#[derive(Debug, Clone)]
pub struct TodoItem {
    pub status: TodoStatus,
    pub content: String,
}

pub fn render_todo_lines(items: &[TodoItem]) -> Vec<Line<'static>> {
    let sem = theme::semantic();
    let mut lines = Vec::new();
    for item in items {
        let (icon, icon_color, text_color, crossed) = match item.status {
            TodoStatus::InProgress => ("◼", sem.accent, sem.text.primary, false),
            TodoStatus::Completed => ("✔", sem.status.success, sem.text.muted, true),
            TodoStatus::Pending => ("◻", sem.text.muted, sem.text.muted, false),
        };
        let prefix_style = Style::default().fg(icon_color).add_modifier(Modifier::BOLD);
        let mut text_style = Style::default().fg(text_color);
        if crossed {
            text_style = text_style.add_modifier(Modifier::CROSSED_OUT);
        }
        let prefix = Span::styled(format!("  {}  ", icon), prefix_style);
        let mut content = item.content.clone();
        if item.status == TodoStatus::Pending {
            content.push_str(" (可开始)");
        }
        let text = Span::styled(content, text_style);
        lines.push(Line::from(vec![prefix, text]));
    }
    for _ in 0..3 {
        lines.push(Line::from(""));
    }
    lines
}

#[derive(Default)]
struct LineCache {
    key: u64,
    current_has_ct: bool,
    /// 从 highlighted_lines 重建的完整 wrap_map（含 spinner/todo 视觉行）
    /// 仅在内容变化（key 变更）时重建，滚动/选区变化复用缓存。
    cached_wrap_map: Vec<WrappedLineInfo>,
    /// 上次计算 wrap_map 时对应的 highlighted_lines 长度，
    /// 用于在内容无变化时复用 cached_wrap_map。
    cached_line_count: usize,
}

// ── 视口裁剪 ────────────────────────────────────────────────────────────────

fn mouse_in_area(mouse_row: u16, mouse_col: u16, area: Rect) -> bool {
    let area_bottom = area.y.saturating_add(area.height);
    let area_right = area.x.saturating_add(area.width);
    mouse_row >= area.y && mouse_row < area_bottom && mouse_col >= area.x && mouse_col < area_right
}

fn mouse_visual_position(mouse_row: u16, mouse_col: u16, area: Rect, scroll_y: u16) -> (u16, u16) {
    (
        mouse_row.saturating_sub(area.y).saturating_add(scroll_y),
        mouse_col.saturating_sub(area.x),
    )
}

fn copy_selected_text_to_clipboard(text: String) {
    std::thread::spawn(move || {
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(text);
        }
    });
}

fn mark_copy_message(char_count: usize) {
    *COPY_CHAR_COUNT.state().write() = char_count;
    *COPY_MESSAGE_UNTIL.state().write() = Some(Instant::now() + Duration::from_millis(2000));
}

/// 视口裁剪：基于 wrap_map 二分查找，返回当前视口内可见的逻辑行范围 [first, last)。
///
/// - `wrap_map`: 每逻辑行的视觉行映射（`visual_row` + `visual_height`）
/// - `scroll_y`: ScrollViewState 的当前滚动偏移（视觉行号）
/// - `vis_height`: 消息区当前可见行数
fn viewport_clip(
    wrap_map: &[WrappedLineInfo],
    scroll_y: u16,
    vis_height: u16,
) -> (usize, usize, u16) {
    let total = wrap_map
        .last()
        .map_or(0, |w| w.visual_row + w.visual_height);
    let vis_start = scroll_y.min(total.saturating_sub(1));
    let vis_end = scroll_y.saturating_add(vis_height).min(total);

    let first = wrap_map.partition_point(|w| w.visual_row + w.visual_height <= vis_start);
    let last = wrap_map.partition_point(|w| w.visual_row < vis_end);

    let local_offset = wrap_map
        .get(first)
        .map_or(0, |w| vis_start.saturating_sub(w.visual_row));

    (first, last, local_offset)
}

// ── 消息区位置追踪 Hook ─────────────────────────────────────────────────

/// 在 pre_component_draw 时记录消息区外边界，供鼠标坐标转换使用。
struct MsgAreaTracker {
    /// 上一帧的消息区 Rect（绝对终端坐标）。首次渲染前为 None。
    rect: Option<Rect>,
}

impl MsgAreaTracker {
    fn new() -> Self {
        Self { rect: None }
    }
}

impl Hook for MsgAreaTracker {
    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.rect = Some(drawer.area);
    }
}

// ── Props ──────────────────────────────────────────────────────────────────

#[derive(Default, Props)]
pub struct MessageAreaProps {
    pub width: usize,
}

#[component]
pub fn MessageArea(props: &MessageAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let render_cache = hooks.use_atom(&RENDER_CACHE);
    let cache_snapshot = render_cache.read();
    // is_loading 双重检测：ACP_STATE.is_loading（acp_bridge 在首条流式事件时置 true）+ RENDER_CACHE CurrentTurn。
    let acp_is_loading = crate::kit::atoms::ACP_STATE.state().read().is_loading;
    let is_loading = acp_is_loading
        || cache_snapshot.entries.last().map_or(false, |(k, _)| {
            matches!(k, crate::kit::render_bridge::VmKey::CurrentTurn(_))
        });

    let entries_len = cache_snapshot.entries.len();
    let raw_ch = cache_snapshot
        .cumulative_heights
        .last()
        .copied()
        .unwrap_or(0);

    // ── 缓存 key：仅 entries 数量/高度/loading 变化时重建 ──
    let line_cache = hooks.use_state(|| LineCache::default());
    let scroll_state = hooks.use_state(ScrollViewState::default);
    let mut auto_scroll = hooks.use_state(|| true);
    let had_ct = hooks.use_state(|| false);
    let new_key = {
        let h = raw_ch as u64;
        let l = entries_len as u64;
        let d = is_loading as u64;
        h.wrapping_mul(0x9e3779b9)
            .wrapping_add(l.wrapping_mul(0x7f4a7c15))
            .wrapping_add(d)
    };

    if line_cache.read().key != new_key {
        let mut ct = false;
        for (key, _entry) in cache_snapshot.entries.iter() {
            if matches!(key, crate::kit::render_bridge::VmKey::CurrentTurn(_)) {
                ct = true;
            }
        }
        let mut lc = line_cache.write();
        lc.key = new_key;
        lc.current_has_ct = ct;
        // key 变更 → 内容已变化，cached_wrap_map 需重建，
        // 通过清空 cached_line_count 触发后续 rebuild。
        lc.cached_wrap_map.clear();
        lc.cached_line_count = 0;
        // 内容变化时触发自动滚动到底部（覆盖 session/load 等无 CT 的批量加载场景）
        auto_scroll.set(true);
    }

    let lc_data = line_cache.read();
    let current_has_ct = lc_data.current_has_ct;
    drop(lc_data);

    // 构建全量行（基于 cache_snapshot entries，仅消息内容，不含 spinner/todo）
    let all_lines: Vec<Line<'static>> = cache_snapshot
        .entries
        .iter()
        .flat_map(|(_, entry)| entry.lines.iter().cloned())
        .collect();
    let empty = cache_snapshot.entries.is_empty() && !is_loading;
    let all_line_count = all_lines.len();
    let content_lines = Arc::new(all_lines.clone());
    drop(cache_snapshot);

    // ── 消息区位置追踪 ──
    let area_hook = hooks.use_hook(MsgAreaTracker::new);
    let area_rect = area_hook.rect;

    // ── 文本选区状态 ──
    let text_sel = hooks.use_state(TextSelection::new);

    // 选区高亮依赖 all_lines，提前拿到
    let sel = text_sel.read();
    let highlighted_lines: Vec<Line<'static>> =
        if let Some(((sr, sc), (er, ec))) = sel.normalized_bounds() {
            if sel.is_active() {
                text_selection::highlight_selected_lines(&all_lines, sr, sc, er, ec)
            } else {
                all_lines
            }
        } else {
            all_lines
        };
    drop(sel);

    // ── wrap_map：仅在内容行数变化时重建，滚动/选区变化复用缓存 ──
    let wrap_map = {
        let lc = line_cache.read();
        let stale = lc.cached_line_count != highlighted_lines.len();
        drop(lc);
        if stale && !highlighted_lines.is_empty() {
            // 宽度需匹配 ScrollView 内部实际渲染宽度。
            //
            // ratatui-kit ScrollView 在 content_height > visible_height 时，
            // ScrollbarVisibility::Automatic 会同时显示垂直+水平滚动条（horizontal_space==0
            // 时 visible_scrollbars 回退到 else → (true, true)）。垂直滚动条占 1 列，
            // 实际渲染宽度 = area.width - 1。若 build_wrap_map 用 area.width，Paragraph
            // 换行点与预测不一致，累积的视觉高度差异导致内容溢出/底部空白。
            let vis_width = area_rect
                .map(|r| r.width.saturating_sub(1))
                .unwrap_or(props.width as u16)
                .max(1);
            let map = crate::kit::render_bridge::build_wrap_map(&highlighted_lines, vis_width);
            let mut lc = line_cache.write();
            lc.cached_wrap_map = map.clone();
            lc.cached_line_count = highlighted_lines.len();
            map
        } else if highlighted_lines.is_empty() {
            Vec::new()
        } else {
            line_cache.read().cached_wrap_map.clone()
        }
    };
    {
        let content_lines_handler = content_lines.clone();
        hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
            if let Event::Key(key) = &event {
                let _ = focus_router::message_accepts_key(key);
            }

            // ── 鼠标事件 ──
            if let Event::Mouse(mouse) = &event {
                // MouseMove 事件频率极高（数百次/秒），不做任何处理直接忽略。
                if matches!(mouse.kind, MouseEventKind::Moved) {
                    return EventResult::Ignored;
                }
                // 判断鼠标是否在消息区内
                if let Some(area) = area_rect {
                    let in_area = mouse_in_area(mouse.row, mouse.column, area);

                    if in_area {
                        let scroll_y = scroll_state.read().offset().y;
                        let (visual_row, visual_col) =
                            mouse_visual_position(mouse.row, mouse.column, area, scroll_y);

                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                // 开始文本拖拽选中
                                text_sel.write().start_drag(visual_row, visual_col);
                                auto_scroll.set(false);
                            }
                            MouseEventKind::Drag(MouseButton::Left) => {
                                let mut sel = text_sel.write();
                                if sel.dragging {
                                    sel.update_drag(visual_row, visual_col);
                                }
                                auto_scroll.set(false);
                            }
                            MouseEventKind::Up(MouseButton::Left) => {
                                let mut sel = text_sel.write();
                                let was_dragging = sel.dragging;
                                if was_dragging {
                                    sel.end_drag();
                                    // 从 content_lines 提取选区文本
                                    if let (Some(start), Some(end)) = (sel.start, sel.end) {
                                        let selected_text = text_selection::extract_selected_text(
                                            start,
                                            end,
                                            &content_lines_handler,
                                        );
                                        if let Some(ref text) = selected_text {
                                            mark_copy_message(text.chars().count());
                                            copy_selected_text_to_clipboard(text.clone());
                                        }
                                        sel.set_selected_text(selected_text);
                                    }
                                }
                                // 非拖拽点击——清除旧选区
                                if !was_dragging {
                                    sel.clear();
                                }
                                auto_scroll.set(false);
                            }
                            _ => {}
                        }
                    }
                }

                // 滚动事件：鼠标滚轮用乘数加速；其他鼠标事件委托 ScrollViewState
                match mouse.kind {
                    MouseEventKind::ScrollDown => {
                        for _ in 0..SCROLL_MULTIPLIER {
                            scroll_state.write().scroll_down();
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        for _ in 0..SCROLL_MULTIPLIER {
                            scroll_state.write().scroll_up();
                        }
                    }
                    _ => {
                        scroll_state.write().handle_event(&event);
                    }
                }
                auto_scroll.set(false);
                return EventResult::Consumed;
            }

            // 键盘事件：仅消费 message 专用键（Ctrl+↑↓HomeEnd），其余透传给 InputArea
            if let Event::Key(key) = &event {
                if key.kind == KeyEventKind::Press && focus_router::message_accepts_key(key) {
                    scroll_state.write().handle_event(&event);
                    auto_scroll.set(false);
                    return EventResult::Consumed;
                }
            }
            EventResult::Ignored
        });
    }

    hooks.use_effect(
        {
            let mut a = auto_scroll;
            let mut h = had_ct;
            let st = scroll_state;
            move || {
                if !h.get() && current_has_ct {
                    a.set(true);
                }
                if a.get() {
                    st.write().scroll_to_bottom();
                }
                h.set(current_has_ct);
            }
        },
        (current_has_ct,),
    );

    // ── Footer 行预计算：必须在 empty 分支之前调用，确保所有 hook 顺序一致 ──
    // [TRAP] build_footer_lines 内部调用 hooks.use_atom / hooks.use_state，
    // 必须每帧按相同顺序执行，否则 ratatui-kit 触发 "Hook type mismatch" panic。
    let footer_lines = build_footer_lines(&mut hooks);

    if empty {
        return element!(
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                Welcome(width: props.width)
            }
        )
        .into_any();
    }

    // ── 视口裁剪 ──
    let scroll_y = scroll_state.read().offset().y as u16;
    let vis_height = area_rect.map(|r| r.height).unwrap_or(60).max(1);
    let (first, last, _local_offset) = viewport_clip(&wrap_map, scroll_y, vis_height);

    let visible_lines: Vec<Line<'static>> =
        highlighted_lines.get(first..last).unwrap_or(&[]).to_vec();

    // 内容在 ScrollView 中的垂直起始位置（视觉行号）。
    // ScrollView 负责整体滚动；这里通过在内容 View 前面插入一个 spacer，
    // 使 visible_lines 出现在内容 View 的正确偏移处。
    let content_top = wrap_map.get(first).map_or(0u16, |w| w.visual_row);

    let total_visual_rows: u16 = if wrap_map.is_empty() {
        if is_loading { 1 } else { 0 }
    } else {
        wrap_map
            .last()
            .map_or(0, |w| w.visual_row + w.visual_height)
    };

    // 底部 spacer：补齐 content_top + visible 实际高度到 total_visual_rows。
    //
    // viewport_clip 只选取 [first..last) 子集，ScrollView 内容 View 高度却是
    // total_visual_rows（全量）。当滚到中间时 visible 子集高度 << total，
    // 差值在视口底部表现为空白区域（有换行内容时更明显，因 wrap_map 每行高度 >1，
    // 与 Paragraph 实际渲染高度的微小差异会产生更大空白）。
    let content_bottom = wrap_map
        .get(last.saturating_sub(1))
        .map_or(0u16, |w| w.visual_row + w.visual_height);
    let bottom_spacer = total_visual_rows.saturating_sub(content_bottom);

    let max_scroll = total_visual_rows.saturating_sub(vis_height);

    // 诊断：记录关键渲染参数，输出到 agent-tui.log
    tracing::info!(
        total_visual_rows,
        vis_height,
        max_scroll,
        scroll_y,
        first,
        last,
        all_count = all_line_count,
        hl_count = highlighted_lines.len(),
        vis_count = visible_lines.len(),
        content_top,
        content_bottom,
        bottom_spacer,
        empty,
        "msg-area diag"
    );

    // ── Footer：spinner + todo，固定在 ScrollView 之外 ──
    // footer_lines 已在 empty 分支前预计算；此处仅消费结果。

    // 暂时禁用 sticky header，测试是否是它导致内容消失
    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            ScrollView(
                flex_direction: Direction::Vertical,
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
                scroll_view_state: scroll_state,
                scroll_bars: clean_scrollbars(),
            ) {
                View(
                    flex_direction: Direction::Vertical,
                    width: Constraint::Fill(1),
                    height: Constraint::Length(total_visual_rows.max(1)),
                ) {
                    View(height: Constraint::Length(content_top)) {}
                    Text(text: Paragraph::new(RatText::from(visible_lines)))
                    View(height: Constraint::Length(bottom_spacer)) {}
                }
            }
            View(
                width: Constraint::Fill(1),
                height: Constraint::Length(footer_lines.len().max(1) as u16),
            ) {
                Text(text: Paragraph::new(RatText::from(footer_lines)))
            }
        }
    )
    .into_any()
}

// ── footer 行构建：在 MessageArea 作用域内计算 spinner + todo 行 ──

fn build_footer_lines(hooks: &mut Hooks) -> Vec<Line<'static>> {
    let semantic = theme::semantic();

    let acp_state = hooks.use_atom(&crate::kit::atoms::ACP_STATE);
    let is_loading = acp_state.read().is_loading;

    let todo_atom = hooks.use_atom(&crate::kit::atoms::TODO_ITEMS);
    let todo_items = todo_atom.read();

    // [TRAP] 所有 hook 调用必须在任何 early return 之前，确保每帧 hook 调用顺序一致。
    // ratatui-kit 按调用顺序索引 hook，顺序变化会触发 "Hook type mismatch" panic。
    let spinner_state = hooks.use_state(|| SpinnerState::new(SpinnerMode::Thinking));
    let load_start = hooks.use_state(|| Option::<Instant>::None);
    let once = hooks.use_state(|| false);

    // 快速路径：无需渲染 footer 行时直接返回空。
    if !is_loading && todo_items.is_empty() {
        return Vec::new();
    }
    if !*once.read() {
        let mut ls = load_start.write();
        if is_loading && ls.is_none() {
            *ls = Some(Instant::now());
            *spinner_state.write() = SpinnerState::new(SpinnerMode::Thinking);
        } else if !is_loading {
            *ls = None;
        }

        // 壁钟时间补偿步进
        if let Some(start) = *ls {
            let elapsed = start.elapsed().as_millis() as u64;
            let target = elapsed / 50;
            let delta = {
                let current = spinner_state.read().raw_tick();
                target.saturating_sub(current).min(20)
            };
            for _ in 0..delta {
                spinner_state.write().advance_tick();
            }
        }
        *once.write() = true;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    if is_loading {
        lines.extend(spinner_state.read().render_to_lines(
            semantic.accent,
            semantic.text.muted,
            true,
            true,
        ));
    }
    if !todo_items.is_empty() {
        lines.extend(render_todo_lines(&todo_items));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrapped(line_idx: usize, visual_row: u16, visual_height: u16) -> WrappedLineInfo {
        WrappedLineInfo {
            line_idx,
            visual_row,
            visual_height,
        }
    }

    #[test]
    fn test_viewport_clip_saturates_visible_end_on_u16_overflow() {
        let wrap_map = vec![wrapped(0, 0, u16::MAX)];
        assert_eq!(
            viewport_clip(&wrap_map, u16::MAX - 1, 10),
            (0, 1, u16::MAX - 1)
        );
    }

    #[test]
    fn test_mouse_area_and_visual_position_saturate_u16_bounds() {
        let area = Rect {
            x: u16::MAX - 1,
            y: u16::MAX - 1,
            width: 10,
            height: 10,
        };
        assert!(mouse_in_area(u16::MAX - 1, u16::MAX - 1, area));
        assert_eq!(mouse_visual_position(u16::MAX, u16::MAX, area, 10), (11, 1));
    }

    /// 回归：visual_row 越界时 clamp 后 extract_selected_text 正常返回。
    ///
    /// 场景：内容 3 行，用户点击在 row=50（空白区域），visual_row=50。
    /// 不 clamp → sr=50 >= lines.len()=3 → extract_selected_text 返回 None。
    /// clamp 到 max_row=2 → sr=2 < 3 → 正常提取。
    #[test]
    fn test_extract_selected_text_clamped_to_content_bounds() {
        let lines: Vec<Line<'static>> = vec![
            Line::from("line 0"),
            Line::from("line 1"),
            Line::from("line 2"),
        ];

        let max_row = (lines.len().saturating_sub(1)) as u16; // 2

        // 不 clamp：sr=50 越界，extract 返回 None
        assert!(
            text_selection::extract_selected_text((50, 0), (50, 5), &lines).is_none(),
            "sr=50 >= len=3 应返回 None"
        );

        // clamp 后 sr=2：正常提取
        let clamped = 50u16.min(max_row);
        let r = text_selection::extract_selected_text((clamped, 0), (clamped, 6), &lines);
        assert!(r.is_some(), "clamped sr=2 应正常提取文本");
        assert_eq!(r.unwrap(), "line 2");
    }

    /// 回归：滚动到中间时底部 spacer 补齐 content_bottom 到 total_visual_rows。
    ///
    /// 场景：wrap_map 有换行内容（多行 visual_height > 1），滚动到中间时
    /// viewport_clip 只返回 [first..last) 子集。如果只渲染 spacer(top) +
    /// visible_lines 而不加底部 spacer，content_top + visible 实际高度远小于
    /// total_visual_rows，ScrollView 底部出现大段空白。
    ///
    /// 修复：在 visible_lines 之后补充 bottom_spacer =
    /// total_visual_rows - content_bottom。
    #[test]
    fn test_bottom_spacer_fills_gap_when_scrolling_mid() {
        // 构建 wrap_map：模拟含换行内容（代码块/长行），每行 visual_height 可能 > 1
        let wrap_map = vec![
            wrapped(0, 0, 5),   // 0-4
            wrapped(1, 5, 10),  // 5-14
            wrapped(2, 15, 5),  // 15-19
            wrapped(3, 20, 10), // 20-29
        ];
        let total_visual_rows =
            wrap_map.last().unwrap().visual_row + wrap_map.last().unwrap().visual_height; // 30

        // 滚动到中间：scroll_y=8, vis_height=12
        let scroll_y = 8u16;
        let vis_height = 12u16;
        let (first, last, _) = viewport_clip(&wrap_map, scroll_y, vis_height);

        assert_eq!(first, 1, "first 应跳过第 0 行（视觉范围 0-4）");
        assert_eq!(last, 3, "last 应包含第 1、2 行（视觉 5-19 覆盖视口 8-20）");

        let content_top = wrap_map[first].visual_row; // 5
        let content_bottom = wrap_map[last.saturating_sub(1)].visual_row
            + wrap_map[last.saturating_sub(1)].visual_height; // 15 + 5 = 20
        let bottom_spacer = total_visual_rows.saturating_sub(content_bottom); // 30 - 20 = 10

        assert_eq!(content_bottom, 20);
        assert_eq!(
            bottom_spacer, 10,
            "中间滚动时底部空白 = total - content_bottom"
        );

        // 验证：spacer(top) + visible_height(wrap_map 预测) + spacer(bottom) = total
        let visible_height = content_bottom - content_top; // 15
        assert_eq!(
            content_top + visible_height + bottom_spacer,
            total_visual_rows
        );
    }

    /// 底部 spacer 在最底部时应为 0（no unnecessary spacer）。
    #[test]
    fn test_bottom_spacer_is_zero_at_bottom() {
        let wrap_map = vec![
            wrapped(0, 0, 3),
            wrapped(1, 3, 5),
            wrapped(2, 8, 2),
            wrapped(3, 10, 4),
        ];
        let total_visual_rows =
            wrap_map.last().unwrap().visual_row + wrap_map.last().unwrap().visual_height; // 14

        // 滚动到最底部
        let scroll_y = total_visual_rows - 5; // 9
        let (_first, last, _) = viewport_clip(&wrap_map, scroll_y, 5);
        let content_bottom = wrap_map[last.saturating_sub(1)].visual_row
            + wrap_map[last.saturating_sub(1)].visual_height;
        let bottom_spacer = total_visual_rows.saturating_sub(content_bottom);

        assert_eq!(
            bottom_spacer, 0,
            "在底部时 bottom_spacer 应为 0，不应有多余空白"
        );
    }
}
