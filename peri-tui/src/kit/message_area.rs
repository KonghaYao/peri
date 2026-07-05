//! MessageArea：仅依赖 RENDER_CACHE 渲染消息，不再订阅完整 ViewStore。
//!
//! RENDER_CACHE atom 变化时，LineCache 根据 (len, ch, loading) key 重建 lines。
//! 滚动/terminal resize 不触发重建——仅重建 Vec<Line>，不做 markdown 解析。
//!
//! - 滚动：由 ScrollViewState 处理键盘/鼠标事件
//! - 智能跟随：use_effect 检测 CurrentTurn 出现
//! - 鼠标文本选中：Down 开始拖拽 → Drag 更新选区 → Up 提取文本并复制到剪贴板

#![allow(clippy::needless_update)]

use std::sync::Arc;

use crate::kit::atoms::{COPY_CHAR_COUNT, COPY_MESSAGE_UNTIL, RENDER_CACHE};
use crate::kit::focus_router;
use crate::kit::panel_registry::clean_scrollbars;
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
    lines: Vec<Line<'static>>,
    content_h: usize,
    current_has_ct: bool,
    /// H6：预计算的选区高亮 chunks（每 CHUNK_LINES 行一组）。
    /// 仅在 key 变化时重建，帧间通过 Arc clone 共享——消除每帧 4 次全量 Line clone。
    highlight_chunks: Arc<[(Vec<Line<'static>>, u16)]>,
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
    // H6: is_loading 从 RENDER_CACHE 推断——存在 CurrentTurn 说明 agent 在运行。
    // 不再订阅 ACP_STATE，避免流式期间 view_count 每次变化都触发无意义重渲染。
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
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut ct = false;
        for (key, entry) in cache_snapshot.entries.iter() {
            if matches!(key, crate::kit::render_bridge::VmKey::CurrentTurn(_)) {
                ct = true;
            }
            for line in entry.lines.iter() {
                lines.push(line.clone());
            }
            lines.push(Line::from(""));
        }
        if is_loading {
            lines.push(Line::from(vec![Span::styled(
                "◜ 思考中…",
                Style::default().fg(semantic.status.running),
            )]));
        }
        // H6：预计算 chunks 并缓存，帧间避免 4 次全量 Line clone。
        const CHUNK_LINES: usize = 200;
        let chunks: Vec<(Vec<Line<'static>>, u16)> = lines
            .chunks(CHUNK_LINES)
            .map(|c| (c.to_vec(), c.len() as u16))
            .collect();
        let mut lc = line_cache.write();
        lc.key = new_key;
        lc.lines = lines;
        lc.content_h = raw_ch.saturating_add(if is_loading { 1 } else { 0 });
        lc.current_has_ct = ct;
        lc.highlight_chunks = Arc::from(chunks);
    }

    let cache = line_cache.read();
    let empty = cache.lines.is_empty();
    let content_lines = Arc::new(cache.lines.clone());
    let current_has_ct = cache.current_has_ct;
    let cached_chunks_arc = Arc::clone(&cache.highlight_chunks);
    drop(cache);
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

    // ── 选区高亮（字符级，通过 span Style 传递） ──
    // H6：无选区时直接用 LineCache 缓存的 chunks（Arc clone O(1)），
    // 避免每帧 content_lines.to_vec() + chunks 重新分配 ~20000 次 Line clone。
    let sel = text_sel.read();
    let is_active_sel = sel.is_active();
    let cached_chunks = Arc::clone(&cached_chunks_arc);

    let display_chunks: Vec<(Vec<Line<'static>>, u16)> = if is_active_sel {
        // 选区活跃时退化为旧路径——重新计算（罕见场景，可接受回退代价）
        let hl = if let Some(((sr, sc), (er, ec))) = sel.normalized_bounds() {
            text_selection::highlight_selected_lines(&content_lines, sr, sc, er, ec)
        } else {
            content_lines.to_vec()
        };
        let chunk_sz = 200usize;
        hl.chunks(chunk_sz)
            .map(|c| (c.to_vec(), c.len() as u16))
            .collect()
    } else {
        cached_chunks.to_vec()
    };
    drop(sel);

    // H5: 性能说明——分块渲染避免 O(N) View widget 和单 Paragraph 溢出
    // 长历史 thread 可达 5000+ 行。逐行 View 在 terminal.draw() 中 O(N) 布局。
    // 改为每 200 行一个 chunk → 5000 行 = 25 个 View widget。
    // H6：chunks 由 LineCache 缓存，帧间仅 Arc clone。
    let total_height: u16 = display_chunks.iter().map(|(_, h)| *h).sum::<u16>().max(1);
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
                height: Constraint::Length(total_height),
            ) {
                for (_i, (chunk_lines, _)) in display_chunks.iter().enumerate() {
                    Text(text: Paragraph::new(RatText::from(chunk_lines.clone())))
                }
            }
        }
    )
    .into_any()
}
