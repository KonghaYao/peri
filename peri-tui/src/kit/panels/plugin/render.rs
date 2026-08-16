use crate::i18n;
use crate::kit::atoms::{PLUGIN_LIST, PluginSummary};
use fluent_bundle::FluentValue;
use ratatui_kit::ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use super::data::get_marketplace_cache;
use super::{
    DiscoverDetailAction, MsStatus, PluginSearchResultItem, SearchState, action_label, action_list,
};

// ── Render: Installed list ────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(super) fn render_installed(
    lines: &mut Vec<Line<'_>>,
    plugins: &[PluginSummary],
    sel: usize,
    scroll_start: usize,
    visible: usize,
    count: usize,
    bold_style: Style,
    muted_style: Style,
    dim_style: Style,
    primary_style: Style,
    title_style: Style,
    error_style: Style,
    success_color: Color,
    title_color: Color,
) {
    lines.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            "panel-plugin-stats",
            &[("count".into(), FluentValue::from(count as i64))],
        ),
        bold_style,
    )]));
    lines.push(Line::from(""));

    if plugins.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-plugin-empty"),
            muted_style,
        )]));
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-plugin-empty-hint"),
            muted_style,
        )]));
    } else {
        for (i, p) in plugins.iter().enumerate().skip(scroll_start).take(visible) {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                title_style
            } else {
                primary_style
            };

            // Status icon
            let (icon, icon_color) = if p.load_error.is_some() {
                ("✗", error_style.fg.unwrap_or_default())
            } else if !p.enabled {
                ("◯", muted_style.fg.unwrap_or_default())
            } else {
                ("✓", success_color)
            };

            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", cursor), Style::new().fg(title_color)),
                Span::styled(format!("{} ", icon), Style::new().fg(icon_color)),
                Span::styled(p.name.clone(), name_style),
                Span::styled(
                    format!(
                        " v{}",
                        if p.version.is_empty() {
                            i18n::tr("panel-plugin-version-unknown")
                        } else {
                            p.version.clone()
                        }
                    ),
                    muted_style,
                ),
            ]));
            if !p.description.is_empty() {
                lines.push(Line::from(vec![Span::styled(
                    format!("     {}", p.description),
                    dim_style,
                )]));
            } else {
                lines.push(Line::from(""));
            }
            let root: String = p.root.chars().take(72).collect();
            lines.push(Line::from(vec![Span::styled(
                format!("     {}", root),
                dim_style,
            )]));
            // extras with i18n labels
            let mut extras: Vec<String> = Vec::new();
            if p.skills_count > 0 {
                extras.push(format!(
                    "{}:{}",
                    i18n::tr("panel-plugin-field-skills"),
                    p.skills_count
                ));
            }
            if p.commands_count > 0 {
                extras.push(format!(
                    "{}:{}",
                    i18n::tr("panel-plugin-field-commands"),
                    p.commands_count
                ));
            }
            if p.agents_count > 0 {
                extras.push(format!(
                    "{}:{}",
                    i18n::tr("panel-plugin-field-agents"),
                    p.agents_count
                ));
            }
            if p.mcp_count > 0 {
                extras.push(format!(
                    "{}:{}",
                    i18n::tr("panel-plugin-field-mcp"),
                    p.mcp_count
                ));
            }
            let extra = if extras.is_empty() {
                String::from("—")
            } else {
                extras.join(" · ")
            };
            lines.push(Line::from(vec![Span::styled(
                format!("     {}", extra),
                dim_style,
            )]));
            lines.push(Line::from(""));
        }
    }
}

// ── Render: Detail + Actions ─────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(super) fn render_detail(
    lines: &mut Vec<Line<'_>>,
    p: &PluginSummary,
    action_index: usize,
    bold_style: Style,
    muted_style: Style,
    dim_style: Style,
    primary_style: Style,
    error_style: Style,
    success_color: Color,
    title_color: Color,
    title_style: Style,
    confirm_action_text: Option<&str>,
    warning_color: Color,
) {
    // Title
    lines.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            "panel-plugin-detail-title",
            &[("name".into(), FluentValue::from(p.name.clone()))],
        ),
        bold_style,
    )]));
    lines.push(Line::from(""));

    // Status
    let (status_text, status_color) = if p.load_error.is_some() {
        (
            format!("  ✗ {}", i18n::tr("panel-plugin-detail-error")),
            error_style.fg.unwrap_or_default(),
        )
    } else if !p.enabled {
        (
            format!("  ◯ {}", i18n::tr("panel-plugin-status-disabled")),
            muted_style.fg.unwrap_or_default(),
        )
    } else {
        (
            format!("  ✓ {}", i18n::tr("panel-plugin-status-enabled")),
            success_color,
        )
    };
    lines.push(Line::from(vec![Span::styled(
        status_text,
        Style::new().fg(status_color),
    )]));
    lines.push(Line::from(""));

    // Fields
    let fields: [(&str, &dyn Fn() -> String); 4] = [
        ("panel-plugin-detail-marketplace", &|| p.marketplace.clone()),
        ("panel-plugin-detail-author", &|| {
            p.author.clone().unwrap_or_else(|| "—".to_string())
        }),
        ("panel-plugin-detail-path", &|| p.root.clone()),
        ("panel-plugin-detail-scope", &|| p.install_scope.clone()),
    ];
    for (label_key, get_value) in &fields {
        lines.push(Line::from(vec![
            Span::styled(format!("    {}: ", i18n::tr(label_key)), muted_style),
            Span::styled(get_value(), dim_style),
        ]));
    }
    lines.push(Line::from(""));

    // Capabilities
    let caps: [(&str, usize); 4] = [
        ("panel-plugin-field-skills", p.skills_count),
        ("panel-plugin-field-commands", p.commands_count),
        ("panel-plugin-field-agents", p.agents_count),
        ("panel-plugin-field-mcp", p.mcp_count),
    ];
    for (label_key, count) in &caps {
        let value = if *count > 0 {
            count.to_string()
        } else {
            "—".to_string()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("    {}: ", i18n::tr(label_key)), muted_style),
            Span::styled(value, dim_style),
        ]));
    }

    // Load error
    if let Some(ref err) = p.load_error {
        lines.push(Line::from(""));
        let err_text: String = err.chars().take(72).collect();
        lines.push(Line::from(vec![Span::styled(
            format!("    {}", err_text),
            error_style,
        )]));
    }

    // Action menu
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        format!("  {}", i18n::tr("panel-plugin-detail-actions")),
        bold_style,
    )]));
    lines.push(Line::from(""));

    // Confirm hint (if in confirm mode)
    if let Some(action) = confirm_action_text {
        let confirm_key = match action {
            "uninstall" => "panel-plugin-confirm-uninstall",
            "delete_marketplace" => "panel-plugin-confirm-delete-mp",
            _ => "panel-plugin-confirm-uninstall",
        };
        lines.push(Line::from(vec![Span::styled(
            format!("  {}", i18n::tr(confirm_key)),
            Style::new().fg(warning_color),
        )]));
        lines.push(Line::from(""));
    }

    let actions = action_list(p.enabled);
    for (i, action) in actions.iter().enumerate() {
        let is_selected = i == action_index;
        let cursor = if is_selected { ">" } else { " " };
        let style = if is_selected {
            title_style
        } else {
            primary_style
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", cursor), Style::new().fg(title_color)),
            Span::styled(format!("    {}", action_label(action)), style),
        ]));
    }
}

// ── Render: Discover List ─────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(super) fn render_discover_list(
    lines: &mut Vec<Line<'_>>,
    search_text: &str,
    show_cursor: bool,
    search_state: &SearchState,
    items: &[&PluginSearchResultItem],
    sel: usize,
    scroll_start: usize,
    visible: usize,
    bold_style: Style,
    muted_style: Style,
    dim_style: Style,
    primary_style: Style,
    _error_style: Style,
    _success_color: Color,
    title_color: Color,
    title_style: Style,
) {
    // Early return for search state
    match search_state {
        SearchState::Loading => {
            lines.push(Line::from(vec![Span::styled("  Discover", bold_style)]));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("panel-plugin-search-loading"),
                muted_style,
            )]));
            return;
        }
        SearchState::Error(msg) => {
            lines.push(Line::from(vec![Span::styled("  Discover", bold_style)]));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                i18n::tr_args(
                    "panel-plugin-search-error",
                    &[("error".into(), FluentValue::from(msg.clone()))],
                ),
                _error_style,
            )]));
            return;
        }
        SearchState::Idle => {}
    }

    lines.push(Line::from(vec![Span::styled("  Discover", bold_style)]));
    lines.push(Line::from(""));

    // Search/filter box — simplified single-line input style
    {
        let display: String = search_text.chars().collect();
        let cursor = if show_cursor { "▌" } else { " " };
        let placeholder = if display.is_empty() {
            i18n::tr("panel-plugin-discover-input")
        } else {
            "".to_string()
        };
        lines.push(Line::from(vec![Span::styled(
            format!("  > {}{}{}", display, cursor, placeholder),
            muted_style,
        )]));
    }
    lines.push(Line::from(""));

    if items.is_empty() {
        if search_text.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("panel-plugin-discover-empty"),
                muted_style,
            )]));
        } else {
            lines.push(Line::from(vec![Span::styled(
                i18n::tr("panel-plugin-search-no-results"),
                muted_style,
            )]));
        }
    } else {
        for (i, item) in items.iter().enumerate().skip(scroll_start).take(visible) {
            let is_selected = i == sel;
            let cursor = if is_selected { ">" } else { " " };
            let name_style = if is_selected {
                title_style
            } else {
                primary_style
            };

            // 已安装标记
            let installed_mark = {
                let store = PLUGIN_LIST.state();
                let installed_guard = store.read();
                let installed_ids: std::collections::HashSet<&str> =
                    installed_guard.iter().map(|p| p.name.as_str()).collect();
                if installed_ids.contains(item.name.as_str()) {
                    " \u{2713}"
                } else {
                    ""
                }
            };

            // Name + version + marketplace
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", cursor), Style::new().fg(title_color)),
                Span::styled(format!("{} v{}  ", item.name, item.version), name_style),
                Span::styled(
                    format!("({}){}", item.marketplace, installed_mark),
                    dim_style,
                ),
            ]));

            // Description (truncated)
            let desc: String = if item.description.is_empty() {
                "—".into()
            } else {
                item.description.chars().take(60).collect()
            };
            lines.push(Line::from(vec![Span::styled(
                format!("    {}", desc),
                dim_style,
            )]));
            lines.push(Line::from(""));
        }
    }
}

// ── Render: Discover Detail ───────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(super) fn render_discover_detail(
    lines: &mut Vec<Line<'_>>,
    dp: &PluginSearchResultItem,
    action_cursor: usize,
    bold_style: Style,
    muted_style: Style,
    dim_style: Style,
    primary_style: Style,
    _success_color: Color,
    title_color: Color,
    title_style: Style,
) {
    // Title
    lines.push(Line::from(vec![Span::styled(
        i18n::tr_args(
            "panel-plugin-detail-title",
            &[("name".into(), FluentValue::from(dp.name.clone()))],
        ),
        bold_style,
    )]));
    lines.push(Line::from(""));

    // Fields
    {
        let fields: [(&str, &str); 4] = [
            ("panel-plugin-discover-field-version", &dp.version),
            ("panel-plugin-discover-field-marketplace", &dp.marketplace),
            (
                "panel-plugin-discover-field-author",
                dp.author.as_deref().unwrap_or("—"),
            ),
            (
                "panel-plugin-discover-field-description",
                if dp.description.is_empty() {
                    "—"
                } else {
                    &dp.description
                },
            ),
        ];
        for (label_key, value) in &fields {
            let truncated: String = value.chars().take(60).collect();
            lines.push(Line::from(vec![
                Span::styled(format!("    {}: ", i18n::tr(label_key)), muted_style),
                Span::styled(truncated, dim_style),
            ]));
        }
    }
    lines.push(Line::from(""));

    // Action menu
    lines.push(Line::from(vec![Span::styled(
        format!("  {}", i18n::tr("panel-plugin-detail-actions")),
        bold_style,
    )]));
    lines.push(Line::from(""));

    for (i, action) in DiscoverDetailAction::ALL.iter().enumerate() {
        let is_selected = i == action_cursor;
        let cursor = if is_selected { ">" } else { " " };
        let style = if is_selected {
            title_style
        } else {
            primary_style
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", cursor), Style::new().fg(title_color)),
            Span::styled(format!("    {}", action.label()), style),
        ]));
    }
}

// ── Obsolete: Render Discover (search box only) ────────────────────────

// ── Render: Marketplaces ─────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(super) fn render_marketplaces(
    lines: &mut Vec<Line<'_>>,
    sel: usize,
    bold_style: Style,
    muted_style: Style,
    success_color: Color,
    warning_color: Color,
    error_style: Style,
    refreshing: bool,
    add_active: bool,
    add_input: &str,
    confirm_action_text: Option<&str>,
    operation_name: Option<&str>,
) {
    // Show confirmation dialog for marketplace delete
    if let Some(action) = confirm_action_text
        && action == "delete_marketplace"
    {
        let name = operation_name.unwrap_or_default();
        lines.push(Line::from(vec![Span::styled(
            format!("  {}: {}", i18n::tr("panel-plugin-confirm-delete-mp"), name),
            Style::new().fg(warning_color),
        )]));
        lines.push(Line::from(""));
        return;
    }

    lines.push(Line::from(vec![Span::styled("  Marketplaces", bold_style)]));
    lines.push(Line::from(""));

    let entries = get_marketplace_cache();
    let dim_color = muted_style.fg.unwrap_or_default();

    // Add Marketplace 行 (item 0)
    if add_active {
        let display: String = add_input.chars().take(40).collect();
        lines.push(Line::from(vec![
            Span::styled("  > ", bold_style),
            Span::styled(
                format!(
                    "{} {}",
                    i18n::tr("panel-plugin-marketplace-add-label"),
                    display
                ),
                bold_style,
            ),
        ]));
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-plugin-marketplace-add-url-hint"),
            muted_style,
        )]));
    } else {
        let is_sel = sel == 0;
        let cursor = if is_sel { ">" } else { " " };
        let style = if is_sel { bold_style } else { muted_style };
        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", cursor), style),
            Span::styled(
                format!("+ {}", i18n::tr("panel-plugin-marketplaces-add")),
                style,
            ),
        ]));
    }
    lines.push(Line::from(""));

    // Marketplace 条目
    for (i, entry) in entries.iter().enumerate() {
        let item_idx = i + 1;
        let is_selected = sel == item_idx;
        let cursor = if is_selected { ">" } else { " " };
        let name_style = if is_selected { bold_style } else { muted_style };

        // 状态图标
        let (icon, icon_color) = match entry.status {
            MsStatus::Fresh => ("●", success_color),
            MsStatus::Cached => ("●", success_color),
            MsStatus::Fetching => ("◌", warning_color),
            MsStatus::Stale => ("○", dim_color),
            MsStatus::Failed => ("✗", error_style.fg.unwrap_or_default()),
            MsStatus::NotFound => ("○", dim_color),
        };
        let status_text = match entry.status {
            MsStatus::Fresh => "fresh",
            MsStatus::Cached => "cached",
            MsStatus::Fetching => "fetching",
            MsStatus::Stale => "stale",
            MsStatus::Failed => "failed",
            MsStatus::NotFound => "not fetched",
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", cursor), name_style),
            Span::styled(format!("{} ", icon), Style::new().fg(icon_color)),
            Span::styled(format!("{} ", entry.name), name_style),
            Span::styled(format!("({})", status_text), muted_style),
        ]));

        // 第二行: source + stats
        let stats = format!(
            "{}: {}  |  plugins: {}  |  installed: {}",
            if entry.auto_update { "auto" } else { "manual" },
            entry.source_label.chars().take(30).collect::<String>(),
            entry.plugin_count,
            entry.installed_count,
        );
        lines.push(Line::from(vec![Span::styled(
            format!("     {}", stats),
            Style::new().fg(dim_color),
        )]));
        lines.push(Line::from(""));
    }

    // Footer hints
    if refreshing {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-plugin-marketplace-refreshing"),
            Style::new().fg(warning_color),
        )]));
    } else {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-plugin-marketplace-hint-keys"),
            muted_style,
        )]));
    }
}

// ── Render: Errors ────────────────────────────────────────────────────

pub(super) fn render_errors(
    lines: &mut Vec<Line<'_>>,
    plugins: &[PluginSummary],
    bold_style: Style,
    muted_style: Style,
    dim_style: Style,
    error_style: Style,
) {
    let errors: Vec<&PluginSummary> = plugins.iter().filter(|p| p.load_error.is_some()).collect();
    lines.push(Line::from(vec![Span::styled(
        format!(
            "  {} ({})",
            i18n::tr("panel-plugin-errors-title"),
            errors.len()
        ),
        bold_style,
    )]));
    lines.push(Line::from(""));

    if errors.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            i18n::tr("panel-plugin-errors-empty"),
            muted_style,
        )]));
    } else {
        for p in &errors {
            lines.push(Line::from(vec![Span::styled(
                format!("  ✗ {} v{}", p.name, p.version),
                error_style,
            )]));
            if let Some(ref err) = p.load_error {
                let err_text: String = err.chars().take(72).collect();
                lines.push(Line::from(vec![Span::styled(
                    format!("      {}", err_text),
                    dim_style,
                )]));
            }
            lines.push(Line::from(""));
        }
    }
}
