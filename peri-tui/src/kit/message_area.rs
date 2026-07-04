//! MessageArea：仅依赖 RENDER_CACHE 渲染消息，不再订阅完整 ViewStore。
//!
//! RENDER_CACHE atom 变化时，LineCache 根据 (len, ch, loading) key 重建 lines。
//! 滚动/terminal resize 不触发重建——仅重建 Vec<Line>，不做 markdown 解析。
//!
//! - 滚动：由 ScrollViewState 处理键盘/鼠标事件
//! - 智能跟随：use_effect 检测 CurrentTurn 出现

#![allow(clippy::needless_update)]

use crate::kit::atoms::{ACP_STATE, RENDER_CACHE};
use crate::kit::focus_router;
use crate::kit::panel_registry::clean_scrollbars;
use crate::kit::theme;
use crate::kit::welcome::Welcome;
use ratatui_kit::{
    components::ScrollViewState,
    crossterm::event::{Event, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::Style,
        text::{Line, Span},
    },
};

// ── 本地行缓存（仅 RENDER_CACHE 内容变化时重建，滚动不触发）─────────────────

#[derive(Default)]
struct LineCache {
    key: u64,
    lines: Vec<Line<'static>>,
    content_h: usize,
    current_has_ct: bool,
}

#[derive(Default, Props)]
pub struct MessageAreaProps {
    pub width: usize,
}

#[component]
pub fn MessageArea(props: &MessageAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let semantic = theme::semantic();

    let render_cache = hooks.use_atom(&RENDER_CACHE);
    let acp = hooks.use_atom(&ACP_STATE);
    let cache_snapshot = render_cache.read();
    let is_loading = acp.read().is_loading;

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
        let mut lc = line_cache.write();
        lc.key = new_key;
        lc.lines = lines;
        lc.content_h = raw_ch.saturating_add(if is_loading { 1 } else { 0 });
        lc.current_has_ct = ct;
    }

    let cache = line_cache.read();
    let empty = cache.lines.is_empty();
    let content_lines = cache.lines.clone();
    let current_has_ct = cache.current_has_ct;
    drop(cache);
    drop(cache_snapshot);

    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        if let Event::Key(key) = &event {
            let _ = focus_router::message_accepts_key(key);
        }
        // 鼠标事件：交 ScrollView 内置滚动处理
        if matches!(&event, Event::Mouse(_)) {
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

    element!(
        ScrollView(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
            scroll_view_state: scroll_state,
            scroll_bars: clean_scrollbars(),
        ) {
            for (i, line) in content_lines.iter().enumerate() {
                View(key: i, height: Constraint::Length(1)) {
                    Text(text: line.clone())
                }
            }
        }
    )
    .into_any()
}
