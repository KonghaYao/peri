//! ratatui-kit LoginPanel component.
//!
//! H1f（Iteration 14）：从 PROVIDER_LIST atom 读取真实 provider 配置
//! （由 service_snapshot 从 peri_config.providers 派生）。Enter 通过
//! PERI_CONFIG_HANDLE 切换 active_provider_id 并持久化。
//!
//! 简化设计：只读列表 + Enter 激活；不提供 New/Edit/Delete UI（这些操作
//! 通过 Setup Wizard 完成）。

use crate::app::panel_types::PanelKind;
use crate::kit::atoms::{PERI_CONFIG_HANDLE, PROVIDER_LIST, ProviderSummary};
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
pub fn LoginPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let cursor = hooks.use_state(|| 0usize);
    let store = hooks.use_atom(&PROVIDER_LIST);
    let providers: Vec<ProviderSummary> = store.read().clone();
    let _ = store;
    let count = providers.len();
    let bump = hooks.use_state(|| 0u32);

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        // S16：不在闭包里捕获 providers（use_event_handler 无 deps 参数），
        // 改为执行时重新从 atom 读取最新数据，避免导航/选择操作使用陈旧快照。
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
                    *cursor.write() = previous_selection(*cursor.read());
                }
                KeyCode::Down => {
                    let latest = PROVIDER_LIST.state().read().len();
                    let mut c = cursor.write();
                    if latest > 0 {
                        *c = next_selection(*c, latest);
                    }
                }
                KeyCode::Enter => {
                    let sel = *cursor.read();
                    let latest_providers = PROVIDER_LIST.state().read().clone();
                    if let Some(p) = latest_providers.get(sel) {
                        let provider_id = p.id.clone();
                        // S16：异步切换 + 持久化，避免同步 disk IO 阻塞主线程
                        std::thread::spawn(move || {
                            activate_provider(&provider_id);
                        });
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

    // S16：TUI-PAGE.md §6.2 样式——Enter::activate · Esc::close
    lines.push(Line::from(vec![Span::styled(
        format!("  {} providers configured", count),
        Style::new().fg(theme::semantic().text.primary).bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  Enter::activate · Esc::close",
        Style::new().fg(theme::semantic().text.muted).italic(),
    )]));
    lines.push(Line::from(""));

    if providers.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No providers configured",
            Style::new().fg(theme::semantic().text.muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Run setup wizard or edit ~/.peri/settings.json",
            Style::new().fg(theme::semantic().text.muted),
        )]));
    } else {
        for (i, p) in providers.iter().enumerate() {
            let is_cursor = i == sel;
            let cursor_mark = if is_cursor { ">" } else { " " };
            let row_style = if is_cursor {
                Style::new().fg(theme::component().panel.title).bold()
            } else {
                Style::new().fg(theme::semantic().text.primary)
            };

            let active_marker = if p.is_active {
                Span::styled(
                    " \u{2714}",
                    Style::new().fg(theme::semantic().status.success).bold(),
                )
            } else {
                Span::styled("  ", Style::new())
            };

            // S16：provider_id (provider_type) ——无 "API key:" 前缀
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor_mark),
                    Style::new().fg(theme::component().panel.title),
                ),
                active_marker,
                Span::styled(format!("{}  ({})", p.id, p.provider_type), row_style),
            ]));

            // API key 状态（configured / missing）
            let key_marker = if p.has_api_key {
                ("api key: configured", theme::semantic().status.success)
            } else {
                ("api key: missing", theme::semantic().status.error)
            };
            lines.push(Line::from(vec![Span::styled(
                format!("     {}", key_marker.0),
                Style::new().fg(key_marker.1),
            )]));
            if let Some(url) = &p.base_url {
                let url_display: String = url.chars().take(70).collect();
                lines.push(Line::from(vec![Span::styled(
                    format!("     base url: {}", url_display),
                    Style::new().fg(theme::semantic().text.dim),
                )]));
            }
            lines.push(Line::from(""));
        }
    }

    // S16：底部 hints
    lines.push(Line::from(vec![Span::styled(
        "  \u{2191}/\u{2193}::navigate  Enter::activate  Esc::close",
        Style::new().fg(theme::semantic().text.muted).italic(),
    )]));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Login, {
            ScrollView(
                scroll_bars: crate::kit::panel_registry::clean_scrollbars(),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
    })
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
