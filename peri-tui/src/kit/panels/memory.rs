//! ratatui-kit MemoryPanel component.
//!
//! H1h（Iteration 14）：从 MEMORY_LIST atom 读取真实 memory 文件列表（由
//! service_snapshot 扫描 `~/.claude/memory/*.md` 派生，2s 刷新）。
//!
//! Enter 调用 `$EDITOR`（fallback `vi`）打开文件——通过 spawn_blocking + Detach
//! 执行，避免阻塞渲染线程。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{LANG_VERSION, MEMORY_LIST, MemoryEntry};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::{AreaTracker, ListLayout, hit_item, is_scrollbar_column};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind},
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
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);
    let store = hooks.use_atom(&MEMORY_LIST);
    let entries: Vec<MemoryEntry> = store.read().clone();
    let _ = store;
    let _ = hooks.use_atom(&LANG_VERSION);
    let count = entries.len();

    // 面板绘制区域（上一帧）——鼠标点击行号反推
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // 视口跟随：让选中项始终可见（issue 2026-07-06-panels-selection-no-scroll-follow）。
    // panel 高度 18 - border 2 - header 3 = 13 行；每项 1 行 → 可见 13 个。
    const VISIBLE_ITEMS: usize = 13;
    let scroll_start = scroll_start_for_selected(*selected.read(), entries.len(), VISIBLE_ITEMS);

    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        {
            move |event| {
                // 鼠标：区域内左键点击 = 选中该项并执行 Enter 动作（click as enter）
                if let Event::Mouse(mouse) = event {
                    if let Some(area) = area
                        && !is_scrollbar_column(&mouse, area)
                        && let Some(idx) = hit_item(
                            &mouse,
                            area,
                            ListLayout {
                                header_rows: 3,
                                item_rows: 1,
                                footer_rows: 0,
                                visible_items: VISIBLE_ITEMS as u16,
                                scroll_start,
                                item_count: count,
                            },
                        )
                    {
                        *selected.write() = idx;
                        let entries = MEMORY_LIST.state().read().clone();
                        if let Some(entry) = entries.get(idx) {
                            open_memory_in_editor(&entry.path);
                        }
                        return EventResult::Consumed;
                    }
                    return match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => EventResult::Consumed,
                        _ => EventResult::Ignored,
                    };
                }
                let Event::Key(key) = event else {
                    return EventResult::Ignored;
                };
                if key.kind != KeyEventKind::Press {
                    return EventResult::Ignored;
                }
                match key.code {
                    KeyCode::Esc => close_panel(),
                    KeyCode::Up => {
                        let mut s = selected.write();
                        *s = previous_selection(*s);
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
        },
    );

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 头部摘要
    lines.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            "panel-memory-stats",
            &[("count".to_string(), FluentValue::from(count as i64))],
        ),
        Style::new()
            .fg(theme_def.read().semantic.text.primary)
            .bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        i18n::tr("panel-memory-nav-hint"),
        Style::new()
            .fg(theme_def.read().semantic.text.muted)
            .italic(),
    )]));
    lines.push(Line::from(""));

    if entries.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-memory-empty"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-memory-empty-hint"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
    } else {
        for (i, entry) in entries
            .iter()
            .enumerate()
            .skip(scroll_start)
            .take(VISIBLE_ITEMS)
        {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold()
            } else {
                Style::new().fg(theme_def.read().semantic.text.primary)
            };

            // size 人类可读
            let size_str = format_size(entry.size_bytes);
            // 相对时间
            let time_str = entry
                .modified
                .map(format_relative_time)
                .unwrap_or_else(|| i18n::tr("common-na"));

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor),
                    Style::new().fg(theme_def.read().component.panel.title),
                ),
                Span::styled(entry.path.clone(), name_style),
                Span::styled(
                    format!("   {}  {}", size_str, time_str),
                    Style::new().fg(theme_def.read().semantic.text.dim),
                ),
            ]));
        }
    }

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    // 面板滚轮仲裁注册（每帧覆盖写入，area 用上一帧组件区域）
    crate::kit::panel_scroll::register_panel_scroll(
        PanelKind::Memory,
        hooks.use_previous_size(),
        sv,
    );

    panel_shell!(PanelKind::Memory, {
            ScrollView(
                scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                state: Some(sv),
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
        ("panel-memory-unit-b", 1),
        ("panel-memory-unit-kb", 1024),
        ("panel-memory-unit-mb", 1024 * 1024),
        ("panel-memory-unit-gb", 1024 * 1024 * 1024),
    ];
    for (key, threshold) in UNITS.iter().rev() {
        if bytes >= *threshold {
            let v = bytes as f64 / *threshold as f64;
            let unit = i18n::tr(key);
            if v >= 10.0 {
                return format!("{:.0} {}", v, unit);
            } else {
                return format!("{:.1} {}", v, unit);
            }
        }
    }
    format!("{} {}", bytes, i18n::tr("panel-memory-unit-b"))
}

/// 相对时间（"3m ago" / "2h ago" / "5d ago" / "2026-06-01"）。
fn format_relative_time(ts: chrono::DateTime<chrono::Utc>) -> String {
    use chrono::Utc;
    let now = Utc::now();
    let delta = now.signed_duration_since(ts);
    let secs = delta.num_seconds();
    if secs < 60 {
        return i18n::tr("panel-memory-time-just-now");
    }
    let mins = secs / 60;
    if mins < 60 {
        return i18n::tr_args(
            "panel-memory-time-min-ago",
            &[("n".to_string(), FluentValue::from(mins))],
        );
    }
    let hours = mins / 60;
    if hours < 24 {
        return i18n::tr_args(
            "panel-memory-time-hour-ago",
            &[("n".to_string(), FluentValue::from(hours))],
        );
    }
    let days = hours / 24;
    if days < 30 {
        return i18n::tr_args(
            "panel-memory-time-day-ago",
            &[("n".to_string(), FluentValue::from(days))],
        );
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
#[path = "memory_test.rs"]
mod tests;
