//! ratatui-kit McpPanel component.
//!
//! H1d（Iteration 14）：从 MCP_SERVERS atom 读取真实 MCP server 列表（由
//! service_snapshot 从 mcp_pool.all_server_infos 派生）。结合 SERVICE_SNAPSHOT.mcp
//! 显示初始化阶段摘要。只读面板——MCP 配置通过 ~/.claude/settings.json 管理。
//!
//! OAuth 授权入口（详情视图）：列表选中「需要认证」server 按 Enter 进入
//! 详情视图，详情里选 [ 授权 ] 按钮（Enter/鼠标点击）才触发 mcp/oauth_start
//! RPC → host 异步授权 → 弹出 OAuthPopup；[ 返回 ] 或 Esc 回列表。

use std::collections::HashMap;

use crate::app::panel_types::PanelKind;
use crate::i18n;
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, AVAILABLE_SLASH_COMMANDS, LANG_VERSION, MCP_SERVERS, McpServerSummary,
    SERVICE_SNAPSHOT,
};
use crate::kit::list_nav::{next_selection, previous_selection, scroll_start_for_selected};
use crate::kit::panel_mouse::{AreaTracker, left_down};
use crate::kit::text_util::wrap_text;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::Constraint,
        style::{Modifier, Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

/// 面板视图：列表 ⇄ OAuth 授权详情。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpView {
    List,
    Detail,
}

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
    // 每 server 已注入的 MCP skill 命令数（决策 1：命令面 fullname =
    // `{server}:{skill}`，来自 available_commands_update 投影；discovery
    // 完成 → 注册表 on_change → 广播 → 本 atom 自动刷新，无需轮询）。
    // 命令面首段 = server 名末段小写（mcp_source_key 派生，与 host 侧
    // 同源），查询时按同规则归一。
    let cmd_store = hooks.use_atom(&AVAILABLE_SLASH_COMMANDS);
    let skills_by_server: HashMap<String, usize> = {
        let mut m: HashMap<String, usize> = HashMap::new();
        for entry in cmd_store.read().iter() {
            if let Some((server, _)) = entry.fullname.split_once(':') {
                *m.entry(server.to_string()).or_insert(0) += 1;
            }
        }
        m
    };
    let _ = cmd_store;
    let mcp_domain_key = |name: &str| -> String {
        name.rsplit_once(':')
            .map(|(_, n)| n)
            .unwrap_or(name)
            .to_lowercase()
    };
    let _ = hooks.use_atom(&LANG_VERSION);

    // 视图状态：List ⇄ Detail（OAuth 授权入口）
    let view = hooks.use_state(|| McpView::List);
    // 进入详情时的列表 index（返回列表后保持选中）
    let detail_idx = hooks.use_state(|| 0usize);
    // 详情视图按钮选择：OAuth server 为 0 = 授权、1 = 返回；默认显式落在
    // 返回，避免列表 Enter 与授权 Enter 的含义混杂。
    let detail_btn = hooks.use_state(|| 0usize);
    // 面板绘制区域（上一帧，绝对坐标）——鼠标命中反推行号
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // 事件闭包另持一份 servers 副本（与渲染端共用同一 atom 副本）
    let servers_for_closure = servers.clone();

    hooks.use_event_handler_with_options(
        EventScope::Current,
        // High：详情视图的 Esc（返回列表）必须先于根层 Normal Esc（关面板）
        // 被消费——同优先级先注册先消费，Normal 会变成死代码（rewind_popup 同款坑）。
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| {
            // ── 鼠标：详情视图按钮行左键点击 = 激活按钮 ──
            if let Event::Mouse(mouse) = event {
                if *view.read() != McpView::Detail {
                    return EventResult::Ignored;
                }
                let Some((row, col)) = left_down(&mouse) else {
                    return EventResult::Ignored;
                };
                let Some(area) = area else {
                    return EventResult::Consumed;
                };
                // 顶部边框行不可点
                if row < area.y.saturating_add(1) {
                    return EventResult::Consumed;
                }
                let content_row = row.saturating_sub(area.y).saturating_sub(1);
                let Some(summary) = servers_for_closure.get(*detail_idx.read()) else {
                    return EventResult::Consumed;
                };
                if content_row == detail_btn_row(summary) {
                    // 按列命中按钮：[ label ]，间隔 2 空格，内容从 area.x 起
                    let labels = detail_button_labels(summary);
                    let mut x = area.x;
                    for (i, label) in labels.iter().enumerate() {
                        let w = format!("[ {label} ]").chars().count() as u16;
                        if col >= x && col < x + w {
                            activate_detail_btn(i, summary, &view, &detail_btn);
                            return EventResult::Consumed;
                        }
                        x = x.saturating_add(w + 2);
                    }
                }
                return EventResult::Consumed;
            }

            let Event::Key(key) = event else {
                return EventResult::Ignored;
            };
            if key.kind != KeyEventKind::Press {
                return EventResult::Ignored;
            }

            if *view.read() == McpView::Detail {
                // ── 详情视图：←→ 选按钮，Enter 激活，Esc 返回列表 ──
                match key.code {
                    KeyCode::Esc => {
                        *view.write() = McpView::List;
                    }
                    KeyCode::Left | KeyCode::Right
                        if servers_for_closure
                            .get(*detail_idx.read())
                            .is_some_and(|summary| summary.needs_auth) =>
                    {
                        let mut b = detail_btn.write();
                        *b = if *b == 0 { 1 } else { 0 };
                    }
                    KeyCode::Enter => {
                        let b = *detail_btn.read();
                        if let Some(summary) = servers_for_closure.get(*detail_idx.read()) {
                            activate_detail_btn(b, summary, &view, &detail_btn);
                        }
                    }
                    _ => {}
                }
                return EventResult::Consumed;
            }

            // ── 列表视图 ──
            match key.code {
                KeyCode::Esc => close_panel(),
                KeyCode::Enter => {
                    let servers = MCP_SERVERS.state().read().clone();
                    let sel = *selected.read();
                    if servers.get(sel).is_some() {
                        *detail_idx.write() = sel;
                        *detail_btn.write() = if servers[sel].needs_auth { 1 } else { 0 };
                        *view.write() = McpView::Detail;
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
        },
    );

    let sel = *selected.read();
    let mut lines: Vec<Line<'_>> = Vec::new();

    // 视口跟随：让选中项始终可见（issue 2026-07-06-panels-selection-no-scroll-follow）。
    // panel 高度 18 - border 2 - header 2 - footer 2 = 12 行；每项 2 行 → 可见 6 个。
    const VISIBLE_ITEMS: usize = 6;
    let scroll_start = scroll_start_for_selected(sel, servers.len(), VISIBLE_ITEMS);

    if *view.read() == McpView::Detail {
        // ── 详情视图（OAuth 授权入口）──
        if let Some(summary) = servers.get(*detail_idx.read()) {
            lines = build_detail_lines(summary, *detail_btn.read(), &theme_def.read());
        } else {
            // detail_idx 失效（列表被刷新缩短）：回到列表
            *view.write() = McpView::List;
        }
    } else {
        // ── 列表视图 ──
        // 摘要头：init phase / connected / total
        let phase_label = match init_phase {
            crate::kit::atoms::McpInitPhase::Pending => i18n::tr("panel-mcp-phase-pending"),
            crate::kit::atoms::McpInitPhase::Initializing => {
                i18n::tr("panel-mcp-phase-initializing")
            }
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
                // OAuth 待授权：状态图标用 warning 色强调 + name 后加徽标
                let auth_badge = if s.needs_auth {
                    format!(" {}", i18n::tr("panel-mcp-needs-auth"))
                } else {
                    String::new()
                };
                let badge_style = Style::new().fg(theme_def.read().semantic.status.warning);

                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {} ", cursor),
                        Style::new().fg(theme_def.read().component.panel.title),
                    ),
                    Span::styled(s.name.clone(), name_style),
                    Span::styled(auth_badge, badge_style),
                    Span::styled(format!("  {}", status_icon), Style::new().fg(status_color)),
                    Span::styled(format!(" {}", s.status), Style::new().fg(status_color)),
                ]));
                lines.push(Line::from(vec![Span::styled(
                    i18n::tr_args(
                        "panel-mcp-server-detail",
                        &[
                            (
                                "transport".to_string(),
                                FluentValue::from(s.transport.as_str()),
                            ),
                            ("count".to_string(), FluentValue::from(s.tools_count as i64)),
                            (
                                "skills".to_string(),
                                FluentValue::from(
                                    skills_by_server
                                        .get(&mcp_domain_key(&s.name))
                                        .copied()
                                        .unwrap_or(0) as i64,
                                ),
                            ),
                        ],
                    ),
                    Style::new().fg(theme_def.read().semantic.text.dim),
                )]));
                let cache_label = match s.cache_status.as_deref() {
                    Some("hit") => i18n::tr("panel-mcp-cache-hit"),
                    Some("live_fetch") => i18n::tr("panel-mcp-cache-live-fetch"),
                    _ => i18n::tr("panel-mcp-cache-none"),
                };
                lines.push(
                    Line::from(format!("  {}", cache_label)).fg(theme_def.read().semantic.text.dim),
                );
            }
        }

        lines.push(Line::from(""));
        // 底部提示统一为列表进入详情；授权只能在详情页显式选择按钮后触发。
        lines.push(
            Line::from(i18n::tr("panel-mcp-list-hint")).fg(theme_def.read().semantic.text.dim),
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

/// 详情视图按钮行在内容区中的行号（鼠标命中反推）。
/// 内容区行号 0 起：空行、标题、空行、URL 标签、URL×n、空行、按钮行 →
/// 按钮行 = 5 + n（无 URL 时 n = 0）。
fn detail_button_labels(s: &McpServerSummary) -> Vec<String> {
    if s.needs_auth {
        vec![
            i18n::tr("panel-mcp-detail-btn-auth"),
            i18n::tr("panel-mcp-detail-btn-back"),
        ]
    } else {
        vec![i18n::tr("panel-mcp-detail-btn-back")]
    }
}

fn detail_btn_row(s: &McpServerSummary) -> u16 {
    let url_lines = s
        .url
        .as_ref()
        .map(|url| wrap_text(url, 52).len() as u16)
        .unwrap_or(0);
    // 空行 + 标题 + 空行 + [错误标题 + 摘要 + 空行] + URL 标签 + URL×n + 空行。
    5 + url_lines + u16::from(s.error_summary.is_some()) * 3
}

/// 激活详情视图按钮：待授权 server 的 0 = 授权、1 = 返回；其他 server
/// 仅有 0 = 返回。键盘 Enter / 鼠标左键点击共用。
fn activate_detail_btn(
    idx: usize,
    summary: &McpServerSummary,
    view: &ReactiveHandle<McpView, SingleWaker>,
    detail_btn: &ReactiveHandle<usize, SingleWaker>,
) {
    if summary.needs_auth && idx == 0 {
        start_oauth(summary.name.clone());
    }
    *detail_btn.write() = 0;
    *view.write() = McpView::List;
}

/// 构造详情视图内容行（纯函数，测试友好）。
fn build_detail_lines(
    s: &McpServerSummary,
    btn_sel: usize,
    theme: &peri_theme::theme::ThemeDefinition,
) -> Vec<Line<'static>> {
    let semantic = &theme.semantic;
    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(Line::from(""));
    // 标题：server 名 + 状态
    let (status_icon, status_color) = derive_status_style(&s.status);
    lines.push(Line::from(vec![
        Span::styled(
            format!("  {}", s.name),
            Style::new().fg(theme.component.panel.title).bold(),
        ),
        Span::styled(format!("  {}", status_icon), Style::new().fg(status_color)),
        Span::styled(format!(" {}", s.status), Style::new().fg(status_color)),
        Span::styled(
            if s.needs_auth {
                format!("  {}", i18n::tr("panel-mcp-needs-auth"))
            } else {
                String::new()
            },
            Style::new().fg(semantic.status.warning),
        ),
    ]));
    lines.push(Line::from(""));
    if let Some(error) = &s.error_summary {
        lines.push(
            Line::from(format!("  {}", i18n::tr("panel-mcp-detail-error")))
                .fg(semantic.status.error),
        );
        lines.push(Line::from(format!("    {error}")).fg(semantic.text.muted));
        lines.push(Line::from(""));
    }
    // URL（HTTP 传输）完整换行展示
    lines.push(Line::from(format!("  {}", i18n::tr("panel-mcp-detail-url"))).fg(semantic.text.dim));
    match &s.url {
        Some(url) => {
            for url_line in wrap_text(url, 52) {
                lines.push(Line::from(format!("    {url_line}")).fg(semantic.text.primary));
            }
        }
        None => {
            lines.push(Line::from(format!("    ({})", i18n::tr("ui-empty"))).fg(semantic.text.dim));
        }
    }
    lines.push(Line::from(""));
    // 按钮行：仅 needs_auth server 有 [授权]；其他详情仅显示 [返回]。
    let labels = detail_button_labels(s);
    let sel_bg = semantic.surface.selection;
    let mut btn_line: Vec<Span<'_>> = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        if i > 0 {
            btn_line.push(Span::raw("  "));
        }
        let text = format!("[ {label} ]");
        let style = if i == btn_sel {
            Style::new()
                .fg(semantic.text.primary)
                .bg(sel_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(semantic.text.primary)
        };
        btn_line.push(Span::styled(text, style));
    }
    lines.push(Line::from(btn_line));
    lines.push(Line::from(""));
    lines.push(Line::from(i18n::tr("panel-mcp-detail-hint")).fg(semantic.text.dim));
    lines
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

/// 发起 OAuth 授权（详情视图 [ 授权 ] 按钮）：`mcp/oauth_start` RPC → host pool
/// spawn_oauth_flow → OauthNeeded 事件 → TUI 弹出授权 popup。
fn start_oauth(server_name: String) {
    if let Some(client_handle) = ACP_CLIENT_HANDLE.get() {
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
