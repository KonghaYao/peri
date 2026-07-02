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
    let store = hooks.use_atom(&PROVIDER_LIST);
    let providers: Vec<ProviderSummary> = store.read().clone();
    let _ = store;
    let count = providers.len();
    let bump = hooks.use_state(|| 0u32);

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        let providers = providers.clone();
        move |event| {
            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
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
            EventResult::Consumed
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
    if !apply_provider_switch(&mut cfg, provider_id) {
        return;
    }
    let snap = cfg.clone();
    drop(cfg);
    let _ = crate::config::save(&snap);
    tracing::info!(provider_id, "LoginPanel: active_provider_id switched");
}

/// 纯函数：若 `provider_id` 与当前 active_provider_id 不同，则更新并返回 true；
/// 否则返回 false（无变更，调用方应跳过持久化）。
///
/// 提取为独立函数便于单测——避免依赖全局 atom 和磁盘 IO。
fn apply_provider_switch(cfg: &mut crate::config::PeriConfig, provider_id: &str) -> bool {
    if cfg.config.active_provider_id == provider_id {
        return false;
    }
    cfg.config.active_provider_id = provider_id.to_string();
    true
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PeriConfig;

    #[test]
    fn test_apply_provider_switch_updates_when_different() {
        let mut cfg = PeriConfig::default();
        // 默认 active_provider_id 为空
        assert!(cfg.config.active_provider_id.is_empty());

        let changed = apply_provider_switch(&mut cfg, "anthropic-prod");
        assert!(changed, "切换到不同 provider 应返回 true");
        assert_eq!(cfg.config.active_provider_id, "anthropic-prod");
    }

    #[test]
    fn test_apply_provider_switch_noop_when_same() {
        let mut cfg = PeriConfig::default();
        cfg.config.active_provider_id = "openai-prod".into();

        let changed = apply_provider_switch(&mut cfg, "openai-prod");
        assert!(!changed, "切换到相同 provider 应返回 false（无变更）");
        assert_eq!(cfg.config.active_provider_id, "openai-prod");
    }

    #[test]
    fn test_apply_provider_switch_to_empty_string_still_changes() {
        // 边界：从有值切到空串——仍视为变更（调用方负责保证 provider_id 有效）
        let mut cfg = PeriConfig::default();
        cfg.config.active_provider_id = "openai-prod".into();

        let changed = apply_provider_switch(&mut cfg, "");
        assert!(changed, "从有值切到空串仍是状态变更");
        assert!(cfg.config.active_provider_id.is_empty());
    }

    #[test]
    fn test_apply_provider_switch_persists_other_fields() {
        // 切换 provider 不应破坏其他字段
        let mut cfg = PeriConfig::default();
        cfg.config.providers.push(crate::config::ProviderConfig {
            id: "p1".into(),
            provider_type: "anthropic".into(),
            api_key: "sk-test".into(),
            ..Default::default()
        });
        cfg.config.active_alias = "sonnet".into();

        let changed = apply_provider_switch(&mut cfg, "p1");
        assert!(changed);
        assert_eq!(cfg.config.active_provider_id, "p1");
        // 其他字段保留
        assert_eq!(cfg.config.active_alias, "sonnet");
        assert_eq!(cfg.config.providers.len(), 1);
        assert_eq!(cfg.config.providers[0].id, "p1");
    }
}
