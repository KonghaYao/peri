//! ratatui-kit ConfigPanel component.
//!
//! H1a（Iteration 14）：从 PERI_CONFIG_HANDLE 读取真实 PeriConfig，操作时
//! write + 调用 config::save 持久化到 ~/.peri/settings.json。permission_mode
//! 通过 PERMISSION_MODE_HANDLE 写运行时 SharedPermissionMode（非持久化——
//! 设计如此，每次启动默认从 YOLO_MODE 环境变量派生）。

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{PERI_CONFIG_HANDLE, PERMISSION_MODE_HANDLE};
use crate::kit::list_nav::{next_selection, previous_selection};
use crate::kit::theme;
use peri_middlewares::prelude::PermissionMode;
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

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        let row_count = CONFIG_ROWS.len();

        move |event| {
            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }
            let sel = *cursor.read();

            match key.code {
                KeyCode::Esc => {
                    close_panel();
                }
                KeyCode::Up => {
                    *cursor.write() = previous_selection(sel);
                }
                KeyCode::Down => {
                    *cursor.write() = next_selection(sel, row_count);
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
            EventResult::Consumed
        }
    });

    // 读取 bump 强制 ratatui-kit 把这个值当作依赖（无此 read 调用则不会重渲染）
    let _ = *bump.read();

    // ---- Render ----
    let sel = *cursor.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        "  Configuration (persisted to ~/.peri/settings.json)",
        Style::new().fg(theme::semantic().text.muted).italic(),
    )]));
    lines.push(Line::from(""));

    for (i, (label, row_type)) in CONFIG_ROWS.iter().enumerate() {
        let is_active = i == sel;
        let cursor_mark = if is_active { "> " } else { "  " };
        let label_style = if is_active {
            Style::new().fg(theme::component().panel.title).bold()
        } else {
            Style::new().fg(theme::semantic().text.primary)
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
                    Style::new().fg(theme::semantic().status.success).bold()
                } else {
                    Style::new().fg(theme::semantic().text.muted)
                };
                let off_style = if val {
                    Style::new().fg(theme::semantic().text.muted)
                } else {
                    Style::new().fg(theme::semantic().status.error).bold()
                };
                Line::from(vec![
                    Span::styled(cursor_mark, Style::new().fg(theme::component().panel.title)),
                    Span::styled(format!("{:<22}", label), label_style),
                    Span::styled(on_text, on_style),
                    Span::styled(" ", Style::new()),
                    Span::styled(off_text, off_style),
                ])
            }
            RowType::Cycle(options) => {
                let idx = read_cycle_idx(i, options);
                let mut spans = vec![
                    Span::styled(cursor_mark, Style::new().fg(theme::component().panel.title)),
                    Span::styled(format!("{:<22}", label), label_style),
                ];
                for (j, opt) in options.iter().enumerate() {
                    if j == idx {
                        spans.push(Span::styled(
                            format!("[{}]", opt),
                            Style::new().fg(theme::semantic().status.success).bold(),
                        ));
                    } else {
                        spans.push(Span::styled(
                            format!(" {}", opt),
                            Style::new().fg(theme::semantic().text.muted),
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
        Span::styled(
            "  ↑/↓::navigate  ",
            Style::new().fg(theme::semantic().text.dim),
        ),
        Span::styled(
            "Enter::toggle",
            Style::new().fg(theme::semantic().border.active),
        ),
        Span::styled(
            "  ←/→::switch  Esc::close",
            Style::new().fg(theme::semantic().text.dim),
        ),
    ]));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Config, {
        ScrollView(
            scroll_bars: crate::kit::panel_registry::clean_scrollbars(),
            width: Constraint::Fill(1),
            height: Constraint::Fill(1),
        ) {
            Text(text: content)
        }
    })
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
                    if let Some(mode_handle) = PERMISSION_MODE_HANDLE.get()
                        && let Some(mode) = parse_permission_mode(new_val)
                    {
                        mode_handle.store(mode);
                    }
                    // permission_mode 不持久化到 settings.json（运行时状态）
                }
                _ => {}
            }
        }
    }
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

fn permission_mode_label(m: PermissionMode) -> &'static str {
    match m {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdit => "accept-edit",
        PermissionMode::AutoMode => "auto-mode",
        PermissionMode::Bypass => "bypass",
    }
}

/// 纯函数：toggle 行的值反转。返回反转后的新值（无效 row 返回 None）。
/// 提取为独立函数便于单测——避免依赖全局 atom。
#[allow(dead_code)]
fn apply_toggle_row(cfg: &mut crate::config::PeriConfig, row: usize) -> Option<bool> {
    let new_val = match row {
        ROW_SHOW_DIFF => {
            cfg.config.diff_enabled = !cfg.config.diff_enabled;
            cfg.config.diff_enabled
        }
        ROW_CACHE_WARN => {
            cfg.config.show_cache_warning = !cfg.config.show_cache_warning;
            cfg.config.show_cache_warning
        }
        ROW_1M_CONTEXT => {
            let cur = cfg.config.context_1m.unwrap_or(false);
            cfg.config.context_1m = Some(!cur);
            !cur
        }
        _ => return None,
    };
    Some(new_val)
}

/// 纯函数：cycle 行前进/后退并写入新值。返回新选项在 options 中的索引。
#[allow(dead_code)]
fn apply_cycle_row(
    cfg: &mut crate::config::PeriConfig,
    row: usize,
    forward: bool,
) -> Option<usize> {
    let options: &[&str] = match row {
        ROW_STREAMING => STREAMING_OPTS,
        ROW_LANGUAGE => LANGUAGE_OPTS,
        ROW_ACTIVE_ALIAS => ALIAS_OPTS,
        ROW_PERMISSION_MODE => PERMISSION_OPTS,
        _ => return None,
    };
    let cur_idx = match row {
        ROW_STREAMING => STREAMING_OPTS
            .iter()
            .position(|s| cfg.config.streaming_mode.as_deref() == Some(*s))
            .unwrap_or(0),
        ROW_LANGUAGE => LANGUAGE_OPTS
            .iter()
            .position(|s| cfg.config.language.as_deref() == Some(*s))
            .unwrap_or(0),
        ROW_ACTIVE_ALIAS => ALIAS_OPTS
            .iter()
            .position(|s| cfg.config.active_alias == *s)
            .unwrap_or(0),
        ROW_PERMISSION_MODE => 0, // 不持久化
        _ => return None,
    };
    let next = if forward {
        (cur_idx + 1) % options.len()
    } else {
        (cur_idx + options.len() - 1) % options.len()
    };
    let new_val = options[next];
    match row {
        ROW_STREAMING => cfg.config.streaming_mode = Some(new_val.to_string()),
        ROW_LANGUAGE => cfg.config.language = Some(new_val.to_string()),
        ROW_ACTIVE_ALIAS => cfg.config.active_alias = new_val.to_string(),
        ROW_PERMISSION_MODE => {} // 由 PERMISSION_MODE_HANDLE 处理
        _ => return None,
    }
    Some(next)
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
    use crate::config::PeriConfig;

    #[test]
    fn test_apply_toggle_row_show_diff_flips() {
        let mut cfg = PeriConfig::default();
        assert!(!cfg.config.diff_enabled);
        let new = apply_toggle_row(&mut cfg, ROW_SHOW_DIFF);
        assert_eq!(new, Some(true));
        assert!(cfg.config.diff_enabled);
        let new = apply_toggle_row(&mut cfg, ROW_SHOW_DIFF);
        assert_eq!(new, Some(false));
        assert!(!cfg.config.diff_enabled);
    }

    #[test]
    fn test_apply_toggle_row_cache_warn_flips() {
        let mut cfg = PeriConfig::default();
        let initial = cfg.config.show_cache_warning;
        let new = apply_toggle_row(&mut cfg, ROW_CACHE_WARN);
        assert_eq!(new, Some(!initial));
        assert_eq!(cfg.config.show_cache_warning, !initial);
    }

    #[test]
    fn test_apply_toggle_row_1m_context_handles_none_initial() {
        // 默认 context_1m = None（unwrap_or(false) → false → toggle 为 true）
        let mut cfg = PeriConfig::default();
        assert_eq!(cfg.config.context_1m, None);
        let new = apply_toggle_row(&mut cfg, ROW_1M_CONTEXT);
        assert_eq!(new, Some(true));
        assert_eq!(cfg.config.context_1m, Some(true));
        let new = apply_toggle_row(&mut cfg, ROW_1M_CONTEXT);
        assert_eq!(new, Some(false));
        assert_eq!(cfg.config.context_1m, Some(false));
    }

    #[test]
    fn test_apply_toggle_row_invalid_returns_none() {
        let mut cfg = PeriConfig::default();
        // ROW_STREAMING 是 Cycle 不是 Toggle——应返回 None
        assert_eq!(apply_toggle_row(&mut cfg, ROW_STREAMING), None);
        // 越界 row
        assert_eq!(apply_toggle_row(&mut cfg, 99), None);
    }

    #[test]
    fn test_apply_cycle_row_streaming_forward_wraps() {
        let mut cfg = PeriConfig::default();
        cfg.config.streaming_mode = Some("none".into()); // idx=2
        let next = apply_cycle_row(&mut cfg, ROW_STREAMING, true);
        assert_eq!(next, Some(0)); // wrap to streaming
        assert_eq!(cfg.config.streaming_mode.as_deref(), Some("streaming"));
    }

    #[test]
    fn test_apply_cycle_row_alias_backward() {
        let mut cfg = PeriConfig::default();
        cfg.config.active_alias = "opus".into(); // idx=0
        let prev = apply_cycle_row(&mut cfg, ROW_ACTIVE_ALIAS, false);
        assert_eq!(prev, Some(2)); // wrap to haiku
        assert_eq!(cfg.config.active_alias, "haiku");
    }

    #[test]
    fn test_apply_cycle_row_language_forward_from_unknown_resets() {
        // 当前值为非选项时，unwrap_or(0) → 视为 idx=0，forward 后到 idx=1
        let mut cfg = PeriConfig::default();
        cfg.config.language = Some("fr".into()); // 非合法选项
        let next = apply_cycle_row(&mut cfg, ROW_LANGUAGE, true);
        assert_eq!(next, Some(1));
        assert_eq!(cfg.config.language.as_deref(), Some("zh"));
    }

    #[test]
    fn test_apply_cycle_row_invalid_returns_none() {
        let mut cfg = PeriConfig::default();
        // ROW_SHOW_DIFF 是 Toggle 不是 Cycle——应返回 None
        assert_eq!(apply_cycle_row(&mut cfg, ROW_SHOW_DIFF, true), None);
        assert_eq!(apply_cycle_row(&mut cfg, 99, true), None);
    }

    #[test]
    fn test_parse_permission_mode_roundtrip() {
        for opt in PERMISSION_OPTS {
            let mode = parse_permission_mode(opt);
            assert!(mode.is_some(), "{} 应解析成功", opt);
            let label = permission_mode_label(mode.unwrap());
            assert_eq!(label, *opt, "label 与原始字符串应一致");
        }
    }

    #[test]
    fn test_parse_permission_mode_invalid() {
        assert!(parse_permission_mode("invalid").is_none());
        assert!(parse_permission_mode("").is_none());
    }
}
