//! First-run onboarding form.
//!
//! Validation is entirely owned by the app state machine: this module only
//! draws the form and turns clicks into semantic intents.

use peri_tui_components::button::ButtonTone;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use url::{Host, Url};

use crate::app::{ActionId, App, FieldId, Focus, Intent};
use crate::ui::hit::{HitMap, Layer};
use crate::ui::text;
use crate::ui::widgets::{self, Rows};

/// Render the onboarding page into `buf`; returns the caret position.
pub fn render(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) -> Option<Position> {
    let state = app.onboarding_form();
    let width = rows.width();
    let mut cursor = None;

    widgets::render_body(
        buf,
        rows.line(),
        text::ONBOARDING_TITLE,
        widgets::title_style(),
    );
    rows.skip(1);

    let intro_height = widgets::wrapped_height(text::ONBOARDING_INTRO, width);
    widgets::render_body(
        buf,
        rows.take(intro_height),
        text::ONBOARDING_INTRO,
        widgets::muted_style(),
    );
    rows.skip(1);

    widgets::render_body(
        buf,
        rows.line(),
        text::field_label(FieldId::OnboardingBaseUrl),
        widgets::section_style(),
    );
    let base_rect = rows.take(3);
    let base_focused = app.focus() == Focus::Field(FieldId::OnboardingBaseUrl);
    if let Some(position) = widgets::render_field(
        buf,
        base_rect,
        state.base_url(),
        base_focused,
        false,
        text::ONBOARDING_BASE_URL_PLACEHOLDER,
    ) {
        cursor = Some(position);
    }
    register_field(hits, base_rect, FieldId::OnboardingBaseUrl);
    if let Some(error) = state.base_url_error() {
        widgets::render_body(buf, rows.line(), error, widgets::error_style());
    }

    rows.skip(1);
    widgets::render_body(
        buf,
        rows.line(),
        text::field_label(FieldId::OnboardingApiKey),
        widgets::section_style(),
    );
    let key_rect = rows.take(3);
    let key_focused = app.focus() == Focus::Field(FieldId::OnboardingApiKey);
    if let Some(position) = widgets::render_field(
        buf,
        key_rect,
        state.api_key(),
        key_focused,
        true,
        text::ONBOARDING_API_KEY_PLACEHOLDER,
    ) {
        cursor = Some(position);
    }
    register_field(hits, key_rect, FieldId::OnboardingApiKey);
    if let Some(error) = state.api_key_error() {
        widgets::render_body(buf, rows.line(), error, widgets::error_style());
    }

    if cleartext_warning(state.base_url().value()) {
        rows.skip(1);
        let height = widgets::wrapped_height(text::CLEARTEXT_WARNING, width);
        widgets::render_body(
            buf,
            rows.take(height),
            text::CLEARTEXT_WARNING,
            widgets::warning_style(),
        );
    }

    if let Some(error) = state.form_error() {
        rows.skip(1);
        let height = widgets::wrapped_height(error, width);
        widgets::render_body(buf, rows.take(height), error, widgets::error_style());
    }

    rows.skip(1);
    let busy = app.validation_purpose() == Some(crate::worker::ValidationPurpose::Onboarding);
    if busy {
        widgets::render_body(buf, rows.line(), text::VALIDATING, widgets::dim_style());
    } else {
        let rect = rows.line();
        let label = text::action_label(ActionId::Submit);
        let focused = app.focus() == Focus::Action(ActionId::Submit);
        let button = widgets::button_rect(rect, rect.x, label);
        widgets::render_button(buf, button, label, focused, ButtonTone::Primary);
        hits.region(Layer::Base, button, Intent::Activate(ActionId::Submit));
    }

    cursor
}

/// Register a clickable text field. The runtime derives the caret from the
/// click column and the rectangle stored here.
fn register_field(hits: &mut HitMap, rect: ratatui::layout::Rect, field: FieldId) {
    hits.region(Layer::Base, rect, Intent::FocusField { field, click: None });
}

/// Whether an `http://` origin is not loopback and therefore sends the key in
/// cleartext. Parsing failures stay silent: submit reports them.
#[must_use]
pub fn cleartext_warning(value: &str) -> bool {
    let Ok(url) = Url::parse(value.trim()) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    match url.host() {
        Some(Host::Ipv4(address)) => !address.is_loopback(),
        Some(Host::Ipv6(address)) => !address.is_loopback(),
        Some(Host::Domain(name)) => !name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

#[cfg(test)]
#[path = "onboarding_test.rs"]
mod tests;
