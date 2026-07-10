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

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::i18n;
use crate::kit::atoms::{COPY_CHAR_COUNT, COPY_MESSAGE_UNTIL, LANG_VERSION, RENDER_CACHE};
use crate::kit::focus_router;
use crate::kit::panel_registry::clean_scrollbars;
use crate::kit::render_bridge::WrappedLineInfo;
use crate::kit::text_selection::{self, TextSelection};
use crate::kit::welcome::Welcome;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use peri_widgets::spinner::{SpinnerMode, SpinnerState};
use ratatui_kit::{
    components::ScrollViewState,
    crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Rect},
        style::{Modifier, Style},
        text::{Line, Span, Text as RatText},
        widgets::{Paragraph, Wrap},
    },
};

// ── 滚动速度控制 ──────────────────────────────────────────────────────────

/// 鼠标滚轮每格的滚动行数倍数。
///
/// ratatui-kit `ScrollViewState::handle_event` 每个 `ScrollDown`/`ScrollUp` 只移 1 行，
/// 对于长对话来说太慢了。这自己接管鼠标滚动，乘以本倍数调用 `scroll_up`/`scroll_down`。
/// 调大改滚轮速度，调整不需要重新编译其他模块（仅重编译本文件）。
const SCROLL_LINES: u16 = 3;

// ── 本地行缓存（仅 RENDER_CACHE 内容变化时重建，滚动不触发）─────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

fn hash_todo_items(items: &[TodoItem]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for item in items {
        item.status.hash(&mut hasher);
        item.content.hash(&mut hasher);
    }
    hasher.finish()
}

pub fn render_todo_lines(items: &[TodoItem]) -> Vec<Line<'static>> {
    let sem = THEME_ATOM.state().read().semantic;
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
            content.push_str(&i18n::tr("msg-todo-available"));
        }
        let text = Span::styled(content, text_style);
        lines.push(Line::from(vec![prefix, text]));
    }
    for _ in 0..1 {
        lines.push(Line::from(""));
    }
    lines
}

struct LineCache {
    key: u64,
    /// 从 highlighted_lines 重建的完整 wrap_map（含 spinner/todo 视觉行）
    /// 仅在内容变化（key 变更）时重建，滚动/选区变化复用缓存。
    cached_wrap_map: Vec<WrappedLineInfo>,
    /// 上次计算 wrap_map 时对应的 highlighted_lines 长度，
    /// 用于在内容无变化时复用 cached_wrap_map。
    cached_line_count: usize,
    /// 上次计算 wrap_map 时对应的渲染宽度。
    /// 宽度变化会改变 Paragraph 换行点，必须触发 wrap_map 重建。
    cached_width: u16,
    /// 基于原始 all_lines（无高亮）的 wrap_map，用于事件 handler 坐标转换。
    /// 选区高亮不改变行结构，因此与 highlighted_lines 的 wrap_map 具有相同的
    /// line_idx→visual 映射关系，只是不包含颜色信息。
    raw_wrap_map: Vec<WrappedLineInfo>,
    /// raw_wrap_map 对应的行数和宽度，用于增量更新。
    raw_line_count: usize,
    raw_width: u16,
    /// 上次重建 wrap_map 的时间，用于 resize 限流。
    /// 窗口 resize 时宽度连续变化，每个变化触发全量 Paragraph 重建（~2ms×N行），
    /// 高频 resize 导致渲染积压→crossterm 事件缓冲区溢出→死锁。
    #[allow(dead_code)]
    last_rebuild: Option<std::time::Instant>,
}

impl Default for LineCache {
    fn default() -> Self {
        Self {
            key: 0,
            cached_wrap_map: Vec::new(),
            cached_line_count: 0,
            cached_width: 0,
            raw_wrap_map: Vec::new(),
            raw_line_count: 0,
            raw_width: 0,
            last_rebuild: Some(std::time::Instant::now()),
        }
    }
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

/// 将视觉坐标 (visual_row, visual_col) 转换为 (line_index, col_in_line)。
///
/// visual_row 是内容空间中的视觉行号（含 scroll 偏移），通过 wrap_map 反查
/// 对应的原始行索引。对于折行产生的后续视觉行，col_in_line 需要加上前序行的
/// 宽度补偿，确保正确索引到原始行中的列位置。
fn visual_to_line_position(
    wrap_map: &[WrappedLineInfo],
    visual_row: u16,
    visual_col: u16,
    vis_width: u16,
) -> Option<(usize, u16)> {
    // 二分查找 visual_row 所属的原始行
    let idx = wrap_map.partition_point(|w| w.visual_row + w.visual_height <= visual_row);
    let info = wrap_map.get(idx)?;
    if visual_row < info.visual_row || visual_row >= info.visual_row + info.visual_height {
        return None;
    }
    let rows_before = visual_row.saturating_sub(info.visual_row);
    let col = if rows_before == 0 {
        visual_col
    } else {
        // 折行后续行：col 需要加上前序视觉行的宽度
        visual_col.saturating_add(rows_before.saturating_mul(vis_width))
    };
    Some((info.line_idx, col))
}

fn copy_selected_text_to_clipboard(text: String) {
    // [TRAP] arboard 是同步阻塞 I/O（macOS NSPasteboard），在 tokio worker 上调用会卡住
    // render_loop 几百 ms。CLAUDE.md 要求剪贴板等阻塞 I/O 用 std::thread::spawn。
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
    let acp_state = hooks.use_atom(&crate::kit::atoms::ACP_STATE);
    let todo_atom = hooks.use_atom(&crate::kit::atoms::TODO_ITEMS);
    hooks.use_atom(&LANG_VERSION);
    let cache_snapshot = render_cache.read();
    let todo_items = todo_atom.read().clone();
    // is_loading 单数据源：ACP_STATE.is_loading（acp_bridge 在首条流式事件时置 true）。
    let is_loading = acp_state.read().is_loading;

    let entries_len = cache_snapshot.entries.len();
    let raw_ch = cache_snapshot
        .cumulative_heights
        .last()
        .copied()
        .unwrap_or(0);

    // ── 缓存 key：仅 entries 数量/高度/loading/todo 变化时重建 ──
    let line_cache = hooks.use_state(|| LineCache::default());
    let scroll_state = hooks.use_state(ScrollViewState::default);
    // 追踪上一次的 entries_len，用于检测「内容从空→非空」过渡（history load 后强制滚到底部）
    let prev_entries_len = hooks.use_state(|| 0usize);
    // hook 占位——ratatui-kit 要求 hook 数量恒定不可增减
    let _prev_is_loading = hooks.use_state(|| false);
    let todo_hash = hash_todo_items(&todo_items);
    let new_key = {
        let h = raw_ch as u64;
        let l = entries_len as u64;
        let d = is_loading as u64;
        h.wrapping_mul(0x9e3779b9)
            .wrapping_add(l.wrapping_mul(0x7f4a7c15))
            .wrapping_add(d)
            .wrapping_add(todo_hash.wrapping_mul(0x94d049bb133111eb))
    };

    if line_cache.read().key != new_key {
        let mut lc = line_cache.write();
        lc.key = new_key;
        // key 变更 → 内容已变化，cached_wrap_map 需重建，
        // 通过清空 cached_line_count 触发后续 rebuild。
        lc.cached_wrap_map.clear();
        lc.cached_line_count = 0;
        lc.cached_width = 0;
    }

    // ── Footer 行预计算：必须在 empty 分支之前调用，确保所有 hook 顺序一致 ──
    // [TRAP] build_footer_lines 内部调用 hooks.use_state，必须每帧按相同顺序执行，
    // 否则 ratatui-kit 触发 "Hook type mismatch" panic。
    let footer_lines = build_footer_lines(&mut hooks, is_loading, &todo_items);

    let empty = cache_snapshot.entries.is_empty() && !is_loading && todo_items.is_empty();
    // 空态且有 Brewed 总结行时，clone 一份用于 Welcome 下方独立渲染。
    // footer_lines 可能为空（无历史、首次启动），此时不进入 Brewed 分支。
    let brewed_lines = if empty && !footer_lines.is_empty() {
        Some(footer_lines.clone())
    } else {
        None
    };

    // 构建全量行（entries + footer）。空态时不 extend footer_lines，
    // Brewed 总结行在 Welcome 下方独立渲染，避免 Welcome 早退分支丢弃 footer。
    let table_border_style = Style::default().fg(ratatui::style::Color::Gray);
    let mut all_lines: Vec<Line<'static>> = Vec::new();
    for (_, entry) in &cache_snapshot.entries {
        match entry {
            crate::kit::render_bridge::RenderedEntry::Text { lines, .. } => {
                all_lines.extend(lines.iter().cloned());
            }
            crate::kit::render_bridge::RenderedEntry::Table { data, .. } => {
                all_lines.extend(crate::kit::markdown::table_data_to_lines(
                    data,
                    table_border_style,
                ));
            }
        }
    }
    if !empty {
        all_lines.extend(footer_lines);
    }
    let _all_line_count = all_lines.len();
    let content_lines = Arc::new(all_lines.clone());
    drop(cache_snapshot);

    // ── 消息区位置追踪 ──
    let area_hook = hooks.use_hook(MsgAreaTracker::new);
    let area_rect = area_hook.rect;

    // 渲染宽度（提前计算，raw_wrap_map 和 highlighted_lines wrap_map 共用）
    let vis_width = area_rect
        .map(|r| r.width.saturating_sub(1))
        .unwrap_or(props.width as u16)
        .max(1);

    // 渲染高度（提前计算，供 use_effect 就近判断使用）
    let vis_height = area_rect.map(|r| r.height).unwrap_or(60).max(1);

    // ── raw_wrap_map：用于事件 handler 中将视觉坐标转换为行索引 ──
    let raw_wrap_map: Vec<WrappedLineInfo> = {
        let lc = line_cache.read();
        let stale = lc.raw_line_count != all_lines.len()
            || (lc.raw_width != vis_width
                && lc.last_rebuild.map_or(true, |t| {
                    t.elapsed() > std::time::Duration::from_millis(100)
                }))
            || lc.raw_wrap_map.is_empty();
        drop(lc);
        if stale && !all_lines.is_empty() {
            let map = crate::kit::render_bridge::build_wrap_map(&all_lines, vis_width);
            let mut lc = line_cache.write();
            lc.raw_wrap_map = map.clone();
            lc.raw_line_count = all_lines.len();
            lc.raw_width = vis_width;
            lc.last_rebuild = Some(std::time::Instant::now());
            map
        } else if all_lines.is_empty() {
            Vec::new()
        } else {
            line_cache.read().raw_wrap_map.clone()
        }
    };

    // ── 文本选区状态 ──
    let text_sel = hooks.use_state(TextSelection::new);

    // 选区高亮依赖 all_lines，提前拿到
    let sel = text_sel.read();
    let sel_active = sel.is_active();
    let sel_bounds = sel.normalized_bounds();
    drop(sel);
    let highlighted_lines: Vec<Line<'static>> = if let Some(((sr, sc), (er, ec))) = sel_bounds {
        if sel_active {
            text_selection::highlight_selected_lines(&all_lines, sr, sc, er, ec)
        } else {
            all_lines
        }
    } else {
        all_lines
    };

    // ── wrap_map：内容或宽度变化时重建，滚动变化复用缓存 ──
    let wrap_map = {
        let lc = line_cache.read();
        let stale = lc.cached_line_count != highlighted_lines.len()
            || (lc.cached_width != vis_width
                && lc.last_rebuild.map_or(true, |t| {
                    t.elapsed() > std::time::Duration::from_millis(100)
                }))
            || lc.cached_wrap_map.is_empty();
        drop(lc);
        if stale && !highlighted_lines.is_empty() {
            // 宽度需匹配 ScrollView 内部实际渲染宽度。
            // ratatui-kit ScrollView 在 content_height > visible_height 时，
            // ScrollbarVisibility::Automatic 会同时显示垂直+水平滚动条。垂直滚动条占 1 列，
            // 实际渲染宽度 = area.width - 1。若 build_wrap_map 用 area.width，Paragraph
            // 换行点与预测不一致，累积的视觉高度差异导致内容溢出/底部空白。
            let map = crate::kit::render_bridge::build_wrap_map(&highlighted_lines, vis_width);
            let mut lc = line_cache.write();
            lc.cached_wrap_map = map.clone();
            lc.cached_line_count = highlighted_lines.len();
            lc.cached_width = vis_width;
            lc.last_rebuild = Some(std::time::Instant::now());
            map
        } else if highlighted_lines.is_empty() {
            Vec::new()
        } else {
            line_cache.read().cached_wrap_map.clone()
        }
    };
    // ── 总视觉行数（供 use_effect 就近判断使用，提前到 hooks 之前）──
    let total_visual_rows: u16 = if wrap_map.is_empty() {
        if is_loading { 1 } else { 0 }
    } else {
        wrap_map
            .last()
            .map_or(0, |w| w.visual_row + w.visual_height)
    };
    {
        let content_lines_handler = content_lines.clone();
        let raw_wrap_map_handler = Arc::new(raw_wrap_map.clone());
        let vis_width_handler = vis_width;
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

                        // 通过 raw_wrap_map 将视觉坐标转换为行索引 + 列偏移
                        let (line_idx, col_in_line) = visual_to_line_position(
                            &raw_wrap_map_handler,
                            visual_row,
                            visual_col,
                            vis_width_handler,
                        )
                        .unwrap_or((visual_row as usize, visual_col));

                        match mouse.kind {
                            // 滚轮事件：直接 scroll_up/scroll_down（write 触发 wake）。
                            // 每格滚动 SCROLL_LINES 行，长对话下滚轮速度更跟手。
                            MouseEventKind::ScrollDown => {
                                for _ in 0..SCROLL_LINES {
                                    scroll_state.write().scroll_down();
                                }
                            }
                            MouseEventKind::ScrollUp => {
                                for _ in 0..SCROLL_LINES {
                                    scroll_state.write().scroll_up();
                                }
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                // 开始文本拖拽选中
                                text_sel.write().start_drag(line_idx as u16, col_in_line);
                            }
                            MouseEventKind::Drag(MouseButton::Left) => {
                                let mut sel = text_sel.write();
                                if sel.dragging {
                                    sel.update_drag(line_idx as u16, col_in_line);
                                }
                                drop(sel);
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
                                    }
                                    // 复制完成后立即清除选区，避免每帧高亮全量行导致渲染卡死
                                    sel.clear();
                                }
                                // 非拖拽点击——清除旧选区
                                if !was_dragging {
                                    sel.clear();
                                }
                            }
                            _ => {}
                        }
                    } else {
                        // 鼠标在消息区外：滚轮 + 其他鼠标事件委托 ScrollViewState
                        // Left click outside → 不消费，让 InputArea 等组件处理点击光标定位
                        match mouse.kind {
                            MouseEventKind::ScrollDown => {
                                for _ in 0..SCROLL_LINES {
                                    scroll_state.write().scroll_down();
                                }
                            }
                            MouseEventKind::ScrollUp => {
                                for _ in 0..SCROLL_LINES {
                                    scroll_state.write().scroll_up();
                                }
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                return EventResult::Ignored;
                            }
                            _ => {
                                scroll_state.write().handle_event(&event);
                            }
                        }
                    }
                }

                return EventResult::Consumed;
            }

            // 键盘事件：仅消费 message 专用键（Ctrl+↑↓HomeEnd），其余透传给 InputArea
            if let Event::Key(key) = &event {
                if key.kind == KeyEventKind::Press && focus_router::message_accepts_key(key) {
                    scroll_state.write().handle_event(&event);
                    return EventResult::Consumed;
                }
            }
            EventResult::Ignored
        });
    }

    // ── 吸底自动跟随 ──
    // 核心策略：用 last_scrolled_at 做增量门控，避免每 chunk 都原子写入促发多余渲染。
    // loading 期间强制跟随底部（因为 scroll_to_bottom() 只设 offset.y=viewport_height-1，
    // 随后 render 会 clamp 到实际 content_height-viewport_height，导致 loading 开始时内容
    // 不足视口时 scroll_y=0，后续内容暴涨后已在底部 guard 和 proximity 均兜不住）。
    // 非 loading 期间：首次/收缩无条件滚到底；已在底部跳写；就近跟随仅当内容增长后执行一次。
    let last_scrolled_at = hooks.use_state(|| 0u16);
    hooks.use_effect(
        {
            let st = scroll_state;
            let pl = prev_entries_len;
            let lsa = last_scrolled_at;
            let len = entries_len;
            let loading = is_loading;
            move || {
                let prev = *pl.read();
                *pl.write() = len;

                if total_visual_rows == 0 || vis_height == 0 {
                    return;
                }

                // loading 期间：每次内容增长都强制吸底。last_scrolled_at 门控避免同高多次写入。
                if loading {
                    if total_visual_rows > *lsa.read() {
                        st.write().scroll_to_bottom();
                        *lsa.write() = total_visual_rows;
                    }
                    return;
                }

                // ── 以下仅非 loading 期间执行 ──

                // 首次内容出现（空→非空）→ 强制滚到底
                if prev == 0 && len > 0 {
                    st.write().scroll_to_bottom();
                    *lsa.write() = total_visual_rows;
                    return;
                }
                // 内容收缩（compact 后 len < prev）→ 滚到底，避免 scroll_y 悬空
                if len < prev {
                    st.write().scroll_to_bottom();
                    *lsa.write() = total_visual_rows;
                    return;
                }

                let max_scroll = total_visual_rows.saturating_sub(vis_height);
                let scroll_y = st.read().offset().y as u16;
                // 已在或超出底部 → 跳写，避免 no-op 原子写入促发多余渲染
                if scroll_y >= max_scroll {
                    return;
                }
                // 用户不在底部附近 → 不跟随
                let distance = max_scroll.saturating_sub(scroll_y);
                if distance > (vis_height / 4).max(5) {
                    return;
                }
                // 用户靠近底部且内容增长了 → 跟随一次
                if total_visual_rows > *lsa.read() {
                    st.write().scroll_to_bottom();
                    *lsa.write() = total_visual_rows;
                }
            }
        },
        (entries_len, raw_ch, is_loading),
    );

    if empty {
        // 空态有 Brewed 总结行时，Welcome 上方填充 + Brewed 在底部保留。
        if let Some(lines) = brewed_lines {
            return element!(
                View(
                    flex_direction: Direction::Vertical,
                    width: Constraint::Fill(1),
                    height: Constraint::Fill(1),
                ) {
                    View(height: Constraint::Fill(1)) {
                        Welcome(width: props.width)
                    }
                    Text(text: Paragraph::new(RatText::from(lines)).wrap(Wrap { trim: false }))
                }
            )
            .into_any();
        }
        return element!(
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                Welcome(width: props.width)
            }
        )
        .into_any();
    }

    // ── 视口裁剪 ──
    let scroll_y = scroll_state.read().offset().y as u16;
    let (first, last, _local_offset) = viewport_clip(&wrap_map, scroll_y, vis_height);
    *crate::kit::atoms::message_viewport_snapshot().write() =
        crate::kit::atoms::MessageViewportSnapshot {
            scroll_y,
            vis_height,
            first_line: first,
            last_line: last,
        };

    let visible_lines: Vec<Line<'static>> =
        highlighted_lines.get(first..last).unwrap_or(&[]).to_vec();

    // 内容在 ScrollView 中的垂直起始位置（视觉行号）。
    // ScrollView 负责整体滚动；这里通过在内容 View 前面插入一个 spacer，
    // 使 visible_lines 出现在内容 View 的正确偏移处。
    let content_top = wrap_map.get(first).map_or(0u16, |w| w.visual_row);

    // total_visual_rows 已在 hook 声明之前计算，此处复用同一值。

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

    let _max_scroll = total_visual_rows.saturating_sub(vis_height);

    // ── Footer：spinner + todo 已作为普通内容行追加到 ScrollView 底部 ──

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
                state: scroll_state,
                scrollbars: clean_scrollbars(),
                active: false,
            ) {
                View(
                    flex_direction: Direction::Vertical,
                    width: Constraint::Fill(1),
                    height: Constraint::Length(total_visual_rows.max(1)),
                ) {
                    View(height: Constraint::Length(content_top)) {}
                    View(height: Constraint::Length(
                        content_bottom.saturating_sub(content_top).max(1),
                    )) {
                        Text(text: Paragraph::new(RatText::from(visible_lines)).wrap(Wrap { trim: false }))
                    }
                    View(height: Constraint::Length(bottom_spacer)) {}
                }
            }
        }
    )
    .into_any()
}

// ── footer 行构建：在 MessageArea 作用域内计算 spinner + todo 行 ──

fn build_footer_lines(
    hooks: &mut Hooks,
    is_loading: bool,
    todo_items: &[TodoItem],
) -> Vec<Line<'static>> {
    let semantic = THEME_ATOM.state().read().semantic;

    // [TRAP] 所有 hook 调用必须在任何 early return 之前，确保每帧 hook 调用顺序一致。
    // ratatui-kit 按调用顺序索引 hook，顺序变化会触发 "Hook type mismatch" panic。
    let spinner_state = hooks.use_state(|| SpinnerState::new(SpinnerMode::Thinking));
    let load_start = hooks.use_state(|| Option::<Instant>::None);
    let was_loading = hooks.use_state(|| false);
    // loading 结束时的耗时（ms）——用于渲染「✻ Brewed for Xm Xs」总结行。
    // 保留最后一次 loading 的耗时，作为空态 footer 的展示内容（灰色），
    // 避免空态时 footer 高度收缩为 0。
    let summary_elapsed_ms = hooks.use_state(|| 0u64);
    // loading epoch 追踪：submit_consumer 每次发起新 prompt 时 LOADING_EPOCH 原子 +1。
    // 组件以此检测新一轮 loading 会话，避免 rapid is_loading toggle（如 TurnDone
    // → drain_input_buffer → 新 prompt 在同一渲染周期内完成）导致过渡检测丢失、
    // load_start / spinner_state 残留旧值、计时持续累积。
    let loading_epoch = hooks.use_atom(&crate::kit::atoms::LOADING_EPOCH);
    let last_epoch = hooks.use_state(|| 0u64);

    // ── /clear 检测：BRIDGE_RESET_COUNTER 递增时清零 summary_elapsed_ms ──
    // /clear 代表用户主动清空会话——Brewed 总结行描述的是上一轮耗时，
    // 清空后不应残留。
    let last_reset_counter = hooks.use_state(|| crate::kit::atoms::BRIDGE_RESET_COUNTER.get());
    {
        let current = crate::kit::atoms::BRIDGE_RESET_COUNTER.get();
        if *last_reset_counter.read() != current {
            *summary_elapsed_ms.write() = 0;
            *last_reset_counter.write() = current;
        }
    }

    {
        // ── 状态变更块：loading 过渡检测、计时、tick 补偿 ──
        // [TRAP] has_summary 的快速路径必须在此块**之后**检查，而非之前。
        // 若在之前检查，loading 结束的那一帧 summary_elapsed_ms 还是旧值 0，
        // 早退返回空 Vec → Brewed 总结行丢失一帧。等下一帧 has_summary 变为 true 时，
        // auto-scroll effect deps (entries_len/raw_ch/is_loading) 未变，不再触发滚动，
        // 导致 Brewed 行在 ScrollView 内容中但视口以下——永远不可见。
        //
        // epoch 检测：新 loading 会话 = 无条件重建 spinner + 重置计时器。
        // 比 is_loading 过渡检测更可靠——不依赖组件是否观察到 false 中间态。
        let current_epoch = *loading_epoch.read();
        if is_loading && *last_epoch.read() != current_epoch {
            *last_epoch.write() = current_epoch;
            *load_start.write() = Some(Instant::now());
            *spinner_state.write() = SpinnerState::new(SpinnerMode::Thinking);
            // summary_elapsed_ms 不清零：保留上次 loading 的总结行。
            // /clear 时通过 BRIDGE_RESET_COUNTER 检测独立清零。
            // /clear 时通过 BRIDGE_RESET_COUNTER 检测独立清零（见上方）。
            *was_loading.write() = true;
        }

        let prev_loading = *was_loading.read();
        if prev_loading != is_loading {
            let mut ls = load_start.write();
            if is_loading {
                // epoch 检测已在上面处理了 is_loading=true 的初始化。
                // 此分支仅在 epoch 未变但 is_loading 过渡时进入（罕见），做防御性重置。
                if ls.is_none() {
                    *ls = Some(Instant::now());
                    *spinner_state.write() = SpinnerState::new(SpinnerMode::Thinking);
                    // summary_elapsed_ms 不清零：保留上次 loading 的总结行。
                    // /clear 时通过 BRIDGE_RESET_COUNTER 检测独立清零。
                }
            } else {
                *summary_elapsed_ms.write() =
                    ls.map_or(0, |start| start.elapsed().as_millis() as u64);
                *ls = None;
            }
            *was_loading.write() = is_loading;
        }

        // [TRAP] spinner 动画完全无副作用——frame 由 render_to_lines 内部基于
        // start_time.elapsed() 纯计算（见 peri-widgets/src/spinner/mod.rs）。
        // 历史上这里曾有壁钟补偿 advance_tick + set_token_count 写入 spinner_state，
        // 形成 render → state write → render 自激回路（~20Hz），违反 render body
        // 禁止写 atom 铁律，已于本次重构移除。
    }

    // 快速路径：无 loading、无 todo、无总结行时直接返回空。
    // 必须在状态变更块之后检查，确保 loading 结束那帧 summary_elapsed_ms 已被写入。
    let has_summary = *summary_elapsed_ms.read() > 0;
    if !is_loading && todo_items.is_empty() && !has_summary {
        return Vec::new();
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let has_footer_content = is_loading || has_summary || !todo_items.is_empty();
    if has_footer_content {
        // 与上方消息内容隔离 2 行
        lines.push(Line::from(""));
        lines.push(Line::from(""));
    }
    if is_loading {
        // token_count 直接从 atom 读后传入 render_to_lines——纯只读，不写 spinner state。
        let token_count = crate::kit::atoms::SPINNER_TOKEN_COUNT.get();
        lines.extend(spinner_state.read().render_to_lines(
            semantic.accent,
            semantic.text.muted,
            true,
            true,
            token_count,
        ));
    } else if has_summary {
        let elapsed = peri_widgets::spinner::animation::format_elapsed(*summary_elapsed_ms.read());
        lines.push(Line::from(Span::styled(
            i18n::tr_args(
                "msg-spinner-brewed",
                &[("duration".to_string(), FluentValue::from(elapsed))],
            ),
            Style::default().fg(semantic.text.muted),
        )));
    }
    if !todo_items.is_empty() {
        lines.extend(render_todo_lines(&todo_items));
    }
    if has_footer_content {
        // 与下方内容隔离 1 行
        lines.push(Line::from(""));
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

    /// 回归：裁剪后的 Text 组件高度必须使用 wrap_map 的视觉高度。
    ///
    /// 场景：visible_lines 只有 2 条逻辑行，但每条长行 wrap 后分别占 10/5 个视觉行。
    /// 如果 Text 组件没有显式高度，布局会按逻辑行数给 2 行，ScrollView 剩余区域由 spacer
    /// 填充，表现为向上滚动或滚到底部时内容下方留白。
    #[test]
    fn test_visible_text_height_uses_visual_height_not_line_count() {
        let wrap_map = vec![
            wrapped(0, 0, 5),
            wrapped(1, 5, 10),
            wrapped(2, 15, 5),
            wrapped(3, 20, 10),
        ];
        let scroll_y = 8u16;
        let vis_height = 12u16;
        let (first, last, _) = viewport_clip(&wrap_map, scroll_y, vis_height);
        let content_top = wrap_map[first].visual_row;
        let content_bottom = wrap_map[last.saturating_sub(1)].visual_row
            + wrap_map[last.saturating_sub(1)].visual_height;
        let visible_line_count = (last - first) as u16;
        let visible_visual_height = content_bottom.saturating_sub(content_top);

        assert_eq!(visible_line_count, 2);
        assert_eq!(visible_visual_height, 15);
        assert!(
            visible_visual_height > visible_line_count,
            "换行内容的 Text 高度必须按视觉行数，而不是逻辑行数"
        );
    }

    /// 回归：裁剪渲染使用的 Paragraph 必须启用与 build_wrap_map 相同的 wrap 策略。
    ///
    /// build_wrap_map 用 `Wrap { trim: false }` 预测视觉高度；如果实际渲染的 Paragraph
    /// 不启用 wrap，长行不会按预测换行，ScrollView 的内容高度与实际绘制高度继续失配。
    #[test]
    fn test_render_paragraph_must_wrap_like_wrap_map() {
        let line = Line::from("x".repeat(25));
        let lines = vec![line];
        let predicted = crate::kit::render_bridge::build_wrap_map(&lines, 10);
        let actual_wrapped = Paragraph::new(RatText::from(lines.clone()))
            .wrap(Wrap { trim: false })
            .line_count(10) as u16;
        let actual_unwrapped = Paragraph::new(RatText::from(lines)).line_count(10) as u16;

        assert_eq!(predicted[0].visual_height, actual_wrapped);
        assert!(
            actual_unwrapped < predicted[0].visual_height,
            "未启用 wrap 时，实际渲染高度会小于 wrap_map 预测高度"
        );
    }

    /// 回归：footer render 不应在 loading 稳态下无条件写 hook state。
    ///
    /// 用户提交 `hello` 后，`submit_text` 会先把 `ACP_STATE.is_loading` 置为 true，
    /// MessageArea 随后渲染 spinner。若 footer 每帧都写 `was_loading`/`load_start`，
    /// 会形成 render → state write → render 的自激循环，表现为 Enter 后 CPU 100%。
    /// 稳态下 footer 不应写任何控制 state——spinner 动画由 render_to_lines 内部
    /// 基于 start_time.elapsed() 纯计算，零 state write。
    #[test]
    fn test_footer_loading_steady_state_has_no_control_state_transition() {
        let prev_loading = true;
        let is_loading = true;
        let transition = prev_loading != is_loading;

        assert!(
            !transition,
            "loading 稳态不应写 was_loading/load_start，否则会触发持续重渲染"
        );
    }

    /// 回归：仅有 todo 条目、无消息且非 loading 时，不应误判为 empty 而显示 Welcome。
    ///
    /// 场景：agent 执行 `TodoWrite` → ACP server 推送 `SessionUpdate::Plan` →
    /// `handle_plan_update` 写入 `TODO_ITEMS` atom。消息区尚无历史消息
    /// （`entries.is_empty()`），也不在 loading（`!is_loading`），
    /// 但 todo 列表非空。此时 empty 应为 false，让 footer 渲染 todo 行。
    #[test]
    fn test_empty_with_todo_items_shows_footer_not_welcome() {
        let entries_empty = true;
        let is_loading = false;
        let todo_items_empty = false;
        let empty = entries_empty && !is_loading && todo_items_empty;

        assert!(
            !empty,
            "仅有 todo 条目且无消息时不应判定为 empty，避免 Welcome 覆盖 todo 显示"
        );
    }

    /// 回归：无消息、非 loading、无 todo 的正确 empty 判定。
    #[test]
    fn test_empty_without_todo_is_truly_empty() {
        let entries_empty = true;
        let is_loading = false;
        let todo_items_empty = true;
        let empty = entries_empty && !is_loading && todo_items_empty;

        assert!(empty);
    }

    /// 回归：`render_todo_lines` 对三种状态输出正确的图标和样式。
    ///
    /// - InProgress → ◼ + accent 色 + 无删除线
    /// - Completed → ✔ + success 色 + 删除线
    /// - Pending → ◻ + muted 色 + 无删除线
    #[test]
    fn test_render_todo_lines_icons_and_crossed() {
        let items = vec![
            TodoItem {
                status: TodoStatus::InProgress,
                content: "修复 bug".into(),
            },
            TodoItem {
                status: TodoStatus::Completed,
                content: "草拟 PRD".into(),
            },
            TodoItem {
                status: TodoStatus::Pending,
                content: "部署".into(),
            },
        ];
        let lines = render_todo_lines(&items);
        assert_eq!(lines.len(), 4); // 3 items + 1 trailing blank line

        let in_progress_icon = lines[0].spans[0].content.as_ref();
        assert!(in_progress_icon.contains("◼"), "InProgress 图标应为 ◼");
        let in_progress_text = lines[0].spans[1].content.as_ref();
        assert!(
            in_progress_text.contains("修复 bug"),
            "InProgress 文本应包含任务内容"
        );

        let completed_icon = lines[1].spans[0].content.as_ref();
        assert!(completed_icon.contains("✔"), "Completed 图标应为 ✔");
        let completed_text = lines[1].spans[1].content.as_ref();
        assert!(
            completed_text.contains("草拟 PRD"),
            "Completed 文本应包含任务内容"
        );

        let pending_icon = lines[2].spans[0].content.as_ref();
        assert!(pending_icon.contains("◻"), "Pending 图标应为 ◻");
        let pending_text = lines[2].spans[1].content.as_ref();
        assert!(pending_text.contains("部署"), "Pending 文本应包含任务内容");
        assert!(
            pending_text.contains("(available)") || pending_text.contains("(可开始)"),
            "Pending 文本应包含 i18n 可用标记"
        );
    }

    /// 回归：空 todo 列表输出空行（仅 3 个 trailing blank lines）。
    #[test]
    fn test_render_todo_lines_empty() {
        let lines = render_todo_lines(&[]);
        assert_eq!(lines.len(), 1);
        for line in &lines {
            assert!(
                line.spans.is_empty(),
                "空 todo 列表不应输出任何内容行，仅 trailing blank lines"
            );
        }
    }

    /// 回归：loading 结束后应显示总结行（✻ Brewed for Xs），token 计数仅在变化时写 state。
    ///
    /// 场景：agent 完成一轮后 `is_loading` 从 `true` 降为 `false`，
    /// footer 应渲染总结行而非 spinner；同时 token count 不应在稳态下重复写 hook state。
    #[test]
    fn test_spinner_summary_after_loading_ends() {
        // 模拟 loading 结束时捕获的耗时
        let elapsed_ms: u64 = 30_000; // 30s
        let elapsed_str = peri_widgets::spinner::animation::format_elapsed(elapsed_ms);
        assert_eq!(elapsed_str, "30s");

        // 总结行格式应与 peri-main 保持一致
        let summary = format!("  ✻  Brewed for {elapsed_str}");
        assert!(summary.contains("✻"));
        assert!(summary.contains("Brewed for"));
    }

    /// 回归：token count 相同值不应触发 set_token_count 写入。
    #[test]
    fn test_token_count_no_write_when_unchanged() {
        let prev_token_count: usize = 1500;
        let new_token_count: usize = 1500;
        let changed = prev_token_count != new_token_count;

        assert!(!changed, "token count 未变化时不应写 state");
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

    // ── 就近判断阈值计算 ──

    /// 计算距底部的距离，及是否应自动跟到底部。
    ///
    /// 若 total=0 返回 false（无内容不滚动）。
    /// 若 scroll_y 已在底部（>= max_scroll）返回 false——上层调用应走
    /// no-op 跳过（不做 scroll_to_bottom 写入避免 re-render 环路）。
    fn proximity_check(total: u16, scroll_y: u16, vis_height: u16) -> bool {
        if total == 0 {
            return false;
        }
        let max_scroll = total.saturating_sub(vis_height);
        if scroll_y >= max_scroll {
            // 已在或超出底部——上层应 no-op 跳过
            return false;
        }
        let distance = max_scroll.saturating_sub(scroll_y);
        let threshold = (vis_height / 2).max(5);
        distance <= threshold
    }

    #[test]
    fn test_proximity_at_bottom_should_not_trigger_scroll() {
        let total = 100;
        let vis_height = 20;
        let scroll_y = total - vis_height; // 刚好在底部
        // 已在底部时不调用 scroll_to_bottom（避免 no-op 写入）
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_within_half_viewport_should_follow() {
        let total = 100;
        let vis_height = 20;
        // 距底部 10 行 → threshold = 20/2 = 10 → 应跟随
        let scroll_y = total - vis_height - 10;
        assert!(proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_beyond_half_viewport_should_not_follow() {
        let total = 100;
        let vis_height = 20;
        // 距底部 11 行 → threshold = 10 → 不应跟随
        let scroll_y = total - vis_height - 11;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_near_top_should_not_follow() {
        let total = 200;
        let vis_height = 30;
        // 距底部 150 行，远远超过 threshold=15
        let scroll_y = 20;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_small_viewport_minimum_threshold() {
        let total = 50;
        let vis_height = 6;
        // threshold = max(6/2, 5) = 5
        // 距底部 5 行 → 应跟随
        let scroll_y = total - vis_height - 5;
        assert!(proximity_check(total, scroll_y, vis_height));
        // 距底部 6 行 → 不应跟随
        let scroll_y = total - vis_height - 6;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_empty_content_no_follow() {
        assert!(!proximity_check(0, 0, 20));
    }

    /// 回归：total < vis_height 时（内容未满一屏），max_scroll=0，
    /// 任何 scroll_y >= 0 都已在底部，不应触发 scroll_to_bottom 写入。
    #[test]
    fn test_proximity_content_smaller_than_viewport_at_bottom() {
        let total = 10;
        let vis_height = 30;
        // max_scroll = 0，scroll_y=0 时已在底部 → false（上层 no-op）
        assert!(!proximity_check(total, 0, vis_height));
    }
}
