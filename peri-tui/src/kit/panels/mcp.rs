//! ratatui-kit McpPanel component.
//!
//! H1d（Iteration 14）：从 MCP_SERVERS atom 读取真实 MCP server 列表（由
//! service_snapshot 从 mcp_pool.all_server_infos 派生）。结合 SERVICE_SNAPSHOT.mcp
//! 显示初始化阶段摘要。只读面板——MCP 配置通过 ~/.claude/settings.json 管理。

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{LANG_VERSION, MCP_SERVERS, McpServerSummary, SERVICE_SNAPSHOT};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
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

#[component]
pub fn McpPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    // 外部滚动状态——面板滚轮仲裁（panel_scroll.rs）驱动，统一 3 行/格 + 节流
    let sv = hooks.use_state(ScrollViewState::default);
    let store = hooks.use_atom(&MCP_SERVERS);
    let servers: Vec<McpServerSummary> = store.read().clone();
    let _ = store;

    let snap_store = hooks.use_atom(&SERVICE_SNAPSHOT);
    let init_phase = snap_store.read().mcp.init_phase;
    let connected_total = snap_store.read().mcp.connected;
    let config_total = snap_store.read().mcp.total;
    let _ = snap_store;
    let _ = hooks.use_atom(&LANG_VERSION);

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
                KeyCode::Enter => {
                    // 选中 OAuth 待授权 server：Enter = 授权按钮（触发
                    // mcp/oauth_start RPC → host pool 异步授权 → popup 弹出）。
                    // 其他状态保持原行为：关闭面板。
                    let servers = MCP_SERVERS.state().read().clone();
                    let sel = *selected.read();
                    let is_needs_auth = servers.get(sel).map(|s| s.needs_auth).unwrap_or(false);
                    if is_needs_auth {
                        if let Some(name) = servers.get(sel).map(|s| s.name.clone()) {
                            start_oauth(name);
                        }
                    } else {
                        close_panel();
                    }
                }
                KeyCode::Up => {
                    let mut s = selected.write();
                    *s = previous_selection(*s);
                }
                KeyCode::Down => {
                    let mut s = selected.write();
                    let count = MCP_SERVERS.state().read().len();
                    if count > 0 {
                        *s = next_selection(*s, count);
                    }
                }
                _ => {}
            }
            EventResult::Consumed
        }
    });

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 视口跟随：让选中项始终可见（issue 2026-07-06-panels-selection-no-scroll-follow）。
    // panel 高度 18 - border 2 - header 2 - footer 2 = 12 行；每项 2 行 → 可见 6 个。
    const VISIBLE_ITEMS: usize = 6;
    let scroll_start = scroll_start_for_selected(sel, servers.len(), VISIBLE_ITEMS);

    // 摘要头：init phase / connected / total
    let phase_label = match init_phase {
        crate::kit::atoms::McpInitPhase::Pending => i18n::tr("panel-mcp-phase-pending"),
        crate::kit::atoms::McpInitPhase::Initializing => i18n::tr("panel-mcp-phase-initializing"),
        crate::kit::atoms::McpInitPhase::Ready => i18n::tr("panel-mcp-phase-ready"),
        crate::kit::atoms::McpInitPhase::Failed => i18n::tr("panel-mcp-phase-failed"),
    };
    lines.push(Line::from(vec![
        Span::styled(
            i18n::tr("panel-mcp-pool-label"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        ),
        Span::styled(
            phase_label.clone(),
            Style::new()
                .fg(theme_def.read().semantic.border.active)
                .bold(),
        ),
        Span::styled(
            i18n::tr_args(
                "panel-mcp-connected",
                &[
                    (
                        "connected".to_string(),
                        FluentValue::from(connected_total as i64),
                    ),
                    ("total".to_string(), FluentValue::from(config_total as i64)),
                ],
            ),
            Style::new().fg(theme_def.read().semantic.text.primary),
        ),
    ]));
    lines.push(Line::from(""));

    if servers.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-mcp-empty"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-mcp-empty-hint"),
            Style::new().fg(theme_def.read().semantic.text.muted),
        )]));
    } else {
        for (i, s) in servers
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
            let (status_icon, status_color) = derive_status_style(&s.status);

            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", cursor),
                    Style::new().fg(theme_def.read().component.panel.title),
                ),
                Span::styled(s.name.clone(), name_style),
                Span::styled(format!("  {}", status_icon), Style::new().fg(status_color)),
                Span::styled(format!(" {}", s.status), Style::new().fg(status_color)),
            ]));
            lines.push(Line::from(vec![
                Span::styled(
                    i18n::tr_args(
                        "panel-mcp-server-detail",
                        &[
                            (
                                "transport".to_string(),
                                FluentValue::from(s.transport.as_str()),
                            ),
                            ("count".to_string(), FluentValue::from(s.tools_count as i64)),
                        ],
                    ),
                    Style::new().fg(theme_def.read().semantic.text.dim),
                ),
                Span::styled(
                    if s.needs_auth {
                        i18n::tr("panel-mcp-needs-auth")
                    } else {
                        String::new()
                    },
                    Style::new().fg(theme_def.read().semantic.status.warning),
                ),
            ]));
        }
    }

    lines.push(Line::from(""));
    // 底部 hint：选中 OAuth 待授权 server 时提示 Enter 授权，否则提示 Enter 关闭
    let sel_hint = servers.get(sel).map(|s| s.needs_auth).unwrap_or(false);
    if sel_hint {
        lines.push(
            Line::from(i18n::tr("panel-mcp-oauth-hint")).fg(theme_def.read().semantic.text.dim),
        );
    } else {
        lines.push(
            Line::from(i18n::tr("common-nav-enter-close")).fg(theme_def.read().semantic.text.dim),
        );
    }

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    // 面板滚轮仲裁注册（每帧覆盖写入，area 用上一帧组件区域）
    crate::kit::panel_scroll::register_panel_scroll(PanelKind::Mcp, hooks.use_previous_size(), sv);

    panel_shell!(PanelKind::Mcp, {
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

fn derive_status_style(status: &str) -> (String, ratatui::style::Color) {
    if status.contains("connected") {
        (
            i18n::tr("panel-mcp-icon-connected"),
            THEME_ATOM.state().read().semantic.status.success,
        )
    } else if status.contains("error") || status.contains("failed") {
        (
            i18n::tr("panel-mcp-icon-error"),
            THEME_ATOM.state().read().semantic.status.error,
        )
    } else {
        (
            i18n::tr("panel-mcp-icon-unknown"),
            THEME_ATOM.state().read().semantic.text.muted,
        )
    }
}

fn close_panel() {
    // I19-A: 弹栈而非清空整个栈，避免同时打开多个不同组面板时关闭一个会全部关闭
    crate::kit::panel_registry::close_active_panel();
}

/// 发起 OAuth 授权（MCP 面板授权按钮）：`mcp/oauth_start` RPC → host pool
/// spawn_oauth_flow → OauthNeeded 事件 → TUI 弹出授权 popup。
fn start_oauth(server_name: String) {
    if let Some(client_handle) = crate::kit::atoms::ACP_CLIENT_HANDLE.get() {
        let client = client_handle.clone();
        tokio::spawn(async move {
            let params = serde_json::json!({ "server_name": server_name });
            if let Err(e) = client.send_raw_request("mcp/oauth_start", params).await {
                tracing::warn!(error = %e, "mcp/oauth_start RPC failed");
            }
        });
    } else {
        tracing::warn!(target: "mcp-panel", "ACP_CLIENT_HANDLE not set, oauth_start skipped");
    }
}
