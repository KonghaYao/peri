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

use crate::kit::atoms::{COPY_CHAR_COUNT, COPY_MESSAGE_UNTIL, RENDER_CACHE};
use crate::kit::focus_router;
use crate::kit::panel_registry::clean_scrollbars;
use crate::kit::render_bridge::WrappedLineInfo;
use crate::kit::text_selection::{self, TextSelection};
use crate::kit::theme;
use crate::kit::welcome::Welcome;
use ratatui_kit::{
    components::ScrollViewState,
    crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Rect},
        style::Style,
        text::{Line, Span, Text as RatText},
        widgets::Paragraph,
    },
};

// ── 本地行缓存（仅 RENDER_CACHE 内容变化时重建，滚动不触发）─────────────────

#[derive(Default)]
struct LineCache {
    key: u64,
    current_has_ct: bool,
    /// 拷贝自 RENDER_CACHE.wrap_map——viewport_clip 二分查找用
    wrap_map: Vec<WrappedLineInfo>,
}

// ── 视口裁剪 ────────────────────────────────────────────────────────────────

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
    let vis_end = (scroll_y + vis_height).min(total);

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

    let render_cache = hooks.use_atom(&RENDER_CACHE);
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
        lc.wrap_map = cache_snapshot.wrap_map.clone();
    }

    let lc_data = line_cache.read();
    let current_has_ct = lc_data.current_has_ct;
    let wrap_map_base = lc_data.wrap_map.clone();
    drop(lc_data);

    // 构建全量行（基于 cache_snapshot entries，不含空行分隔符，含 spinner）
    let mut all_lines: Vec<Line<'static>> = cache_snapshot
        .entries
        .iter()
        .flat_map(|(_, entry)| entry.lines.iter().cloned())
        .collect();
    if is_loading {
        all_lines.push(Line::from(vec![Span::styled(
            "◜ 思考中…",
            Style::default().fg(semantic.status.running),
        )]));
    }
    let empty = cache_snapshot.entries.is_empty() && !is_loading;
    let content_lines = Arc::new(all_lines.clone());
    drop(cache_snapshot);

    // ── 消息区位置追踪 ──
    let area_hook = hooks.use_hook(MsgAreaTracker::new);
    let area_rect = area_hook.rect;

    // ── 文本选区状态 ──
    let text_sel = hooks.use_state(TextSelection::new);

    // ── 事件处理（鼠标选中 + 滚动 + 键盘） ──
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
                    let in_area = mouse.row >= area.y
                        && mouse.row < area.y + area.height
                        && mouse.column >= area.x
                        && mouse.column < area.x + area.width;

                    if in_area {
                        let scroll_y = scroll_state.read().offset().y;
                        let visual_row = mouse.row - area.y + scroll_y;
                        let visual_col = mouse.column - area.x;

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
                                            let char_count = text.chars().count();
                                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                                let _ = clipboard.set_text(text);
                                            }
                                            // 设置复制提示（状态栏显示 "已复制 N 字符"）
                                            *COPY_CHAR_COUNT.state().write() = char_count;
                                            *COPY_MESSAGE_UNTIL.state().write() = Some(
                                                std::time::Instant::now()
                                                    + std::time::Duration::from_millis(2000),
                                            );
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

    // ── 选区高亮 + 视口裁剪 ──
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

    // 扩展 wrap_map 包含 spinner（如果 loading）
    let mut wrap_map = wrap_map_base;
    if is_loading && !wrap_map.is_empty() {
        let spinner_vis_row = wrap_map
            .last()
            .map_or(0, |w| w.visual_row + w.visual_height);
        wrap_map.push(WrappedLineInfo {
            line_idx: highlighted_lines.len().saturating_sub(1),
            visual_row: spinner_vis_row,
            visual_height: 1,
        });
    }

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
