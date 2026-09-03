//! 后台任务显示区域组件。
//!
//! 位于 AppShell 根层 StatusBar 下方，每行展示一个活跃的 bg subagent / bg shell / workflow 任务。
//! 格式：`● coder  修改文档  2m15s`，空态高度 0。

use crate::kit::atoms::{self, BgDisplayEntry};
use crate::kit::bg_task_click::{
    BgTaskLineHit, apply_bg_task_click_route, build_bg_task_line_hits,
    route_bg_task_click_at_index, sort_bg_display_rows, visible_bg_display_entries,
};
use crate::kit::mouse_router;
use crate::kit::panel_mouse::AreaTracker;
use ratatui_kit::{
    crossterm::event::{Event, MouseButton, MouseEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Color, Style},
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// 状态符号
mod status_symbol {
    pub const IDLE: &str = "\u{25CE}"; // ◎
    pub const RUNNING: &str = "\u{25CF}"; // ●
    pub const DONE: &str = "\u{2714}"; // ✔
    pub const ERROR: &str = "\u{2717}"; // ✗
}

/// `Instant::duration_since()` 的安全包装。
/// 当 `earlier > later` 时返回 Duration::ZERO，避免 panic。
fn safe_elapsed(later: Instant, earlier: Instant) -> Duration {
    if later >= earlier {
        later.duration_since(earlier)
    } else {
        Duration::ZERO
    }
}

#[component]
pub fn BgTaskArea(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let display = hooks.use_atom(&atoms::BG_DISPLAY);
    let _heartbeat = hooks.use_atom(&atoms::RENDER_HEARTBEAT);
    let (term_w, _) = hooks.use_terminal_size();

    let area_tracker = hooks.use_hook(AreaTracker::new);
    let area_rect = area_tracker.rect;
    let line_hits = hooks.use_state(Arc::<Vec<BgTaskLineHit>>::default);

    hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
        let Event::Mouse(mouse) = event else {
            return EventResult::Ignored;
        };
        if mouse.kind != MouseEventKind::Up(MouseButton::Left) {
            return EventResult::Ignored;
        }
        let Some(area) = area_rect else {
            return EventResult::Ignored;
        };
        let hits = line_hits.read().clone();
        let Some(hit) =
            crate::kit::bg_task_click::hit_test_bg_task_line(&hits, area, mouse.column, mouse.row)
        else {
            return EventResult::Ignored;
        };
        if !mouse_router::bg_bar_click_allowed(true) {
            return EventResult::Ignored;
        }
        let entries = atoms::BG_DISPLAY.state().read().clone();
        let now = Instant::now();
        let active = visible_bg_display_entries(&entries, now);
        let sorted = sort_bg_display_rows(active);
        let Some(route) = route_bg_task_click_at_index(&sorted, hit.sorted_index) else {
            return EventResult::Ignored;
        };
        apply_bg_task_click_route(route);
        EventResult::Consumed
    });

    let entries = display.read();
    let now = Instant::now();

    let active = visible_bg_display_entries(&entries, now);
    let sorted = sort_bg_display_rows(active);

    let max_width = (term_w as usize).saturating_sub(2);

    let lines: Vec<Line<'static>> = sorted
        .iter()
        .map(|entry| render_entry_line(entry, now, max_width))
        .collect();

    let height = lines.len() as u16;

    if let Some(area) = area_rect {
        let hits = build_bg_task_line_hits(area, &sorted);
        *line_hits.write_no_update() = Arc::new(hits);
    } else {
        *line_hits.write_no_update() = Arc::new(Vec::new());
    }

    element! {
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(height),
        ) {
            Text(text: Paragraph::new(lines))
        }
    }
}

/// 渲染单行：`● coder  修改文档                         2m15s`
fn render_entry_line(entry: &BgDisplayEntry, now: Instant, max_width: usize) -> Line<'static> {
    // 1. 耗时
    let time_str = elapsed_str(entry, now);
    let time_wide = UnicodeWidthStr::width(time_str.as_str());

    // 2. 前缀宽度：符号(1) + 空格(1) + agent_type + 空格(2)
    let agent_wide = UnicodeWidthStr::width(entry.agent_type.as_str());
    let prefix_width = 1 + 1 + agent_wide + 2;

    // 3. desc 可用宽度 = 终端宽 - 前缀 - 时间（至少留 1 列间距）
    let desc_available = max_width
        .saturating_sub(prefix_width)
        .saturating_sub(time_wide)
        .saturating_sub(1);
    let desc_text = truncate_desc(&entry.desc, desc_available);

    // 4. 左侧实际宽度
    let desc_wide = UnicodeWidthStr::width(desc_text.as_str());
    let left_wide = prefix_width + desc_wide;

    // 5. 填充空格推到右侧
    let pad_len = max_width
        .saturating_sub(left_wide)
        .saturating_sub(time_wide);

    let (symbol, color) = entry_symbol_color(entry);

    let mut spans = vec![
        Span::styled(symbol.to_string(), Style::default().fg(color)),
        Span::raw(" "),
        Span::styled(
            entry.agent_type.to_string(),
            Style::default()
                .fg(Color::Gray)
                .add_modifier(ratatui::style::Modifier::DIM),
        ),
        Span::raw("  "),
        Span::raw(desc_text),
    ];

    if pad_len > 0 {
        spans.push(Span::raw(" ".repeat(pad_len)));
    }

    spans.push(Span::styled(
        time_str,
        Style::default()
            .fg(Color::Gray)
            .add_modifier(ratatui::style::Modifier::DIM),
    ));

    Line::from(spans)
}

/// 计算条目的耗时字符串（安全版本）
fn elapsed_str(entry: &BgDisplayEntry, now: Instant) -> String {
    let elapsed = if entry.is_active {
        safe_elapsed(now, entry.created_at)
    } else {
        // 已完成：显示总运行时长
        entry
            .completed_at
            .and_then(|t| {
                if t >= entry.created_at {
                    Some(t.duration_since(entry.created_at))
                } else {
                    None
                }
            })
            .unwrap_or_default()
    };
    format_elapsed(elapsed)
}

/// CJK 安全的 desc 截断：超宽时尾部加 "…"
fn truncate_desc(desc: &str, max_wide: usize) -> String {
    if max_wide == 0 {
        return String::new();
    }
    let full_wide = UnicodeWidthStr::width(desc);
    if full_wide <= max_wide {
        return desc.to_string();
    }

    // 预留 "…" 的宽度（2 列）
    let ellipsis = "\u{2026}"; // …
    let body_max = max_wide.saturating_sub(2);

    let mut accumulated: usize = 0;
    let mut chars: Vec<char> = Vec::new();
    for ch in desc.chars() {
        let ch_wide = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if accumulated + ch_wide > body_max {
            break;
        }
        chars.push(ch);
        accumulated += ch_wide;
    }
    let mut s: String = chars.into_iter().collect();
    s.push_str(ellipsis);
    s
}

/// 格式化为紧凑形式：`Xs` / `XmXs` / `XhXm`
fn format_elapsed(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// 判定条目的状态符号和颜色
fn entry_symbol_color(entry: &BgDisplayEntry) -> (&'static str, Color) {
    if entry.is_error {
        (status_symbol::ERROR, Color::Red)
    } else if !entry.is_active {
        (status_symbol::DONE, Color::Green)
    } else if entry.current_tool.is_some() {
        (status_symbol::RUNNING, Color::White)
    } else {
        (status_symbol::IDLE, Color::Yellow)
    }
}
