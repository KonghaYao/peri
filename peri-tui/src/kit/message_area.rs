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

use crate::kit::atoms::{COPY_CHAR_COUNT, COPY_MESSAGE_UNTIL, RENDER_CACHE, VIEW_MODELS};
use crate::kit::focus_router;
use crate::kit::panel_registry::clean_scrollbars;
use crate::kit::render_bridge::WrappedLineInfo;
use crate::kit::text_selection::{self, TextSelection};
use crate::kit::theme;
use crate::kit::welcome::Welcome;
use peri_acp_types::view_model::ViewModel;
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

fn render_todo_lines(items: &[TodoItem]) -> Vec<Line<'static>> {
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
    let semantic = theme::semantic();
    let component = theme::component();

    let render_cache = hooks.use_atom(&RENDER_CACHE);
    let view_models = hooks.use_atom(&VIEW_MODELS);
    let cache_snapshot = render_cache.read();
    // is_loading 从 RENDER_CACHE 推断——存在 CurrentTurn 说明 agent 在运行。
    let is_loading = cache_snapshot.entries.last().map_or(false, |(k, _)| {
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
    let spinner_state = hooks.use_state(|| SpinnerState::new(SpinnerMode::Thinking));
    // 每帧推进 tick——同一帧可能多次调 render，用 tick_done 标记避免重复推进
    let tick_done = hooks.use_state(|| false);
    if !*tick_done.read() {
        spinner_state.write().advance_tick();
        *tick_done.write() = true;
    }
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
    }

    let lc_data = line_cache.read();
    let current_has_ct = lc_data.current_has_ct;
    drop(lc_data);

    // 构建全量行（基于 cache_snapshot entries，不含空行分隔符，含 spinner）
    let mut all_lines: Vec<Line<'static>> = cache_snapshot
        .entries
        .iter()
        .flat_map(|(_, entry)| entry.lines.iter().cloned())
        .collect();
    if is_loading {
        let spinner = spinner_state.read();
        let spinner_lines =
            spinner.render_to_lines(semantic.status.running, semantic.text.muted, true, true);
        for line in spinner_lines {
            all_lines.push(line);
        }
    }

    // ── Todo 列表（从 ACP SessionUpdate::Plan 消费） ──
    let todo_atom = hooks.use_atom(&crate::kit::atoms::TODO_ITEMS);
    let todo_items = todo_atom.read();
    if !todo_items.is_empty() {
        for line in render_todo_lines(&todo_items) {
            all_lines.push(line);
        }
    }
    let empty = cache_snapshot.entries.is_empty() && !is_loading;
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
            let vis_width = area_rect
                .map(|r| r.width)
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

                // 委托滚动事件给 ScrollViewState
                scroll_state.write().handle_event(&event);
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
    let (first, last, local_offset) = viewport_clip(&wrap_map, scroll_y, vis_height);

    let visible_lines: Vec<Line<'static>> =
        highlighted_lines.get(first..last).unwrap_or(&[]).to_vec();

    let total_visual_rows: u16 = if wrap_map.is_empty() {
        if is_loading { 1 } else { 0 }
    } else {
        wrap_map
            .last()
            .map_or(0, |w| w.visual_row + w.visual_height)
    };

    let max_scroll = total_visual_rows.saturating_sub(vis_height);

    // ── Sticky Header：滚动时显示最后一条用户消息摘要 ──
    let sticky_header: Option<Vec<Line<'static>>> = {
        let store = view_models.read();
        let last_user_text = store
            .committed
            .iter()
            .rev()
            .chain(store.current_turn.iter().rev())
            .find_map(|vm| {
                if let ViewModel::UserBubble(data) = vm {
                    Some(data.text.chars().take(80).collect::<String>())
                } else {
                    None
                }
            });
        drop(store);

        last_user_text.filter(|_| max_scroll > 0).map(|text| {
            vec![Line::styled(
                format!("❯ {}", text),
                Style::default()
                    .fg(semantic.text.primary)
                    .bg(component.message.user_bg),
            )]
        })
    };

    let show_sticky = sticky_header.is_some();

    if show_sticky {
        let hdr_lines = sticky_header.unwrap();
        element!(
            View(
                flex_direction: Direction::Vertical,
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                // Sticky header — 固定在顶部
                Text(text: Paragraph::new(RatText::from(hdr_lines)))
                // 消息区主体
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
                        Text(text: Paragraph::new(RatText::from(visible_lines))
                            .scroll((local_offset, 0)))
                    }
                }
            }
        )
        .into_any()
    } else {
        element!(
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
                    Text(text: Paragraph::new(RatText::from(visible_lines))
                        .scroll((local_offset, 0)))
                }
            }
        )
        .into_any()
    }
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
}
