//! Shell chrome: header, sidebar, banner and footer.
//!
//! Layout is computed once per frame and returned as [`ShellLayout`]. Rendering
//! and hit registration both consume that value, so mouse routing always sees
//! the geometry of the frame the user clicked.

use peri_tui_components::button::ButtonTone;
use peri_tui_components::scrollbar::{bounded_offset, VerticalBar};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{
    ActionId, App, BannerActions, BannerTone, BannerView, Focus, HealthState, Page, SidebarGroup,
    NAV_ENTRIES, NAV_ROW_COUNT,
};
use crate::ui::hit::{HitMap, Layer, ScrollZone};
use crate::ui::text;
use crate::ui::widgets::{self, StatusTone};

/// Minimum usable width.
pub const MIN_WIDTH: u16 = 50;
/// Minimum usable height.
pub const MIN_HEIGHT: u16 = 12;
/// Sidebar width at `width >= 120`.
pub const SIDEBAR_WIDE: u16 = 26;
/// Sidebar width at `90..=119`.
pub const SIDEBAR_MEDIUM: u16 = 24;
/// Sidebar width at `80..=89`.
pub const SIDEBAR_NARROW: u16 = 22;
/// Width below which the sidebar becomes an overlay menu.
pub const SIDEBAR_HIDDEN_BELOW: u16 = 80;
/// One header row.
pub const HEADER_HEIGHT: u16 = 1;
/// One footer row.
pub const FOOTER_HEIGHT: u16 = 1;
/// Fixed sidebar rows for the pinned bottom actions and their divider.
const SIDEBAR_PINNED_ROWS: u16 = 3;

/// Sidebar width for a terminal width, or `None` when it is behind the menu.
#[must_use]
pub fn sidebar_width(width: u16) -> Option<u16> {
    if width >= 120 {
        Some(SIDEBAR_WIDE)
    } else if width >= 90 {
        Some(SIDEBAR_MEDIUM)
    } else if width >= SIDEBAR_HIDDEN_BELOW {
        Some(SIDEBAR_NARROW)
    } else {
        None
    }
}

/// Whether the terminal is too small to draw the shell.
#[must_use]
pub fn too_small(area: Rect) -> bool {
    area.width < MIN_WIDTH || area.height < MIN_HEIGHT
}

/// Geometry of one frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShellLayout {
    /// Header row.
    pub header: Rect,
    /// Optional banner row under the header.
    pub banner: Option<Rect>,
    /// Sidebar column, absent when the menu button replaces it.
    pub sidebar: Option<Rect>,
    /// Sidebar rows available for navigation (scroll viewport).
    pub sidebar_list: Option<Rect>,
    /// Pinned action rows: "Show getting started" then "Sign out".
    pub sidebar_actions: Option<[Rect; 2]>,
    /// Page content column.
    pub body: Rect,
    /// Footer row.
    pub footer: Rect,
    /// Clickable menu button when the sidebar is hidden.
    pub menu_button: Option<Rect>,
}

impl ShellLayout {
    /// Compute the shell geometry for `area` and the current banner.
    #[must_use]
    pub fn compute(area: Rect, app: &App) -> Self {
        let banner_height = u16::from(app.active_banner().is_some());
        let middle_height = area
            .height
            .saturating_sub(HEADER_HEIGHT + banner_height + FOOTER_HEIGHT);
        let header = Rect::new(area.x, area.y, area.width, HEADER_HEIGHT);
        let banner = if banner_height == 1 {
            Some(Rect::new(
                area.x,
                header.bottom(),
                area.width,
                banner_height,
            ))
        } else {
            None
        };
        let middle_y = header.bottom().saturating_add(banner_height);
        let middle = Rect::new(area.x, middle_y, area.width, middle_height);
        let footer = Rect::new(area.x, middle.bottom(), area.width, FOOTER_HEIGHT);

        let (sidebar, body) = match sidebar_width(area.width) {
            Some(width) => {
                let width = width.min(middle.width);
                (
                    Some(Rect::new(middle.x, middle.y, width, middle.height)),
                    Rect::new(
                        middle.x.saturating_add(width),
                        middle.y,
                        middle.width.saturating_sub(width),
                        middle.height,
                    ),
                )
            }
            None => (None, middle),
        };

        let (sidebar_list, sidebar_actions) = match sidebar {
            Some(sidebar) if sidebar.height > SIDEBAR_PINNED_ROWS => {
                let list_height = sidebar.height - SIDEBAR_PINNED_ROWS;
                let list = Rect::new(
                    sidebar.x.saturating_add(1),
                    sidebar.y,
                    sidebar.width.saturating_sub(2),
                    list_height,
                );
                let divider_y = sidebar.y.saturating_add(list_height).saturating_add(1);
                let actions = [
                    Rect::new(
                        sidebar.x.saturating_add(1),
                        divider_y,
                        sidebar.width.saturating_sub(2),
                        1,
                    ),
                    Rect::new(
                        sidebar.x.saturating_add(1),
                        divider_y.saturating_add(1),
                        sidebar.width.saturating_sub(2),
                        1,
                    ),
                ];
                (Some(list), Some(actions))
            }
            Some(sidebar) => (
                Some(Rect::new(
                    sidebar.x.saturating_add(1),
                    sidebar.y,
                    sidebar.width.saturating_sub(2),
                    0,
                )),
                None,
            ),
            None => (None, None),
        };

        let menu_button = if sidebar.is_none() && app.screen() == crate::app::Screen::Main {
            Some(Rect::new(
                header.x,
                header.y,
                widgets::button_width(text::MENU_BUTTON, ButtonTone::Secondary).min(header.width),
                1,
            ))
        } else {
            None
        };

        Self {
            header,
            banner,
            sidebar,
            sidebar_list,
            sidebar_actions,
            body,
            footer,
            menu_button,
        }
    }

    /// Width of the navigation text column after reserving the scrollbar.
    #[must_use]
    pub fn nav_width(&self) -> u16 {
        self.sidebar_list
            .map_or(0, |list| list.width.saturating_sub(1))
    }
}

/// Sidebar row: a group heading or a page entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NavRow {
    /// Group heading, not clickable.
    Group(SidebarGroup),
    /// Page entry, clickable.
    Page(Page),
}

/// Flattened sidebar rows in render order.
#[must_use]
pub fn nav_rows() -> Vec<NavRow> {
    let mut rows = Vec::with_capacity(NAV_ROW_COUNT);
    let mut group: Option<SidebarGroup> = None;
    for item in NAV_ENTRIES {
        if group != Some(item.group) {
            group = Some(item.group);
            rows.push(NavRow::Group(item.group));
        }
        rows.push(NavRow::Page(item.page));
    }
    rows
}

/// Health tone for the header dot and Settings.
#[must_use]
pub fn health_tone(state: &HealthState) -> StatusTone {
    match state {
        HealthState::Healthy => StatusTone::Ok,
        HealthState::Degraded(_) => StatusTone::Warning,
        HealthState::Unreachable(_) => StatusTone::Error,
        HealthState::Unknown => StatusTone::Muted,
    }
}

/// Header line for the current state.
#[must_use]
pub fn header_line(app: &App) -> Line<'static> {
    let onboarding = app.screen() == crate::app::Screen::Onboarding;
    let health_label = if onboarding {
        text::HEALTH_NOT_CONNECTED.to_owned()
    } else {
        text::health_label(app.health())
    };
    let mut spans = vec![
        Span::styled(text::APP_TITLE, widgets::title_style()),
        Span::raw("  "),
        Span::styled(app.header_base_url(), widgets::muted_style()),
        Span::raw("  "),
        widgets::status_dot(if onboarding {
            StatusTone::Muted
        } else {
            health_tone(app.health())
        }),
        Span::raw(" "),
        Span::styled(health_label, widgets::muted_style()),
    ];
    if !onboarding {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            text::last_refresh_label(app.last_refresh(), std::time::Instant::now()),
            widgets::dim_style(),
        ));
    }
    Line::from(spans)
}

/// Footer hint for the current focus and overlay.
#[must_use]
pub fn footer_hint(app: &App, narrow: bool) -> &'static str {
    if app.overlay().is_some() {
        return text::FOOTER_OVERLAY;
    }
    match app.screen() {
        crate::app::Screen::Onboarding
        | crate::app::Screen::SettingsCredentials
        | crate::app::Screen::SettingsObservability => text::FOOTER_FIELD,
        crate::app::Screen::SessionCreate => text::FOOTER_CREATE,
        crate::app::Screen::SessionDetail => text::FOOTER_DETAIL,
        crate::app::Screen::SessionEdit => text::FOOTER_EDIT,
        crate::app::Screen::Main if app.page() == Page::Settings => text::FOOTER_SETTINGS,
        crate::app::Screen::Main if app.page() == Page::SessionLog => text::FOOTER_SESSION_LOG,
        crate::app::Screen::Main
            if matches!(
                app.page(),
                Page::CoreMetrics | Page::AgentMetrics | Page::RequestLog
            ) =>
        {
            if narrow {
                text::FOOTER_METRICS_NARROW
            } else {
                text::FOOTER_METRICS
            }
        }
        crate::app::Screen::Main => {
            if narrow {
                text::FOOTER_MAIN_NARROW
            } else {
                text::FOOTER_MAIN
            }
        }
    }
}

/// Loaded-scope note for the current page.
#[must_use]
pub fn scope_note(app: &App) -> String {
    match app.screen() {
        crate::app::Screen::Onboarding => text::SCOPE_ONBOARDING.to_owned(),
        crate::app::Screen::SettingsCredentials | crate::app::Screen::SettingsObservability => {
            text::SCOPE_SETTINGS.to_owned()
        }
        crate::app::Screen::SessionDetail => {
            text::scope_detail(app.detail().timeline().loaded(), app.detail().stale())
        }
        crate::app::Screen::SessionEdit => text::SCOPE_EDIT.to_owned(),
        crate::app::Screen::SessionCreate => text::SCOPE_CREATE.to_owned(),
        crate::app::Screen::Main if app.page() == Page::Settings => text::SCOPE_SETTINGS.to_owned(),
        crate::app::Screen::Main if app.page() == Page::SessionLog => {
            text::scope_session_log(app.session_log().loaded(), app.session_log().stale())
        }
        crate::app::Screen::Main if app.page() == Page::CoreMetrics => {
            text::core_metrics_scope(app.core_metrics().window(), app.core_metrics().stale())
        }
        crate::app::Screen::Main if app.page() == Page::AgentMetrics => text::agent_metrics_scope(
            app.agent_metrics().window(),
            app.agent_metrics().loaded(),
            app.agent_metrics().truncated(),
            app.agent_metrics().stale(),
        ),
        crate::app::Screen::Main if app.page() == Page::RequestLog => text::request_log_scope(
            app.request_log().window(),
            app.request_log().page(),
            app.request_log().stale(),
        ),
        crate::app::Screen::Main => text::SCOPE_EMPTY.to_owned(),
    }
}

/// Draw the header and return nothing; hit registration is page-level.
pub fn render_header(buf: &mut Buffer, layout: &ShellLayout, app: &App) {
    let header = layout.header;
    if header.is_empty() {
        return;
    }
    let mut offset = 0u16;
    if let Some(button) = layout.menu_button {
        widgets::render_button(buf, button, text::MENU_BUTTON, false, ButtonTone::Secondary);
        offset = button.width.saturating_add(1).min(header.width);
    }
    let width = header.width.saturating_sub(offset);
    if width == 0 {
        return;
    }
    let area = Rect::new(header.x.saturating_add(offset), header.y, width, 1);
    buf.set_line(area.x, area.y, &header_line(app), area.width);
}

/// Register the menu button click target.
pub fn register_menu_button(hits: &mut HitMap, layout: &ShellLayout, app: &App) {
    if let Some(button) = layout.menu_button {
        if app.screen() == crate::app::Screen::Main {
            hits.region(Layer::Base, button, crate::app::Intent::ToggleMenu);
        }
    }
}

/// Draw the optional error/contract banner and register its buttons.
pub fn render_banner(
    buf: &mut Buffer,
    layout: &ShellLayout,
    banner: &BannerView,
    hits: &mut HitMap,
) {
    let Some(area) = layout.banner else {
        return;
    };
    if area.is_empty() {
        return;
    }
    let style = match banner.tone {
        BannerTone::Error => widgets::error_style(),
        BannerTone::Warning => widgets::warning_style(),
    };
    let buttons = banner_buttons(area, banner.actions);
    let text_right = buttons
        .iter()
        .map(|(_, rect)| rect.x)
        .min()
        .unwrap_or(area.right())
        .saturating_sub(1);
    let message_width = text_right.saturating_sub(area.x);
    if message_width > 0 {
        let message = text::banner_text(banner.code.as_deref(), &banner.message);
        buf.set_string(area.x, area.y, widgets::fit(&message, message_width), style);
    }
    hits.region(Layer::Base, area, crate::app::Intent::OpenErrorActions);
    for (action, rect) in buttons {
        let (label, tone) = match action {
            ActionId::Retry => (text::BANNER_RETRY, ButtonTone::Primary),
            _ => (text::BANNER_SETTINGS, ButtonTone::Secondary),
        };
        widgets::render_button(buf, rect, label, false, tone);
        let intent = match action {
            ActionId::Retry => crate::app::Intent::Retry,
            _ => crate::app::Intent::OpenSettings,
        };
        hits.region(Layer::Base, rect, intent);
    }
}

/// Button rectangles inside a banner, right aligned.
#[must_use]
pub fn banner_buttons(area: Rect, actions: BannerActions) -> Vec<(ActionId, Rect)> {
    let labels = match actions {
        BannerActions::None => return Vec::new(),
        BannerActions::Settings => vec![(
            ActionId::OpenSettings,
            text::BANNER_SETTINGS,
            ButtonTone::Secondary,
        )],
        BannerActions::RetryAndSettings => vec![
            (
                ActionId::OpenSettings,
                text::BANNER_SETTINGS,
                ButtonTone::Secondary,
            ),
            (ActionId::Retry, text::BANNER_RETRY, ButtonTone::Primary),
        ],
    };
    let mut x = area.right().saturating_sub(1);
    let mut rects = Vec::with_capacity(labels.len());
    for (action, label, tone) in labels {
        let width = widgets::button_width(label, tone);
        if width == 0 || x < area.x.saturating_add(2) {
            break;
        }
        x = x.saturating_sub(width);
        rects.push((action, Rect::new(x, area.y, width, 1)));
        x = x.saturating_sub(1);
    }
    rects
}

/// Draw the sidebar and register navigation, action and scrollbar hits.
pub fn render_sidebar(buf: &mut Buffer, layout: &ShellLayout, app: &App, hits: &mut HitMap) {
    let Some(sidebar) = layout.sidebar else {
        return;
    };
    if sidebar.is_empty() {
        return;
    }
    let separator = Style::default().fg(widgets::theme().semantic.border.default);
    for row in sidebar.y..sidebar.bottom() {
        let x = sidebar.right().saturating_sub(1);
        buf.set_string(x, row, "│", separator);
    }

    if let (Some(list), Some(actions)) = (layout.sidebar_list, layout.sidebar_actions) {
        let nav_width = layout.nav_width();
        let rows = nav_rows();
        let start = app.sidebar_scroll();
        let visible = usize::from(list.height);
        for (offset, row) in rows.iter().enumerate().skip(start).take(visible) {
            let y = list
                .y
                .saturating_add(u16::try_from(offset - start).unwrap_or(0));
            let rect = Rect::new(list.x, y, nav_width, 1);
            if rect.is_empty() {
                continue;
            }
            match row {
                NavRow::Group(group) => {
                    widgets::render_section(buf, rect, &format!(" {}", text::group_label(*group)));
                }
                NavRow::Page(page) => {
                    let selected = *page == app.page();
                    if selected {
                        buf.set_style(rect, widgets::selected_style());
                    }
                    let marker = if selected { ">" } else { " " };
                    let label = format!("{marker} {}", text::page_label(*page));
                    let style = if selected {
                        widgets::selected_style()
                    } else {
                        widgets::muted_style()
                    };
                    buf.set_string(rect.x, rect.y, widgets::fit(&label, rect.width), style);
                    hits.region(Layer::Base, rect, crate::app::Intent::SelectPage(*page));
                }
            }
        }
        hits.scroll_zone(list, ScrollZone::Sidebar);
        let scrollbar_area = Rect::new(list.right().saturating_sub(1), list.y, 1, list.height);
        if let Some(bar) = VerticalBar::new(
            list.height,
            usize::from(list.height),
            NAV_ROW_COUNT,
            app.sidebar_scroll(),
        ) {
            bar.paint(
                scrollbar_area,
                buf,
                widgets::selected_style(),
                widgets::dim_style(),
            );
            hits.scrollbar(scrollbar_area, ScrollZone::Sidebar, bar);
        }

        let divider = Rect::new(list.x, actions[0].y.saturating_sub(1), nav_width, 1);
        widgets::render_rule(buf, divider);
        render_sidebar_action(
            buf,
            actions[0],
            app,
            hits,
            ActionId::ShowGettingStarted,
            text::action_label(ActionId::ShowGettingStarted),
        );
        render_sidebar_action(
            buf,
            actions[1],
            app,
            hits,
            ActionId::SignOut,
            text::action_label(ActionId::SignOut),
        );
    }
}

fn render_sidebar_action(
    buf: &mut Buffer,
    rect: Rect,
    app: &App,
    hits: &mut HitMap,
    action: ActionId,
    label: &str,
) {
    if rect.is_empty() {
        return;
    }
    let focused = app.focus() == Focus::Action(action);
    let style = if focused {
        widgets::selected_style()
    } else {
        widgets::muted_style()
    };
    if focused {
        buf.set_style(rect, widgets::selected_style());
    }
    buf.set_string(rect.x, rect.y, widgets::fit(label, rect.width), style);
    hits.region(Layer::Base, rect, crate::app::Intent::Activate(action));
}

/// Draw the footer hint and scope note.
pub fn render_footer(buf: &mut Buffer, layout: &ShellLayout, app: &App) {
    let footer = layout.footer;
    if footer.is_empty() {
        return;
    }
    let narrow = layout.sidebar.is_none();
    let line = format!("{}  ·  {}", footer_hint(app, narrow), scope_note(app));
    buf.set_string(
        footer.x,
        footer.y,
        widgets::fit(&line, footer.width),
        widgets::dim_style(),
    );
}

/// Centered "terminal too small" screen.
pub fn render_too_small(buf: &mut Buffer, area: Rect) {
    if area.is_empty() {
        return;
    }
    let middle = Rect::new(
        area.x,
        area.y + area.height / 2,
        area.width,
        2.min(area.height),
    );
    let title = Line::from(vec![Span::styled(
        text::TERMINAL_TOO_SMALL,
        widgets::error_style(),
    )]);
    widgets::render_centered(buf, middle, title);
    let hint_area = Rect::new(area.x, middle.y.saturating_add(1), area.width, 1);
    widgets::render_centered(
        buf,
        hint_area,
        Line::from(vec![Span::styled(
            text::TERMINAL_TOO_SMALL_HINT,
            widgets::muted_style(),
        )]),
    );
}

/// Clamp a sidebar offset helper used by tests and the runtime.
#[must_use]
pub fn clamp_sidebar(offset: usize, viewport: usize) -> usize {
    let max = NAV_ROW_COUNT.saturating_sub(viewport);
    offset.min(max)
}

/// Wheel delta to sidebar offset.
#[must_use]
pub fn scrolled(offset: usize, delta: i32, viewport: usize) -> usize {
    let max = NAV_ROW_COUNT.saturating_sub(viewport);
    bounded_offset(
        offset,
        delta.saturating_mul(i32::from(peri_tui_components::scrollbar::SCROLL_LINES)),
        max,
    )
}

#[cfg(test)]
#[path = "shell_test.rs"]
mod tests;
