//! ratatui-kit MemoryPanel component.
//!
//! H1h（Iteration 14）：从 MEMORY_LIST atom 读取真实 memory 文件列表（由
//! service_snapshot 扫描 `~/.claude/memory/*.md` 派生，2s 刷新）。
//!
//! Enter 调用 `$EDITOR`（fallback `vi`）打开文件——通过 spawn_blocking + Detach
//! 执行，避免阻塞渲染线程。

use crate::kit::atoms::{MEMORY_LIST, MemoryEntry};
use crate::kit::theme;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

#[component]
pub fn MemoryPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);
    let store = hooks.use_store(*MEMORY_LIST.get().unwrap());
    let entries: Vec<MemoryEntry> = store.read().clone();
    let _ = store;
    let count = entries.len();

    hooks.use_local_events({
        let selected = selected.clone();
        let entries = entries.clone();
        let count = count;
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => close_panel(),
                    KeyCode::Up | KeyCode::Char('k') => {
                        *selected.write() = selected.read().saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let mut s = selected.write();
                        if count > 0 {
                            *s = (*s + 1).min(count - 1);
                        }
                    }
                    KeyCode::Enter => {
                        let sel = *selected.read();
                        if let Some(entry) = entries.get(sel) {
                            open_memory_in_editor(&entry.path);
                        }
                    }
                    _ => {}
                }
            }
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 头部摘要
    lines.push(Line::from(vec![Span::styled(
        format!("  {} memory files in ~/.claude/memory", count),
        Style::new().fg(theme::TEXT).bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  Enter) Edit in $EDITOR  Esc) Close",
        Style::new().fg(theme::MUTED).italic(),
    )]));
    lines.push(Line::from(""));

    if entries.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No memory files found",
            Style::new().fg(theme::MUTED),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Create ~/.claude/memory/<name>.md to persist cross-session notes",
            Style::new().fg(theme::MUTED),
        )]));
    } else {
        for (i, entry) in entries.iter().enumerate() {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };

            // size 人类可读
            let size_str = format_size(entry.size_bytes);
            // 相对时间
            let time_str = entry
                .modified
                .map(format_relative_time)
                .unwrap_or_else(|| "—".to_string());

            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", cursor), Style::new().fg(theme::THINKING)),
                Span::styled(entry.path.clone(), name_style),
                Span::styled(
                    format!("   {}  {}", size_str, time_str),
                    Style::new().fg(theme::DIM),
                ),
            ]));
        }
    }

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Memory ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(80),
            height: Constraint::Length(20),
        ) {
            ScrollView(
                scroll_bars: ScrollBars::default(),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
        }
    )
}

/// 人类可读的字节大小。
fn format_size(bytes: u64) -> String {
    const UNITS: &[(&str, u64)] = &[
        ("B", 1),
        ("KB", 1024),
        ("MB", 1024 * 1024),
        ("GB", 1024 * 1024 * 1024),
    ];
    for (unit, threshold) in UNITS.iter().rev() {
        if bytes >= *threshold {
            let v = bytes as f64 / *threshold as f64;
            if v >= 10.0 {
                return format!("{:.0} {}", v, unit);
            } else {
                return format!("{:.1} {}", v, unit);
            }
        }
    }
    format!("{} B", bytes)
}

/// 相对时间（"3m ago" / "2h ago" / "5d ago" / "2026-06-01"）。
fn format_relative_time(ts: chrono::DateTime<chrono::Utc>) -> String {
    use chrono::Utc;
    let now = Utc::now();
    let delta = now.signed_duration_since(ts);
    let secs = delta.num_seconds();
    if secs < 60 {
        return "just now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{}m ago", mins);
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{}h ago", hours);
    }
    let days = hours / 24;
    if days < 30 {
        return format!("{}d ago", days);
    }
    // 超过 30 天显示日期
    ts.format("%Y-%m-%d").to_string()
}

/// 用 `$EDITOR`（fallback `vi`）打开 memory 文件，detach 不阻塞 TUI。
fn open_memory_in_editor(rel_path: &str) {
    use std::path::PathBuf;
    use std::process::Command;

    let Some(home) = dirs_next::home_dir() else {
        tracing::warn!("open_memory_in_editor: home_dir unknown");
        return;
    };
    let full: PathBuf = home.join(".claude").join("memory").join(rel_path);
    if !full.exists() {
        tracing::warn!(?full, "open_memory_in_editor: file missing");
        return;
    }

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    // 拆分 EDITOR 可能包含的参数（如 "code -w" / "nvim -f"）
    let mut parts = editor.split_whitespace();
    let Some(bin) = parts.next() else {
        tracing::warn!("open_memory_in_editor: empty EDITOR");
        return;
    };
    let mut cmd = Command::new(bin);
    for arg in parts {
        cmd.arg(arg);
    }
    cmd.arg(&full);

    tracing::info!(?full, bin, "MemoryPanel: spawning editor");
    if let Err(e) = cmd.spawn() {
        tracing::warn!(?e, bin, "MemoryPanel: editor spawn failed");
    }
}

fn close_panel() {
    use crate::kit::atoms::{ACTIVE_PANEL, OPEN_PANELS};
    if let Some(atom) = ACTIVE_PANEL.get() {
        *atom.write() = None;
    }
    if let Some(atom) = OPEN_PANELS.get() {
        atom.write().clear();
    }
}
