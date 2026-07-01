//! ratatui-kit LoginPanel component.
//!
//! H1f（Iteration 14）：从 PROVIDER_LIST atom 读取真实 provider 配置
//! （由 service_snapshot 从 peri_config.providers 派生）。Enter 通过
//! PERI_CONFIG_HANDLE 切换 active_provider_id 并持久化。
//!
//! 简化设计：只读列表 + Enter 激活；不提供 New/Edit/Delete UI（这些操作
//! 通过 Setup Wizard 完成）。

use crate::kit::atoms::{PERI_CONFIG_HANDLE, PROVIDER_LIST, ProviderSummary};
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
pub fn LoginPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);
    let store = hooks.use_store(*PROVIDER_LIST.get().unwrap());
    let providers: Vec<ProviderSummary> = store.read().clone();
    let _ = store;
    let count = providers.len();
    let bump = hooks.use_state(|| 0u32);

    hooks.use_local_events({
        let providers = providers.clone();
        move |event: Event| {
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => close_panel(),
                    KeyCode::Up | KeyCode::Char('k') => {
                        *cursor.write() = cursor.read().saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let mut c = cursor.write();
                        if count > 0 {
                            *c = (*c + 1).min(count - 1);
                        }
                    }
                    KeyCode::Enter => {
                        let sel = *cursor.read();
                        if let Some(p) = providers.get(sel) {
                            activate_provider(&p.id);
                        }
                        *bump.write() += 1;
                    }
                    _ => {}
                }
            }
        }
    });

    let _ = *bump.read();
    let sel = *cursor.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        format!("  {} providers configured", count),
        Style::new().fg(theme::TEXT).bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  Enter) Activate  Esc) Close",
        Style::new().fg(theme::MUTED).italic(),
    )]));
    lines.push(Line::from(""));

    if providers.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No providers configured",
            Style::new().fg(theme::MUTED),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Run setup wizard or edit ~/.peri/settings.json",
            Style::new().fg(theme::MUTED),
        )]));
    } else {
        for (i, p) in providers.iter().enumerate() {
            let is_cursor = i == sel;
            let cursor_mark = if is_cursor { ">" } else { " " };
            let row_style = if is_cursor {
                Style::new().fg(theme::THINKING).bold()
            } else {
                Style::new().fg(theme::TEXT)
            };

            let active_marker = if p.is_active {
                Span::styled(" \u{2714}", Style::new().fg(theme::SAGE).bold())
            } else {
                Span::styled("  ", Style::new())
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor_mark),
                    Style::new().fg(theme::THINKING),
                ),
                active_marker,
                Span::styled(p.id.clone(), row_style),
                Span::styled(
                    format!("  ({})", p.provider_type),
                    Style::new().fg(theme::MUTED),
                ),
            ]));

            // API key 状态
            let key_marker = if p.has_api_key {
                ("configured", theme::SAGE)
            } else {
                ("missing", theme::ERROR)
            };
            lines.push(Line::from(vec![
                Span::styled("     API key: ".to_string(), Style::new().fg(theme::MUTED)),
                Span::styled(key_marker.0, Style::new().fg(key_marker.1)),
            ]));
            if let Some(url) = &p.base_url {
                let url_display: String = url.chars().take(70).collect();
                lines.push(Line::from(vec![Span::styled(
                    format!("     Base URL: {}", url_display),
                    Style::new().fg(theme::DIM),
                )]));
            }
            lines.push(Line::from(""));
        }
    }

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Login ")
                .fg(theme::THINKING)
                .bold()
                .centered(),
            width: Constraint::Length(80),
            height: Constraint::Length(22),
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

/// H1f: 切换 active_provider_id 并持久化到 settings.json。
fn activate_provider(provider_id: &str) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let mut cfg = handle.write();
    if cfg.config.active_provider_id == provider_id {
        return;
    }
    cfg.config.active_provider_id = provider_id.to_string();
    let snap = cfg.clone();
    drop(cfg);
    let _ = crate::config::save(&snap);
    tracing::info!(provider_id, "LoginPanel: active_provider_id switched");
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
