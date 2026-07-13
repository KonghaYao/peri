//! ratatui-kit LoginPanel component.
//!
//! H1f（Iteration 14）：从 PROVIDER_LIST atom 读取真实 provider 配置
//! （由 service_snapshot 从 peri_config.providers 派生）。Enter 通过
//! PERI_CONFIG_HANDLE 切换 active_provider_id 并持久化。
//!
//! 简化设计：只读列表 + Enter 激活；不提供 New/Edit/Delete UI（这些操作
//! 通过 Setup Wizard 完成）。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    NOTIFICATION, Notification, PERI_CONFIG_HANDLE, PROVIDER_LIST, ProviderSummary,
    SERVICE_SNAPSHOT,
};
use crate::kit::list_nav::{next_selection, previous_selection};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
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
use std::time::{Duration, Instant};

#[component]
pub fn LoginPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let cursor = hooks.use_state(|| 0usize);
    let store = hooks.use_atom(&PROVIDER_LIST);
    let providers: Vec<ProviderSummary> = store.read().clone();
    let _ = store;
    let count = providers.len();

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
                    let mut c = cursor.write();
                    *c = previous_selection(*c);
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
                        let provider_type = p.provider_type.clone();
                        // 同步写 PERI_CONFIG_HANDLE + 更新 PROVIDER_LIST.is_active
                        if let Some(handle) = PERI_CONFIG_HANDLE.get() {
                            let mut cfg = handle.write();
                            cfg.config.active_provider_id = provider_id.clone();
                            // 即时推送 SERVICE_SNAPSHOT——同时更新 provider_name 和
                            // model_name（不同 provider 的 alias→model 映射可能不同）
                            let snap = cfg.clone();
                            drop(cfg);
                            let resolved_name = {
                                let active_prov = snap
                                    .config
                                    .providers
                                    .iter()
                                    .find(|p| p.id == provider_id);
                                active_prov
                                    .and_then(|p| p.models.get_model(&snap.config.active_alias))
                                    .map(|s| s.to_string())
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| snap.config.active_alias.clone())
                            };
                            let s_handle = SERVICE_SNAPSHOT.state();
                            let mut svc_snap = s_handle.read().clone();
                            svc_snap.provider_name = provider_type;
                            svc_snap.model_name = resolved_name;
                            *s_handle.write() = svc_snap;
                        }
                        // 更新 PROVIDER_LIST 的 is_active 标记——该 atom 是启动时
                        // 静态构建的，is_active 不会自动随 active_provider_id 变更刷新
                        let updated_providers: Vec<ProviderSummary> = latest_providers
                            .iter()
                            .map(|pr| ProviderSummary {
                                is_active: pr.id == provider_id,
                                ..pr.clone()
                            })
                            .collect();
                        *PROVIDER_LIST.state().write() = updated_providers;
                        // 异步持久化——始终执行
                        tokio::spawn(async move {
                            activate_provider(&provider_id);
                        });
                    }
                    // 关闭面板——无论选择相同还是不同 provider 都关闭
                    close_panel();
                }
                _ => {}
            }
            EventResult::Consumed
        }
    });

    let sel = *cursor.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // S16：TUI-PAGE.md §6.2 样式——Enter::select · Esc::close
    lines.push(Line::from(vec![Span::styled(
        format!("  {} providers configured", count),
        Style::new()
            .fg(theme_def.read().semantic.text.primary)
            .bold(),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "  Enter::select · Esc::close",
        Style::new()
            .fg(theme_def.read().semantic.text.muted)
            .italic(),
    )]));
    lines.push(Line::from(""));

    if providers.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No providers configured",
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Run setup wizard or edit ~/.peri/settings.json",
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
    } else {
        for (i, p) in providers.iter().enumerate() {
            let is_cursor = i == sel;
            let cursor_mark = if is_cursor { ">" } else { " " };
            let row_style = if is_cursor {
                Style::new()
                    .fg(theme_def.read().component.panel.title)
                    .bold()
            } else {
                Style::new().fg(theme_def.read().semantic.text.primary)
            };

            let active_marker = if p.is_active {
                Span::styled(
                    " \u{2714}",
                    Style::new()
                        .fg(theme_def.read().semantic.status.success)
                        .bold(),
                )
            } else {
                Span::styled("  ", Style::new())
            };

            // S16：provider_id (provider_type) ——无 "API key:" 前缀
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor_mark),
                    Style::new().fg(theme_def.read().component.panel.title),
                ),
                active_marker,
                Span::styled(format!("{}  ({})", p.id, p.provider_type), row_style),
            ]));

            // API key 状态（configured / missing）
            let key_marker = if p.has_api_key {
                (
                    "api key: configured",
                    theme_def.read().semantic.status.success,
                )
            } else {
                ("api key: missing", theme_def.read().semantic.status.error)
            };
            lines.push(Line::from(vec![Span::styled(
                format!("     {}", key_marker.0),
                Style::new().fg(key_marker.1),
            )]));
            if let Some(url) = &p.base_url {
                let url_display: String = url.chars().take(70).collect();
                lines.push(Line::from(vec![Span::styled(
                    format!("     base url: {}", url_display),
                    Style::new().fg(theme_def.read().semantic.text.dim),
                )]));
            }
            lines.push(Line::from(""));
        }
    }

    // S16：底部 hints
    lines.push(Line::from(vec![Span::styled(
        "  \u{2191}/\u{2193}::navigate  Enter::select  Esc::close",
        Style::new()
            .fg(theme_def.read().semantic.text.muted)
            .italic(),
    )]));

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Login, {
            ScrollView(
                scrollbars: crate::kit::panel_registry::clean_scrollbars(),
                width: Constraint::Fill(1),
                height: Constraint::Fill(1),
            ) {
                Text(text: content)
            }
    })
}

/// H1f: 持久化当前 PERI_CONFIG_HANDLE 到 settings.json。
///
/// 不检查 active_provider_id 是否变更——调用方已在事件处理器中同步更新。
fn activate_provider(_provider_id: &str) {
    let Some(handle) = PERI_CONFIG_HANDLE.get() else {
        return;
    };
    let cfg = handle.read();
    let snap = cfg.clone();
    drop(cfg);
    match crate::config::save(&snap) {
        Ok(()) => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr("config-saved").to_string(),
                until: Instant::now() + Duration::from_secs(1),
            });
        }
        Err(e) => {
            *NOTIFICATION.state().write() = Some(Notification {
                message: i18n::tr_args(
                    "config-save-failed",
                    &[(
                        "error".to_string(),
                        FluentValue::from(e.to_string().as_str()),
                    )],
                ),
                until: Instant::now() + Duration::from_secs(2),
            });
        }
    }
    tracing::info!(provider_id = _provider_id, "LoginPanel: config persisted");
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

#[cfg(test)]
mod tests {}
