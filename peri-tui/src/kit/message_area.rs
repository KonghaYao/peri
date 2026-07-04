//! MessageArea：消息流渲染区——render_bridge 预计算 lines + 本地 LineCache。
//!
//! RENDER_CACHE atom 变化时，LineCache 根据 (len, ch, loading) key 重建 lines。
//! 滚动/terminal resize 不触发重建——仅重建 Vec<Line>，不做 markdown 解析。
//!
//! - 滚动：Ctrl+Up/Down/Home/End + 鼠标滚轮
//! - 智能跟随：use_effect 检测 CurrentTurn 出现

#![allow(clippy::needless_update)]

use crate::kit::atoms::{ACP_STATE, RENDER_CACHE};
use crate::kit::focus_router;
use crate::kit::theme;
use crate::kit::welcome::Welcome;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction, Position},
        style::Style,
        text::{Line, Span},
        widgets::{Paragraph, Wrap},
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
    let all_lines = &cache.lines;
    let empty = all_lines.is_empty();
    let content_h = cache.content_h;
    let current_has_ct = cache.current_has_ct;
    drop(cache_snapshot);

    // ── 滚动状态 ──
    let scroll_offset = hooks.use_state(|| 0u16);
    let mut auto_scroll = hooks.use_state(|| true);
    let (_, term_h) = hooks.use_terminal_size();
    let vp_h: u16 = term_h.saturating_sub(4).max(1);
    let max_scroll: u16 = content_h
        .saturating_sub(vp_h as usize)
        .min(u16::MAX as usize) as u16;

    // use_effect：仅 current_has_ct 变化时运行，恢复自动滚动
    let had_ct = hooks.use_state(|| false);
    hooks.use_effect(
        {
            let mut a = auto_scroll;
            let mut h = had_ct;
            move || {
                if !h.get() && current_has_ct {
                    a.set(true);
                }
                h.set(current_has_ct);
            }
        },
        (current_has_ct,),
    );

    // 自动滚底 + clamp（仅值变化时写入）
    {
        let mut new_val = *scroll_offset.read();
        if auto_scroll.get() {
            new_val = max_scroll;
        }
        new_val = new_val.min(max_scroll);
        if new_val != *scroll_offset.read() {
            *scroll_offset.write() = new_val;
        }
    }
    let offset = *scroll_offset.read();

    // ── 事件处理 ──
    hooks.use_event_handler(
        EventScope::Global,
        EventPriority::High,
        move |event| match event {
            Event::Key(key)
                if key.kind == KeyEventKind::Press
                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let _ = focus_router::message_accepts_key(&key);
                match key.code {
                    KeyCode::Up => {
                        auto_scroll.set(false);
                        *scroll_offset.write() = scroll_offset.read().saturating_sub(3);
                        EventResult::Consumed
                    }
                    KeyCode::Down => {
                        *scroll_offset.write() = (*scroll_offset.read() + 3).min(max_scroll);
                        EventResult::Consumed
                    }
                    KeyCode::Home => {
                        auto_scroll.set(false);
                        *scroll_offset.write() = 0;
                        EventResult::Consumed
                    }
                    KeyCode::End => {
                        auto_scroll.set(true);
                        *scroll_offset.write() = max_scroll;
                        EventResult::Consumed
                    }
                    _ => EventResult::Ignored,
                }
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => {
                    auto_scroll.set(false);
                    *scroll_offset.write() = scroll_offset.read().saturating_sub(3);
                    EventResult::Consumed
                }
                MouseEventKind::ScrollDown => {
                    *scroll_offset.write() = (*scroll_offset.read() + 3).min(max_scroll);
                    EventResult::Consumed
                }
                _ => EventResult::Ignored,
            },
            _ => EventResult::Ignored,
        },
    );

    // ── 渲染 ──
    if empty {
        return element!(
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                Welcome(width: props.width)
            }
        )
        .into_any();
    }

    let text_content = Paragraph::new(all_lines.clone()).wrap(Wrap { trim: false });
    let needs_bar = content_h > vp_h as usize;

    element!(
        View(
            flex_direction: Direction::Horizontal,
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                Text(text: text_content, scroll: Position::new(0, offset), wrap: false)
            }
            { if needs_bar {
                element!(scrollbar(
                    offset: offset,
                    content_h: content_h.min(u16::MAX as usize) as u16,
                    vp_h: vp_h,
                )).into_any()
            } else {
                element!(View(width: Constraint::Length(0), height: Constraint::Fill(1))).into_any()
            } }
        }
    )
    .into_any()
}

#[derive(Clone, Props, Default)]
struct ScrollbarProps {
    offset: u16,
    content_h: u16,
    vp_h: u16,
}

#[component]
#[allow(non_camel_case_types)]
fn scrollbar(props: &ScrollbarProps) -> impl Into<AnyElement<'static>> {
    let max_s = props.content_h.saturating_sub(props.vp_h);
    if max_s == 0 {
        return element!(View(width: Constraint::Length(0), height: Constraint::Fill(1)));
    }
    let thumb_h = ((props.vp_h as u64 * props.vp_h as u64) / props.content_h as u64)
        .max(1)
        .min(props.vp_h as u64) as u16;
    let thumb_y =
        ((props.vp_h.saturating_sub(thumb_h) as u64 * props.offset as u64) / max_s as u64) as u16;
    let dim = theme::semantic().text.dim;
    let top_text = " ".repeat(thumb_y as usize);
    let thumb_text = " ".repeat(thumb_h as usize);

    element!(
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Length(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: top_text)
            Text(text: thumb_text, style: Style::default().bg(dim))
            Text(text: " ")
        }
    )
}
