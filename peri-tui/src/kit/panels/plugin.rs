//! ratatui-kit PluginPanel v2 component.
//!
//! v2 Phase 2: 4-tab multi-view with dual-mode (list/detail) state machine.
//! ←/→ 切换视图，↑/↓ 导航，Enter 进详情/执行操作，Esc 返回列表。
//! 无 ScrollView——避免其内置 handler 与自定义 ↑/↓ 冲突。

use crate::app::panel_types::PanelKind;
use crate::components::textarea::TextAreaState;
use crate::i18n;
use crate::kit::atoms::{
    ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, LANG_VERSION, PLUGIN_LIST, PluginSummary,
    PluginViewTab, RENDER_HEARTBEAT,
};
use crate::kit::list_nav::scroll_start_for_selected;
use crate::kit::panel_mouse::AreaTracker;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::{
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::Paragraph,
};

mod data;
mod discover;
mod discover_handler;
use discover::{DiscoverState, SearchSession};
mod panel_handler;
mod render;
mod search_handler;
mod search_request;

#[cfg(test)]
#[path = "plugin/search_request_test.rs"]
mod search_request_tests;

use self::data::{get_discover_cache, get_marketplace_cache};
use self::render::{
    render_detail, render_discover_detail, render_discover_list, render_errors, render_installed,
    render_marketplaces,
};

// ── Search state machine ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum SearchState {
    Idle,
    Loading,
    Error(String),
}

// ── Discover detail actions ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscoverDetailAction {
    InstallUser,
    InstallProject,
    BackToList,
}

impl DiscoverDetailAction {
    const ALL: [DiscoverDetailAction; 3] = [
        DiscoverDetailAction::InstallUser,
        DiscoverDetailAction::InstallProject,
        DiscoverDetailAction::BackToList,
    ];

    fn label(&self) -> String {
        match self {
            Self::InstallUser => i18n::tr("panel-plugin-discover-install-user"),
            Self::InstallProject => i18n::tr("panel-plugin-discover-install-project"),
            Self::BackToList => i18n::tr("panel-plugin-action-back"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
struct PluginSearchResultItem {
    name: String,
    version: String,
    marketplace: String,
    description: String,
    author: Option<String>,
}

// ── Marketplace view types ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
enum MsStatus {
    Fresh,
    Cached,
    Fetching,
    Stale,
    Failed,
    NotFound,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct MsEntry {
    name: String,
    source_label: String,
    plugin_count: usize,
    installed_count: usize,
    status: MsStatus,
    last_updated: Option<String>,
    auto_update: bool,
}

// ── Tab switching ──────────────────────────────────────────────────────

fn cycle_forward(t: PluginViewTab) -> PluginViewTab {
    match t {
        PluginViewTab::Installed => PluginViewTab::Discover,
        PluginViewTab::Discover => PluginViewTab::Marketplaces,
        PluginViewTab::Marketplaces => PluginViewTab::Errors,
        PluginViewTab::Errors => PluginViewTab::Installed,
    }
}

fn cycle_backward(t: PluginViewTab) -> PluginViewTab {
    match t {
        PluginViewTab::Installed => PluginViewTab::Errors,
        PluginViewTab::Errors => PluginViewTab::Marketplaces,
        PluginViewTab::Marketplaces => PluginViewTab::Discover,
        PluginViewTab::Discover => PluginViewTab::Installed,
    }
}

const VISIBLE_ITEMS: usize = 5;

// ── Component ──────────────────────────────────────────────────────

#[component]
pub fn PluginPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme_def = hooks.use_atom(&THEME_ATOM);
    let selected = hooks.use_state(|| 0usize);
    let active_tab = hooks.use_state(|| PluginViewTab::Installed);
    let detail_plugin_idx = hooks.use_state(|| Option::<usize>::None);
    let action_index = hooks.use_state(|| 0usize);
    let confirm_action = hooks.use_state(|| Option::<String>::None);
    let cursor_visible = hooks.use_state(|| true);
    let cursor_last_toggle = hooks.use_state(std::time::Instant::now);
    let operation_loading = hooks.use_state(|| Option::<String>::None);
    let add_marketplace_input = hooks.use_state(TextAreaState::default);
    let add_marketplace_active = hooks.use_state(|| false);
    let marketplace_refreshing = hooks.use_state(|| false);
    // Marketplace detail state
    let marketplace_detail = hooks.use_state(|| Option::<usize>::None);
    let marketplace_detail_action = hooks.use_state(|| 0usize);
    let discover = hooks.use_state(DiscoverState::default);
    let store = hooks.use_atom(&PLUGIN_LIST);
    let plugins: Vec<PluginSummary> = store.read().clone();
    hooks.use_atom(&LANG_VERSION);
    hooks.use_atom(&ACTIVE_SESSION_ID);
    hooks.use_atom(&BRIDGE_RESET_COUNTER);
    discover
        .write_no_update()
        .reset_session(SearchSession::current());
    let count = plugins.len();

    // RENDER_HEARTBEAT: 驱动 cursor blink（Discover 搜索框）
    {
        let _hb = hooks.use_atom(&RENDER_HEARTBEAT);
    }

    // 面板绘制区域（上一帧）——鼠标点击行号反推
    let area;
    {
        let tracker = hooks.use_hook(AreaTracker::new);
        area = tracker.rect;
    }

    // High priority — 搜索框文本输入（仅 Discover tab 生效，先于 Normal handler）
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::High,
        EventOptions { hit_test: true },
        move |event| {
            search_handler::handle_search_event(
                event,
                area,
                active_tab,
                discover,
                detail_plugin_idx,
                marketplace_detail,
                marketplace_detail_action,
                confirm_action,
                operation_loading,
                add_marketplace_input,
                add_marketplace_active,
            )
        },
    );

    // ── 键盘 ──
    hooks.use_event_handler_with_options(
        EventScope::Current,
        EventPriority::Normal,
        EventOptions { hit_test: true },
        move |event| {
            panel_handler::handle_panel_event(
                event,
                area,
                selected,
                active_tab,
                discover,
                action_index,
                confirm_action,
                operation_loading,
                detail_plugin_idx,
                marketplace_detail,
                marketplace_detail_action,
                marketplace_refreshing,
                add_marketplace_input,
                add_marketplace_active,
            )
        },
    );

    // ── 构建行 ──
    let sel = *selected.read();
    let current_tab = *active_tab.read();
    let detail_idx = *detail_plugin_idx.read();
    let ai = *action_index.read();
    let scroll_start = scroll_start_for_selected(sel, count, VISIBLE_ITEMS);

    // cursor blink toggle for Discover search box
    let show_cursor = {
        let now = std::time::Instant::now();
        let mut last = cursor_last_toggle.write_no_update();
        if now.duration_since(*last).as_millis() >= 500 {
            let mut v = cursor_visible.write_no_update();
            *v = !*v;
            *last = now;
        }
        *cursor_visible.read()
    };

    let guard = theme_def.read();
    let title_style = Style::new().fg(guard.component.panel.title).bold();
    let primary_style = Style::new().fg(guard.semantic.text.primary);
    let bold_style = Style::new().fg(guard.semantic.text.primary).bold();
    let muted_style = Style::new().fg(guard.semantic.text.muted);
    let dim_style = Style::new().fg(guard.semantic.text.dim);
    let error_style = Style::new().fg(guard.semantic.status.error);
    let warning_color = guard.semantic.status.warning;
    let success_color = guard.semantic.status.success;
    let title_color = guard.component.panel.title;
    let active_tab_style = Style::new()
        .fg(guard.semantic.text.primary)
        .bg(title_color)
        .bold();
    let inactive_tab_style = muted_style;

    let mut lines: Vec<Line<'_>> = Vec::new();

    // ── tab bar ──
    lines.push(Line::from(vec![
        Span::styled(
            format!("  {}  ", i18n::tr("panel-plugin-tab-installed")),
            if current_tab == PluginViewTab::Installed {
                active_tab_style
            } else {
                inactive_tab_style
            },
        ),
        Span::styled(
            format!("  {}  ", i18n::tr("panel-plugin-tab-discover")),
            if current_tab == PluginViewTab::Discover {
                active_tab_style
            } else {
                inactive_tab_style
            },
        ),
        Span::styled(
            format!("  {}  ", i18n::tr("panel-plugin-tab-marketplaces")),
            if current_tab == PluginViewTab::Marketplaces {
                active_tab_style
            } else {
                inactive_tab_style
            },
        ),
        Span::styled(
            format!("  {}  ", i18n::tr("panel-plugin-tab-errors")),
            if current_tab == PluginViewTab::Errors {
                active_tab_style
            } else {
                inactive_tab_style
            },
        ),
    ]));

    // ── Content ──
    // 检查 discover detail 模式
    let discover_read = discover.read();
    if let Some(dp) = discover_read.detail.as_ref() {
        render_discover_detail(
            &mut lines,
            dp,
            discover_read.detail_action,
            bold_style,
            muted_style,
            dim_style,
            primary_style,
            success_color,
            title_color,
            title_style,
        );
    } else if let Some(mp_sel) = *marketplace_detail.read() {
        let entries = get_marketplace_cache();
        if let Some(entry) = entries.get(mp_sel.saturating_sub(1)) {
            let ma = *marketplace_detail_action.read();
            // Title
            lines.push(Line::from(vec![Span::styled(
                i18n::tr_args(
                    "panel-plugin-detail-title",
                    &[("name".into(), FluentValue::from(entry.name.clone()))],
                ),
                bold_style,
            )]));
            lines.push(Line::from(""));
            // Source
            let source_display: String = entry.source_label.chars().take(60).collect();
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "    {}: ",
                        i18n::tr("panel-plugin-discover-field-marketplace")
                    ),
                    muted_style,
                ),
                Span::styled(source_display, dim_style),
            ]));
            lines.push(Line::from(""));
            // Actions
            lines.push(Line::from(vec![Span::styled(
                format!("  {}", i18n::tr("panel-plugin-detail-actions")),
                bold_style,
            )]));
            lines.push(Line::from(""));
            let actions: [String; 2] = [
                i18n::tr("panel-plugin-marketplace-action-refresh"),
                i18n::tr("panel-plugin-marketplace-action-delete"),
            ];
            for (i, action_label) in actions.iter().enumerate() {
                let is_selected = i == ma;
                let cursor = if is_selected { ">" } else { " " };
                let style = if is_selected {
                    title_style
                } else {
                    primary_style
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{} ", cursor), Style::new().fg(title_color)),
                    Span::styled(format!("    {}", action_label), style),
                ]));
            }
        }
    } else if let Some(di) = detail_idx {
        if let Some(p) = plugins.get(di) {
            let confirm_text = confirm_action.read().clone();
            render_detail(
                &mut lines,
                p,
                ai,
                bold_style,
                muted_style,
                dim_style,
                primary_style,
                error_style,
                success_color,
                title_color,
                title_style,
                confirm_text.as_deref(),
                warning_color,
            );
        }
    } else {
        match current_tab {
            PluginViewTab::Installed => render_installed(
                &mut lines,
                &plugins,
                sel,
                scroll_start,
                VISIBLE_ITEMS,
                count,
                bold_style,
                muted_style,
                dim_style,
                primary_style,
                title_style,
                error_style,
                success_color,
                title_color,
            ),
            PluginViewTab::Discover => {
                let local = get_discover_cache();
                let items = discover_read.visible_items(&local);
                let scroll =
                    scroll_start_for_selected(discover_read.selected, items.len(), VISIBLE_ITEMS);
                render_discover_list(
                    &mut lines,
                    &discover_read.editor.text,
                    show_cursor && discover_read.focused,
                    &discover_read.status,
                    &items,
                    discover_read.selected,
                    scroll,
                    VISIBLE_ITEMS,
                    bold_style,
                    muted_style,
                    dim_style,
                    primary_style,
                    error_style,
                    success_color,
                    title_color,
                    title_style,
                )
            }
            PluginViewTab::Marketplaces => render_marketplaces(
                &mut lines,
                sel,
                bold_style,
                muted_style,
                success_color,
                warning_color,
                error_style,
                *marketplace_refreshing.read(),
                *add_marketplace_active.read(),
                add_marketplace_input.read().text.as_str(),
                confirm_action.read().clone().as_deref(),
                operation_loading.read().clone().as_deref(),
            ),
            PluginViewTab::Errors => render_errors(
                &mut lines,
                &plugins,
                bold_style,
                muted_style,
                dim_style,
                error_style,
            ),
        }
    }

    // ── footer ──
    let footer_text = if *add_marketplace_active.read() {
        i18n::tr("panel-plugin-marketplace-add-input-footer").to_string()
    } else if let Some(ref op) = *operation_loading.read() {
        match op.as_str() {
            "uninstall" => format!("{}...", i18n::tr("panel-plugin-action-uninstall")),
            "enable" => format!("{}...", i18n::tr("panel-plugin-action-enable")),
            "disable" => format!("{}...", i18n::tr("panel-plugin-action-disable")),
            "install" => format!("{}...", i18n::tr("panel-plugin-action-install")),
            "update" => format!("{}...", i18n::tr("panel-plugin-action-update")),
            _ => format!("{}...", op),
        }
    } else if confirm_action.read().is_some() {
        i18n::tr("panel-plugin-confirm-hint")
    } else if discover_read.detail.is_some() {
        i18n::tr("common-nav-enter-close")
    } else if detail_idx.is_some() {
        i18n::tr("common-nav-enter-close")
    } else if marketplace_detail.read().is_some() {
        i18n::tr("panel-plugin-marketplace-detail-hint")
    } else {
        i18n::tr("common-nav-tab-close")
    };
    lines.push(Line::from(vec![Span::styled(footer_text, muted_style)]));

    drop(guard);

    let content = Paragraph::new(ratatui::text::Text::from(lines));

    panel_shell!(PanelKind::Plugin, {
        Text(text: content)
    })
}

// ── Action list ──────────────────────────────────────────────────────

fn action_list(enabled: bool) -> Vec<&'static str> {
    let mut actions = Vec::new();
    if enabled {
        actions.push("disable");
    } else {
        actions.push("enable");
    }
    actions.push("uninstall");
    actions.push("update");
    actions.push("back");
    actions
}

fn action_label(action: &str) -> String {
    match action {
        "disable" => i18n::tr("panel-plugin-action-disable"),
        "enable" => i18n::tr("panel-plugin-action-enable"),
        "uninstall" => i18n::tr("panel-plugin-action-uninstall"),
        "update" => i18n::tr("panel-plugin-action-update"),
        "back" => i18n::tr("panel-plugin-action-back"),
        _ => action.to_string(),
    }
}

fn close_panel() {
    crate::kit::panel_registry::close_active_panel();
}
