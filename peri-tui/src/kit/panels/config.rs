//! ratatui-kit ConfigPanel component.
//!
//! H1a（Iteration 14）：从 PERI_CONFIG_HANDLE 读取真实 PeriConfig，操作时
//! write + 调用 config::save 持久化到 ~/.peri/settings.json。permission_mode
//! 通过 PERMISSION_MODE_HANDLE 写运行时 SharedPermissionMode（非持久化——
//! 设计如此，每次启动默认从 YOLO_MODE 环境变量派生）。

#![allow(dead_code)]

use crate::kit::atoms::{PERI_CONFIG_HANDLE, PERMISSION_MODE_HANDLE};
use crate::kit::theme;
use peri_middlewares::prelude::PermissionMode;
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

// ---------------------------------------------------------------------------
// Row type
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum RowType {
    Toggle,
    Cycle(&'static [&'static str]),
}

// ---------------------------------------------------------------------------
// 配置行——每行绑定 PeriConfig 或 SharedPermissionMode 的真实字段
// ---------------------------------------------------------------------------

const ROW_SHOW_DIFF: usize = 0;
const ROW_CACHE_WARN: usize = 1;
const ROW_STREAMING: usize = 2;
const ROW_1M_CONTEXT: usize = 3;
const ROW_LANGUAGE: usize = 4;
const ROW_ACTIVE_ALIAS: usize = 5;
const ROW_PERMISSION_MODE: usize = 6;

const STREAMING_OPTS: &[&str] = &["streaming", "block", "none"];
const LANGUAGE_OPTS: &[&str] = &["en", "zh"];
const ALIAS_OPTS: &[&str] = &["opus", "sonnet", "haiku"];
const PERMISSION_OPTS: &[&str] = &["default", "accept-edit", "auto-mode", "bypass"];

const CONFIG_ROWS: &[(&str, RowType)] = &[
    ("Show Diff", RowType::Toggle),
    ("Cache Warning", RowType::Toggle),
    ("Streaming Mode", RowType::Cycle(STREAMING_OPTS)),
    ("1M Context", RowType::Toggle),
    ("Language", RowType::Cycle(LANGUAGE_OPTS)),
    ("Active Alias", RowType::Cycle(ALIAS_OPTS)),
    ("Permission Mode", RowType::Cycle(PERMISSION_OPTS)),
];

#[component]
pub fn ConfigPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);
    // bump：每次操作后递增，强制重渲染（PERI_CONFIG_HANDLE 是 RwLock 非 atom，
    // 写入不会自动触发 ratatui-kit 重渲染，需要手动 bump）
    let bump = hooks.use_state(|| 0u32);

    hooks.use_local_events({
        let cursor = cursor.clone();
        let bump = bump.clone();
        let row_count = CONFIG_ROWS.len();

        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                let sel = *cursor.read();

                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        close_panel();
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        *cursor.write() = sel.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        *cursor.write() = (sel + 1).min(row_count - 1);
                    }
                    KeyCode::Char(' ') | KeyCode::Enter => {
                        activate_row(sel, true);
                        *bump.write() += 1;
                    }
                    KeyCode::Left => {
                        activate_row(sel, false);
                        *bump.write() += 1;
                    }
                    KeyCode::Right => {
                        activate_row(sel, true);
                        *bump.write() += 1;
                    }
                    _ => {}
                }
            }
        }
    });

    // 读取 bump 强制 ratatui-kit 把这个值当作依赖（无此 read 调用则不会重渲染）
    let _ = *bump.read();

    // ---- Render ----
    let sel = *cursor.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        "  Configuration (persisted to ~/.peri/settings.json)",
        Style::new().fg(theme::MUTED).italic(),
    )]));
    lines.push(Line::from(""));

    for (i, (label, row_type)) in CONFIG_ROWS.iter().enumerate() {
        let is_active = i == sel;
        let cursor_mark = if is_active { "> " } else { "  " };
        let label_style = if is_active {
            Style::new().fg(theme::THINKING).bold()
        } else {
            Style::new().fg(theme::TEXT)
        };

        let value_line = match row_type {
            RowType::Toggle => {
                let val = read_toggle(i);
                let (on_text, off_text) = if val {
                    ("[ON]", " OFF")
                } else {
                    (" ON ", "[OFF]")
                };
                let on_style = if val {
                    Style::new().fg(theme::SAGE).bold()
                } else {
                    Style::new().fg(theme::MUTED)
                };
                let off_style = if val {
                    Style::new().fg(theme::MUTED)
                } else {
                    Style::new().fg(theme::ERROR).bold()
                };
                Line::from(vec![
                    Span::styled(cursor_mark, Style::new().fg(theme::THINKING)),
                    Span::styled(format!("{:<22}", label), label_style),
                    Span::styled(on_text, on_style),
                    Span::styled(" ", Style::new()),
                    Span::styled(off_text, off_style),
                ])
            }
            RowType::Cycle(options) => {
                let idx = read_cycle_idx(i, options);
                let mut spans = vec![
                    Span::styled(cursor_mark, Style::new().fg(theme::THINKING)),
                    Span::styled(format!("{:<22}", label), label_style),
                ];
                for (j, opt) in options.iter().enumerate() {
                    if j == idx {
                        spans.push(Span::styled(
                            format!("[{}]", opt),
                            Style::new().fg(theme::SAGE).bold(),
                        ));
                    } else {
                        spans.push(Span::styled(
                            format!(" {}", opt),
                            Style::new().fg(theme::MUTED),
                        ));
                    }
                    if j < options.len() - 1 {
                        spans.push(Span::styled(" ", Style::new()));
                    }
                }
                Line::from(spans)
            }
        };
        lines.push(value_line);
    }

    // Footer hints
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  j/k Nav  ", Style::new().fg(theme::DIM)),
        Span::styled("Space/Enter Toggle", Style::new().fg(theme::ACCENT)),
        Span::styled("  ←→ Cycle  q Close", Style::new().fg(theme::DIM)),
    ]));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Config ")
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

// ---------------------------------------------------------------------------
// 真实读写：通过 PERI_CONFIG_HANDLE / PERMISSION_MODE_HANDLE 操作
// ---------------------------------------------------------------------------

/// 读取 toggle 字段当前值（true=ON / false=OFF）。
fn read_toggle(row: usize) -> bool {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return false;
    };
    let cfg = handle.read();
    match row {
        ROW_SHOW_DIFF => cfg.config.diff_enabled,
        ROW_CACHE_WARN => cfg.config.show_cache_warning,
        ROW_1M_CONTEXT => cfg.config.context_1m.unwrap_or(false),
        _ => false,
    }
}

/// 读取 cycle 字段当前选项索引。
fn read_cycle_idx(row: usize, options: &[&str]) -> usize {
    match row {
        ROW_STREAMING => {
            let cur = PERI_CONFIG_HANDLE
                .get()
                .map(|h| h.read().config.streaming_mode.clone())
                .unwrap_or_default()
                .unwrap_or_else(|| "streaming".to_string());
            options.iter().position(|o| *o == cur.as_str()).unwrap_or(0)
        }
        ROW_LANGUAGE => {
            let cur = PERI_CONFIG_HANDLE
                .get()
                .map(|h| h.read().config.language.clone())
                .unwrap_or_default()
                .unwrap_or_else(|| "en".to_string());
            options.iter().position(|o| *o == cur.as_str()).unwrap_or(0)
        }
        ROW_ACTIVE_ALIAS => {
            let cur = PERI_CONFIG_HANDLE
                .get()
                .map(|h| h.read().config.active_alias.clone())
                .unwrap_or_default();
            if cur.is_empty() {
                1 // default sonnet
            } else {
                options.iter().position(|o| *o == cur.as_str()).unwrap_or(1)
            }
        }
        ROW_PERMISSION_MODE => {
            let cur = PERMISSION_MODE_HANDLE
                .get()
                .map(|m| permission_mode_label(m.load()))
                .unwrap_or("default");
            options.iter().position(|o| *o == cur).unwrap_or(0)
        }
        _ => 0,
    }
}

/// 激活某行：toggle 反转，cycle forward=true 前进 / forward=false 后退。
fn activate_row(row: usize, forward: bool) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let row_type = &CONFIG_ROWS[row].1;
    match row_type {
        RowType::Toggle => {
            let mut cfg = handle.write();
            match row {
                ROW_SHOW_DIFF => cfg.config.diff_enabled = !cfg.config.diff_enabled,
                ROW_CACHE_WARN => cfg.config.show_cache_warning = !cfg.config.show_cache_warning,
                ROW_1M_CONTEXT => {
                    let cur = cfg.config.context_1m.unwrap_or(false);
                    cfg.config.context_1m = Some(!cur);
                }
                _ => {}
            }
            // drop guard before save（save 借 &cfg）
            let cfg_snapshot = cfg.clone();
            drop(cfg);
            let _ = crate::config::save(&cfg_snapshot);
        }
        RowType::Cycle(options) => {
            let cur_idx = read_cycle_idx(row, options);
            let next = if forward {
                (cur_idx + 1) % options.len()
            } else {
                (cur_idx + options.len() - 1) % options.len()
            };
            let new_val = options[next];
            match row {
                ROW_STREAMING => {
                    let mut cfg = handle.write();
                    cfg.config.streaming_mode = Some(new_val.to_string());
                    let snap = cfg.clone();
                    drop(cfg);
                    let _ = crate::config::save(&snap);
                }
                ROW_LANGUAGE => {
                    let mut cfg = handle.write();
                    cfg.config.language = Some(new_val.to_string());
                    let snap = cfg.clone();
                    drop(cfg);
                    let _ = crate::config::save(&snap);
                }
                ROW_ACTIVE_ALIAS => {
                    let mut cfg = handle.write();
                    cfg.config.active_alias = new_val.to_string();
                    let snap = cfg.clone();
                    drop(cfg);
                    let _ = crate::config::save(&snap);
                }
                ROW_PERMISSION_MODE => {
                    if let Some(mode_handle) = PERMISSION_MODE_HANDLE.get() {
                        if let Some(mode) = parse_permission_mode(new_val) {
                            mode_handle.store(mode);
                        }
                    }
                    // permission_mode 不持久化到 settings.json（运行时状态）
                }
                _ => {}
            }
        }
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

fn permission_mode_label(m: PermissionMode) -> &'static str {
    match m {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdit => "accept-edit",
        PermissionMode::AutoMode => "auto-mode",
        PermissionMode::Bypass => "bypass",
    }
}

fn parse_permission_mode(s: &str) -> Option<PermissionMode> {
    match s {
        "default" => Some(PermissionMode::Default),
        "accept-edit" => Some(PermissionMode::AcceptEdit),
        "auto-mode" => Some(PermissionMode::AutoMode),
        "bypass" => Some(PermissionMode::Bypass),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_mode_label_roundtrip() {
        for &label in PERMISSION_OPTS {
            let mode = parse_permission_mode(label).unwrap();
            assert_eq!(permission_mode_label(mode), label);
        }
    }

    #[test]
    fn test_parse_permission_mode_unknown() {
        assert!(parse_permission_mode("unknown").is_none());
        assert!(parse_permission_mode("").is_none());
    }
}
