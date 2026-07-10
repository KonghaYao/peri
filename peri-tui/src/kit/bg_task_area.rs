//! 后台任务显示区域组件。
//!
//! 位于 AppShell 根层 StatusBar 下方，展示活跃的 bg subagent / bg shell / workflow 任务。
//! 每行格式：`◎ agent_type desc current_tool · N tools`。
//! 最大 5 行，超出显示 `… N more`。纯展示，不响应键盘/鼠标。

use crate::kit::atoms::{self, BgDisplayEntry};
use crate::kit::view_render;
use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Color, Style},
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use std::time::Instant;

/// 后台显示区域最大可见行数
const MAX_VISIBLE_ROWS: usize = 5;

/// 完成后保留时长（秒）
const DONE_KEEP_SECS: u64 = 3;

/// 状态符号
mod status_symbol {
    pub const IDLE: &str = "\u{25CE}"; // ◎
    pub const RUNNING: &str = "\u{25CF}"; // ●
    pub const DONE: &str = "\u{2714}"; // ✔
    pub const ERROR: &str = "\u{2717}"; // ✗
}

#[component]
pub fn BgTaskArea(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let display = hooks.use_atom(&atoms::BG_DISPLAY);
    // 订阅渲染心跳，确保闪烁动画持续更新
    let _heartbeat = hooks.use_atom(&atoms::RENDER_HEARTBEAT);

    let entries = display.read();
    let now = Instant::now();

    // 过滤过期条目（is_active=false 且 elapsed > 3s）
    let active: Vec<&BgDisplayEntry> = entries
        .iter()
        .filter(|e| {
            e.is_active
                || e.completed_at
                    .map_or(true, |t| now.duration_since(t).as_secs() < DONE_KEEP_SECS)
        })
        .collect();

    if active.is_empty() {
        // 无条目 → 高度 0，不渲染
        return element! {
            View(
                flex_direction: Direction::Vertical,
                width: Constraint::Fill(1),
                height: Constraint::Length(0),
            ) {}
        };
    }

    // 排序：活跃在前，完成/失败在后
    let mut sorted: Vec<&&BgDisplayEntry> = active.iter().collect();
    sorted.sort_by_key(|e| (!e.is_active, e.completed_at));

    let visible_count = sorted.len().min(MAX_VISIBLE_ROWS);
    let overflow_count = sorted.len().saturating_sub(MAX_VISIBLE_ROWS);

    // 渲染计数器用于运行中条目闪烁
    let render_count =
        view_render::RENDER_CALL_COUNT.with(|c| c.load(std::sync::atomic::Ordering::Relaxed));

    // 构建可见行
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(visible_count + 1);

    for entry in sorted.iter().take(MAX_VISIBLE_ROWS) {
        let line = render_entry_line(entry, render_count);
        lines.push(line);
    }

    // 溢出行
    if overflow_count > 0 {
        lines.push(Line::from(Span::styled(
            format!("… {} more", overflow_count),
            Style::default()
                .fg(Color::Gray)
                .add_modifier(ratatui::style::Modifier::DIM),
        )));
    }

    let height = lines.len() as u16;

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

/// 渲染单行：`◎ agent_type  desc  current_tool · N tools`
fn render_entry_line(entry: &BgDisplayEntry, render_count: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(5);

    // 1. 状态符号
    let (symbol, color, blink) = entry_state(entry, render_count);
    let mut symbol_style = Style::default().fg(color);
    if blink {
        symbol_style = symbol_style.add_modifier(ratatui::style::Modifier::HIDDEN);
    }
    spans.push(Span::styled(symbol.to_string(), symbol_style));
    spans.push(Span::raw(" "));

    // 2. agent_type（dim 色）
    spans.push(Span::styled(
        entry.agent_type.clone(),
        Style::default()
            .fg(Color::Gray)
            .add_modifier(ratatui::style::Modifier::DIM),
    ));
    spans.push(Span::raw("  "));

    // 3. desc（尾部截断由终端处理）
    spans.push(Span::raw(entry.desc.clone()));

    // 4. tool_call（仅当有 current_tool 时显示）
    if let Some(ref tool) = entry.current_tool {
        spans.push(Span::raw("  "));
        let tool_text = if entry.tool_count > 0 {
            format!("{} · {} tools", tool, entry.tool_count)
        } else {
            tool.clone()
        };
        spans.push(Span::styled(tool_text, Style::default().fg(Color::White)));
    } else if entry.tool_count > 0 && entry.current_tool.is_none() && !entry.is_active {
        // 已完成且无当前工具 → 显示工具计数
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("· {} tools", entry.tool_count),
            Style::default().fg(Color::Green),
        ));
    }

    Line::from(spans)
}

/// 判定条目的状态符号、颜色、是否闪烁
fn entry_state(entry: &BgDisplayEntry, render_count: usize) -> (&'static str, Color, bool) {
    if entry.is_error {
        return (status_symbol::ERROR, Color::Red, false);
    }
    if !entry.is_active {
        return (status_symbol::DONE, Color::Green, false);
    }
    if entry.current_tool.is_some() {
        let blink = (render_count / 16) % 2 == 0;
        return (status_symbol::RUNNING, Color::White, blink);
    }
    (status_symbol::IDLE, Color::Yellow, false)
}
