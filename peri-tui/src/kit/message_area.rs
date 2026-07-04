//! MessageArea：消息流渲染区——render_bridge 预计算 lines + Paragraph.scroll 视口裁剪。
//!
//! 渲染流程与原版完全一致：拼接全部 VM lines → Paragraph → scroll 视口。
//! 唯一区别：lines 来自 RENDER_CACHE atom（预计算），而非每帧调 render_v2_vm 重解析 markdown。
//!
//! - 滚动：Ctrl+Up/Down/Home/End + 鼠标滚轮
//! - 智能跟随：新 turn 自动滚底

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

#[derive(Default, Props)]
pub struct MessageAreaProps {
    pub width: usize,
}

#[component]
pub fn MessageArea(props: &MessageAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let semantic = theme::semantic();

    // ── 从 RENDER_CACHE atom 读取预计算的 lines（替代原来的 render_v2_vm 遍历）──
    let render_cache = hooks.use_atom(&RENDER_CACHE);
    let acp = hooks.use_atom(&ACP_STATE);
    let cache_snapshot = render_cache.read();
    let is_loading = acp.read().is_loading;

    // ── 按原版方式拼接全部 lines ──
    let mut all_lines: Vec<Line<'static>> = Vec::new();
    let mut current_has_ct = false;
    for (key, entry) in cache_snapshot.entries.iter() {
        if matches!(key, crate::kit::render_bridge::VmKey::CurrentTurn(_)) {
            current_has_ct = true;
        }
        for line in entry.lines.iter() {
            all_lines.push(line.clone());
        }
        all_lines.push(Line::from(""));
    }

    let empty = all_lines.is_empty();

    if is_loading {
        all_lines.push(Line::from(vec![Span::styled(
            "◜ 思考中…",
            Style::default().fg(semantic.status.running),
        )]));
    }

    // ── 高度（原版 visual_height，现在从 cumulative_heights 读）──
    let content_h = cache_snapshot
        .cumulative_heights
        .last()
        .copied()
        .unwrap_or(0)
        .saturating_add(if is_loading { 1 } else { 0 });
    drop(cache_snapshot); // 尽早释放读锁

    // ── 滚动（与原版完全一致）──
    let scroll_offset = hooks.use_state(|| 0u16);
    let mut auto_scroll = hooks.use_state(|| true);
    let (_, term_h) = hooks.use_terminal_size();
    let vp_h: u16 = term_h.saturating_sub(4).max(1);
    let max_scroll: u16 = content_h
        .saturating_sub(vp_h as usize)
        .min(u16::MAX as usize) as u16;

    // 新 turn → 恢复自动滚动。use_effect 在渲染后运行，闭包中 h/a 是上一次的值。
    // deps 用 current_has_ct 精确控制：仅值变化时触发，避免无限重渲染。
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

    // 自动滚底 + clamp（仅值变化时写入，避免同值触发重渲染循环）
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

    // ── 事件（与原版完全一致）──
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

    // ── 渲染（与原版完全一致）──
    if empty {
        element!(
            View(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                Welcome(width: props.width)
            }
        )
        .into_any()
    } else {
        let text_content = Paragraph::new(all_lines).wrap(Wrap { trim: false });
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
