//! ratatui-kit MemoryPanel component.
//!
//! H1h（Iteration 14）：从 MEMORY_LIST atom 读取真实 memory 文件列表（由
//! service_snapshot 扫描 `~/.claude/memory/*.md` 派生，2s 刷新）。
//!
//! Enter 调用 `$EDITOR`（fallback `vi`）打开文件——通过 spawn_blocking + Detach
//! 执行，避免阻塞渲染线程。

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{MEMORY_LIST, MemoryEntry};
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::theme;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

#[component]
pub fn MemoryPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let selected = hooks.use_state(|| 0usize);
    let store = hooks.use_atom(&MEMORY_LIST);
    let entries: Vec<MemoryEntry> = store.read().clone();
    let _ = store;
    let count = entries.len();

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        move |event| {
            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }
            match key.code {
                KeyCode::Esc => close_panel(),
                KeyCode::Up => {
                    *selected.write() = previous_selection(*selected.read());
                }
                KeyCode::Down => {
                    let mut s = selected.write();
                    let count = MEMORY_LIST.state().read().len();
                    if count > 0 {
                        *s = next_selection(*s, count);
                    }
                }
                KeyCode::Enter => {
                    let sel = *selected.read();
                    let entries = MEMORY_LIST.state().read().clone();
                    if let Some(entry) = entries.get(sel) {
                        open_memory_in_editor(&entry.path);
                    }
                }
                _ => {}
            }
            EventResult::Consumed
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 头部摘要
    lines.push(Line::from(vec![Span::styled(
        format!("  {} memory files in ~/.claude/memory", count),
        Style::new().fg(theme::semantic().text.primary).bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  Enter) Edit in $EDITOR  Esc) Close",
        Style::new().fg(theme::semantic().text.muted).italic(),
    )]));
    lines.push(Line::from(""));

    if entries.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No memory files found",
            Style::new().fg(theme::semantic().text.muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Create ~/.claude/memory/<name>.md to persist cross-session notes",
            Style::new().fg(theme::semantic().text.muted),
        )]));
    } else {
        for (i, entry) in entries.iter().enumerate() {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new().fg(theme::component().panel.title).bold()
            } else {
                Style::new().fg(theme::semantic().text.primary)
            };

            // size 人类可读
            let size_str = format_size(entry.size_bytes);
            // 相对时间
            let time_str = entry
                .modified
                .map(format_relative_time)
                .unwrap_or_else(|| "—".to_string());

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor),
                    Style::new().fg(theme::component().panel.title),
                ),
                Span::styled(entry.path.clone(), name_style),
                Span::styled(
                    format!("   {}  {}", size_str, time_str),
                    Style::new().fg(theme::semantic().text.dim),
                ),
            ]));
        }
    }

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Memory, {
            ScrollView(
                scroll_bars: crate::kit::panel_registry::clean_scrollbars(),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
    })
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
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size_bytes_below_kb() {
        // 0 走 fallthrough 路径（UNITS 全部 threshold 都不满足 0 >= t）
        assert_eq!(format_size(0), "0 B");
        // 1~9 走 B 分支：v = bytes/1.0 < 10 → "X.0 B"
        assert_eq!(format_size(1), "1.0 B");
        assert_eq!(format_size(9), "9.0 B");
        // 10~1023 走 B 分支：v >= 10 → "XX B"
        assert_eq!(format_size(10), "10 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn test_format_size_kb_threshold() {
        // 1024 = 1.0 KB（< 10，保留 1 位小数）
        assert_eq!(format_size(1024), "1.0 KB");
        // 1536 = 1.5 KB
        assert_eq!(format_size(1536), "1.5 KB");
        // 10240 = 10 KB（≥10，无小数）
        assert_eq!(format_size(10240), "10 KB");
        // 51200 = 50 KB
        assert_eq!(format_size(51200), "50 KB");
    }

    #[test]
    fn test_format_size_mb_and_gb() {
        // 1 MB = 1048576
        assert_eq!(format_size(1048576), "1.0 MB");
        // 5 MB
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
        // 1 GB
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn test_format_size_u64_max_no_overflow() {
        // u64::MAX ~ 18.45 EB，但本函数最高只到 GB 单位——不应 panic 或 overflow
        let s = format_size(u64::MAX);
        assert!(
            s.ends_with(" GB"),
            "expected GB suffix for u64::MAX, got: {}",
            s
        );
    }

    #[test]
    fn test_format_relative_time_just_now() {
        use chrono::Utc;
        let now = Utc::now();
        // 30s 前 → "just now"
        assert_eq!(format_relative_time(now), "just now");
        // 59s 前 → "just now"
        let almost_minute_ago = now - chrono::Duration::seconds(59);
        assert_eq!(format_relative_time(almost_minute_ago), "just now");
    }

    #[test]
    fn test_format_relative_time_minutes() {
        use chrono::Utc;
        let now = Utc::now();
        let five_min_ago = now - chrono::Duration::minutes(5);
        assert_eq!(format_relative_time(five_min_ago), "5m ago");
        let fifty_nine_min_ago = now - chrono::Duration::minutes(59);
        assert_eq!(format_relative_time(fifty_nine_min_ago), "59m ago");
    }

    #[test]
    fn test_format_relative_time_hours() {
        use chrono::Utc;
        let now = Utc::now();
        let two_hours_ago = now - chrono::Duration::hours(2);
        assert_eq!(format_relative_time(two_hours_ago), "2h ago");
        let twenty_three_hours_ago = now - chrono::Duration::hours(23);
        assert_eq!(format_relative_time(twenty_three_hours_ago), "23h ago");
    }

    #[test]
    fn test_format_relative_time_days() {
        use chrono::Utc;
        let now = Utc::now();
        let five_days_ago = now - chrono::Duration::days(5);
        assert_eq!(format_relative_time(five_days_ago), "5d ago");
        let twenty_nine_days_ago = now - chrono::Duration::days(29);
        assert_eq!(format_relative_time(twenty_nine_days_ago), "29d ago");
    }

    #[test]
    fn test_format_relative_time_over_30_days_falls_to_date() {
        use chrono::Utc;
        let now = Utc::now();
        let forty_days_ago = now - chrono::Duration::days(40);
        // 超过 30 天 → 显示 YYYY-MM-DD
        let result = format_relative_time(forty_days_ago);
        assert!(
            result.len() == 10 && result.chars().nth(4) == Some('-'),
            "expected date format YYYY-MM-DD, got: {}",
            result
        );
    }
}
