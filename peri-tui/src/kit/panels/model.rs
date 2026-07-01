//! ratatui-kit ModelPanel component.
//!
//! S6c：alias 列表沿用静态元数据（Opus/Sonnet/Haiku），但**当前激活 alias**
//! 从 `SERVICE_SNAPSHOT` atom 读取——这样面板和 status bar 始终一致。
//! Enter/←→ 操作目前只更新本地 selected_tab state，**真实切换 provider/model**
//! 需要 S11 解耦后通过 AcpClient 触发（暂留 TODO）。

use crate::kit::atoms::{PERI_CONFIG_HANDLE, SERVICE_SNAPSHOT};
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

// ---------------------------------------------------------------------------
// 静态 alias 元数据（与 active 状态无关）
// ---------------------------------------------------------------------------

struct ModelAliasEntry {
    name: &'static str,
    key: &'static str,
    effort: &'static str,
    max_tokens: u32,
    context_1m: bool,
    model_id: &'static str,
}

const MODEL_ALIASES: &[ModelAliasEntry] = &[
    ModelAliasEntry {
        name: "Opus",
        key: "opus",
        effort: "high",
        max_tokens: 32000,
        context_1m: false,
        model_id: "claude-opus-4-20250514",
    },
    ModelAliasEntry {
        name: "Sonnet",
        key: "sonnet",
        effort: "high",
        max_tokens: 64000,
        context_1m: false,
        model_id: "claude-sonnet-4-20250514",
    },
    ModelAliasEntry {
        name: "Haiku",
        key: "haiku",
        effort: "low",
        max_tokens: 8000,
        context_1m: false,
        model_id: "claude-3-5-haiku-20241022",
    },
];

#[component]
pub fn ModelPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);
    // selected_tab stores the index of the selected alias
    let selected_tab = hooks.use_state(|| 1usize); // default Sonnet

    // S6c: 订阅 SERVICE_SNAPSHOT——active alias 来自 atom，确保面板和 status bar 一致
    let snapshot = hooks.use_store(*SERVICE_SNAPSHOT.get().unwrap());
    let active_alias = snapshot.read().model_alias.clone();
    let active_provider = snapshot.read().provider_name.clone();
    let _ = snapshot; // StoreState 是 Copy，无需显式 drop

    hooks.use_local_events({
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        // 由 PanelOverlay 上层 Esc 处理关闭
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        let mut c = cursor.write();
                        *c = c.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let mut c = cursor.write();
                        *c = (*c + 1).min(MODEL_ALIASES.len() - 1);
                    }
                    KeyCode::Enter => {
                        // H2: 通过 PERI_CONFIG_HANDLE 直接 write active_alias。
                        // ACP server 持同一 Arc，立即生效；service_snapshot 2s 内
                        // 派生到 SERVICE_SNAPSHOT.model_alias 让 status bar 同步刷新。
                        let sel = *cursor.read();
                        *selected_tab.write() = sel;
                        if let Some(handle) = PERI_CONFIG_HANDLE.get() {
                            let new_alias = MODEL_ALIASES[sel].key.to_string();
                            let mut cfg = handle.write();
                            if cfg.config.active_alias != new_alias {
                                cfg.config.active_alias = new_alias;
                                tracing::info!(
                                    alias = MODEL_ALIASES[sel].key,
                                    "ModelPanel: active_alias switched"
                                );
                            }
                        }
                        // 关闭面板
                        if let Some(atom) = crate::kit::atoms::ACTIVE_PANEL.get() {
                            *atom.write() = None;
                        }
                        if let Some(atom) = crate::kit::atoms::OPEN_PANELS.get() {
                            atom.write().clear();
                        }
                    }
                    KeyCode::Left => {
                        let mut s = selected_tab.write();
                        *s = s.saturating_sub(1);
                        *cursor.write() = *s;
                    }
                    KeyCode::Right => {
                        let mut s = selected_tab.write();
                        *s = (*s + 1).min(MODEL_ALIASES.len() - 1);
                        *cursor.write() = *s;
                    }
                    _ => {}
                }
            }
        }
    });

    let sel = *cursor.read();
    let local_selected = *selected_tab.read();

    // active alias 优先取 atom；atom 为空时回退到本地 selected_tab
    let active_idx = MODEL_ALIASES
        .iter()
        .position(|e| e.key.eq_ignore_ascii_case(&active_alias))
        .unwrap_or(local_selected);
    let active_entry = &MODEL_ALIASES[active_idx];
    let provider_label = if active_provider.is_empty() {
        "(unconfigured)".to_string()
    } else {
        active_provider.clone()
    };

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Header
    lines.push(Line::from(vec![Span::styled(
        "  Model Alias Selection",
        Style::new().fg(theme::TEXT).bold(),
    )]));
    lines.push(Line::from(vec![
        Span::styled("  Provider: ", Style::new().fg(theme::MUTED)),
        Span::styled(provider_label, Style::new().fg(theme::ACCENT).bold()),
    ]));
    lines.push(Line::from(""));

    // Alias rows
    for (i, entry) in MODEL_ALIASES.iter().enumerate() {
        let is_selected = i == active_idx;
        let is_cursor = i == sel;
        let cursor_mark = if is_cursor { "\u{276f}" } else { " " };
        let check = if is_selected { "\u{2714}" } else { " " };

        let name_style = if is_selected {
            Style::new().fg(theme::SAGE).bold()
        } else if is_cursor {
            Style::new().fg(theme::THINKING).bold()
        } else {
            Style::new().fg(theme::TEXT)
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", cursor_mark),
                Style::new().fg(theme::THINKING),
            ),
            Span::styled(format!("{:<10}", entry.name), name_style),
            Span::styled(
                format!(" {}", check),
                if is_selected {
                    Style::new().fg(theme::SAGE)
                } else {
                    Style::new().fg(theme::MUTED)
                },
            ),
            Span::styled(
                format!("  {}", entry.model_id),
                Style::new().fg(theme::MUTED),
            ),
        ]));
    }

    lines.push(Line::from(""));

    // Current selection details
    lines.push(Line::from(vec![Span::styled(
        format!("  Active: {}", active_entry.name),
        Style::new().fg(theme::ACCENT).bold(),
    )]));
    lines.push(Line::from(vec![
        Span::styled("  Model ID: ", Style::new().fg(theme::MUTED)),
        Span::styled(active_entry.model_id, Style::new().fg(theme::TEXT)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Effort: ", Style::new().fg(theme::MUTED)),
        Span::styled(active_entry.effort, Style::new().fg(theme::WARNING).bold()),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Max Tokens: ", Style::new().fg(theme::MUTED)),
        Span::styled(
            active_entry.max_tokens.to_string(),
            Style::new().fg(theme::TEXT),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  1M Context: ", Style::new().fg(theme::MUTED)),
        Span::styled(
            if active_entry.context_1m { "ON" } else { "OFF" },
            if active_entry.context_1m {
                Style::new().fg(theme::SAGE)
            } else {
                Style::new().fg(theme::MUTED)
            },
        ),
    ]));

    // Footer
    lines.push(Line::from(""));
    lines.push(Line::from("  j/k) Nav  Enter) Select  ←/→) Switch  q) Close").fg(theme::DIM));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Model ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(50),
            height: Constraint::Length(18),
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
