//! Modal dialog overlays: sign-out confirmation, narrow menu, and banner
//! actions.

use peri_tui_components::button::ButtonTone;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Widget};

use crate::app::{ActionId, App, Intent, Overlay, Page};
use crate::ui::hit::{HitMap, Layer};
use crate::ui::text;
use crate::ui::widgets::{self, Rows};

/// Draw a modal frame and return its inner area.
fn panel(
    buf: &mut Buffer,
    area: Rect,
    desired_width: u16,
    desired_height: u16,
    title: &str,
    hits: &mut HitMap,
) -> Option<Rect> {
    let width = desired_width.min(area.width.saturating_sub(4));
    let height = desired_height.min(area.height.saturating_sub(2));
    if width == 0 || height == 0 {
        return None;
    }
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let y = area
        .y
        .saturating_add(area.height.saturating_sub(height) / 2);
    let panel = Rect::new(x, y, width, height);
    hits.set_overlay_area(panel);
    Clear.render(panel, buf);
    let block = Block::bordered()
        .title(Line::from(title.to_owned()).style(widgets::title_style()))
        .border_style(widgets::border_style(true));
    let inner = block.inner(panel);
    block.render(panel, buf);
    if inner.is_empty() {
        None
    } else {
        Some(inner)
    }
}

/// Confirm sign-out dialog.
pub fn render_sign_out(buf: &mut Buffer, area: Rect, app: &App, hits: &mut HitMap) {
    let Some(inner) = panel(buf, area, 66, 8, text::SIGNOUT_TITLE, hits) else {
        return;
    };
    let mut rows = Rows::new(inner);
    let height = widgets::wrapped_height(text::SIGNOUT_MESSAGE, inner.width);
    widgets::render_body(
        buf,
        rows.take(height),
        text::SIGNOUT_MESSAGE,
        widgets::muted_style(),
    );
    rows.skip(1);
    let row = rows.line();
    let cancel = text::action_label(ActionId::CancelSignOut);
    let confirm = text::action_label(ActionId::ConfirmSignOut);
    let cancel_rect = widgets::button_rect(row, row.x, cancel, ButtonTone::Secondary);
    widgets::render_button(
        buf,
        cancel_rect,
        cancel,
        app.overlay_cursor() == 0,
        ButtonTone::Secondary,
    );
    hits.region(
        Layer::Overlay,
        cancel_rect,
        Intent::Activate(ActionId::CancelSignOut),
    );
    let confirm_x = cancel_rect.right().saturating_add(2);
    let confirm_rect = widgets::button_rect(row, confirm_x, confirm, ButtonTone::Danger);
    widgets::render_button(
        buf,
        confirm_rect,
        confirm,
        app.overlay_cursor() == 1,
        ButtonTone::Danger,
    );
    hits.region(
        Layer::Overlay,
        confirm_rect,
        Intent::Activate(ActionId::ConfirmSignOut),
    );
    rows.skip(1);
    widgets::render_body(buf, rows.line(), text::SIGNOUT_HINT, widgets::dim_style());
}

/// Narrow-terminal navigation menu.
pub fn render_menu(buf: &mut Buffer, area: Rect, app: &App, hits: &mut HitMap) {
    let Some(inner) = panel(buf, area, 44, 17, text::MENU_TITLE, hits) else {
        return;
    };
    let hint_rows = 1u16.min(inner.height);
    let viewport = usize::from(inner.height.saturating_sub(hint_rows));
    if viewport == 0 {
        return;
    }
    let pages = Page::ALL;
    let max_start = pages.len().saturating_sub(viewport);
    let cursor = app.overlay_cursor().min(pages.len() - 1);
    let start = cursor
        .saturating_sub(viewport.saturating_sub(1))
        .min(max_start);
    let mut rows = Rows::new(inner);
    for (index, page) in pages.iter().enumerate().skip(start).take(viewport) {
        let row = rows.line();
        if row.is_empty() {
            break;
        }
        let selected = index == cursor;
        if selected {
            buf.set_style(row, widgets::selected_style());
        }
        let marker = if selected { ">" } else { " " };
        let label = format!("{marker} {}", text::page_label(*page));
        let style = if selected {
            widgets::selected_style()
        } else {
            widgets::muted_style()
        };
        buf.set_string(row.x, row.y, widgets::fit(&label, row.width), style);
        hits.region(Layer::Overlay, row, Intent::SelectMenuPage(*page));
    }
    if inner.height >= 2 {
        let hint_row = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
        buf.set_string(
            hint_row.x,
            hint_row.y,
            widgets::fit(text::MENU_HINT, hint_row.width),
            widgets::dim_style(),
        );
    }
}

/// Banner error actions: Retry and Settings.
pub fn render_error_actions(buf: &mut Buffer, area: Rect, app: &App, hits: &mut HitMap) {
    let Some(inner) = panel(buf, area, 64, 9, text::ERROR_ACTIONS_TITLE, hits) else {
        return;
    };
    let banner = app.active_banner();
    let mut rows = Rows::new(inner);
    if let Some(banner) = &banner {
        let message = text::banner_text(banner.code.as_deref(), &banner.message);
        let height = widgets::wrapped_height(&message, inner.width);
        widgets::render_body(
            buf,
            rows.take(height),
            &message,
            match banner.tone {
                crate::app::BannerTone::Error => widgets::error_style(),
                crate::app::BannerTone::Warning => widgets::warning_style(),
            },
        );
    }
    rows.skip(1);
    let row = rows.line();
    let retry = text::BANNER_RETRY;
    let settings = text::BANNER_SETTINGS;
    let retry_rect = widgets::button_rect(row, row.x, retry, ButtonTone::Primary);
    widgets::render_button(
        buf,
        retry_rect,
        retry,
        app.overlay_cursor() == 0,
        ButtonTone::Primary,
    );
    hits.region(Layer::Overlay, retry_rect, Intent::Retry);
    let settings_x = retry_rect.right().saturating_add(2);
    let settings_rect = widgets::button_rect(row, settings_x, settings, ButtonTone::Secondary);
    widgets::render_button(
        buf,
        settings_rect,
        settings,
        app.overlay_cursor() == 1,
        ButtonTone::Secondary,
    );
    hits.region(Layer::Overlay, settings_rect, Intent::OpenSettings);
    rows.skip(1);
    widgets::render_body(
        buf,
        rows.line(),
        text::ERROR_ACTIONS_HINT,
        widgets::dim_style(),
    );
}

/// Copy and actions for a two-button confirmation dialog.
struct ConfirmSpec {
    title: &'static str,
    message: &'static str,
    hint: &'static str,
    cancel: ActionId,
    confirm: ActionId,
}

/// Confirmation dialog: a safe first choice and a danger confirm.
fn render_confirm(buf: &mut Buffer, area: Rect, app: &App, hits: &mut HitMap, spec: ConfirmSpec) {
    let Some(inner) = panel(buf, area, 66, 8, spec.title, hits) else {
        return;
    };
    let mut rows = Rows::new(inner);
    let height = widgets::wrapped_height(spec.message, inner.width);
    widgets::render_body(buf, rows.take(height), spec.message, widgets::muted_style());
    rows.skip(1);
    let row = rows.line();
    let cancel_label = text::action_label(spec.cancel);
    let confirm_label = text::action_label(spec.confirm);
    let cancel_rect = widgets::button_rect(row, row.x, cancel_label, ButtonTone::Secondary);
    widgets::render_button(
        buf,
        cancel_rect,
        cancel_label,
        app.overlay_cursor() == 0,
        ButtonTone::Secondary,
    );
    hits.region(Layer::Overlay, cancel_rect, Intent::Activate(spec.cancel));
    let confirm_x = cancel_rect.right().saturating_add(2);
    let confirm_rect = widgets::button_rect(row, confirm_x, confirm_label, ButtonTone::Danger);
    widgets::render_button(
        buf,
        confirm_rect,
        confirm_label,
        app.overlay_cursor() == 1,
        ButtonTone::Danger,
    );
    hits.region(Layer::Overlay, confirm_rect, Intent::Activate(spec.confirm));
    rows.skip(1);
    widgets::render_body(buf, rows.line(), spec.hint, widgets::dim_style());
}

/// Delete-session confirmation.
pub fn render_delete_confirm(buf: &mut Buffer, area: Rect, app: &App, hits: &mut HitMap) {
    render_confirm(
        buf,
        area,
        app,
        hits,
        ConfirmSpec {
            title: text::DELETE_TITLE,
            message: text::DELETE_MESSAGE,
            hint: text::DELETE_HINT,
            cancel: ActionId::CancelDeleteSession,
            confirm: ActionId::ConfirmDeleteSession,
        },
    );
}

/// Cancel-running-turn confirmation.
pub fn render_cancel_turn_confirm(buf: &mut Buffer, area: Rect, app: &App, hits: &mut HitMap) {
    render_confirm(
        buf,
        area,
        app,
        hits,
        ConfirmSpec {
            title: text::CANCEL_TURN_TITLE,
            message: text::CANCEL_TURN_MESSAGE,
            hint: text::CANCEL_TURN_HINT,
            cancel: ActionId::CancelCancelTurn,
            confirm: ActionId::ConfirmCancelTurn,
        },
    );
}

/// Dispatch on the active overlay.
pub fn render_active(buf: &mut Buffer, area: Rect, app: &App, hits: &mut HitMap) {
    match app.overlay() {
        Some(Overlay::Help) => super::help::render(buf, area, hits),
        Some(Overlay::ConfirmSignOut) => render_sign_out(buf, area, app, hits),
        Some(Overlay::NarrowMenu) => render_menu(buf, area, app, hits),
        Some(Overlay::ErrorActions) => render_error_actions(buf, area, app, hits),
        Some(Overlay::ConfirmDeleteSession) => render_delete_confirm(buf, area, app, hits),
        Some(Overlay::ConfirmCancelTurn) => render_cancel_turn_confirm(buf, area, app, hits),
        None => {}
    }
}
