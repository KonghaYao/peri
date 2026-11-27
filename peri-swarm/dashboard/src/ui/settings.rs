//! Settings page and the credential sub-page.

use std::time::Instant;

use peri_tui_components::button::ButtonTone;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;

use crate::api;
use crate::app::{ActionId, App, FieldId, Focus, Intent, SettingsStatus};
use crate::ui::hit::{HitMap, Layer};
use crate::ui::text;
use crate::ui::widgets::{self, Rows};

use super::onboarding::cleartext_warning;

/// Render the Settings page.
pub fn render(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let width = rows.width();
    widgets::render_body(
        buf,
        rows.line(),
        text::SETTINGS_TITLE,
        widgets::title_style(),
    );
    rows.skip(1);

    widgets::render_section(buf, rows.line(), text::SECTION_CONNECTION);
    let base_url = app.config_base_url().unwrap_or(text::DASH).to_owned();
    widgets::render_label_value(
        buf,
        rows.line(),
        text::LABEL_BASE_URL,
        &base_url,
        widgets::title_style(),
    );
    widgets::render_label_value(
        buf,
        rows.line(),
        text::LABEL_API_KEY,
        if app.has_config() {
            text::KEY_SET
        } else {
            text::KEY_NOT_SET
        },
        widgets::muted_style(),
    );
    widgets::render_label_value(
        buf,
        rows.line(),
        text::LABEL_HEALTH,
        &text::health_label(app.health()),
        widgets::tone_style(super::shell::health_tone(app.health())),
    );
    widgets::render_label_value(
        buf,
        rows.line(),
        text::LABEL_CONTRACT,
        app.contract().unwrap_or(text::CONTRACT_NOT_REPORTED),
        if app.contract_mismatch() {
            widgets::warning_style()
        } else {
            widgets::muted_style()
        },
    );
    widgets::render_label_value(
        buf,
        rows.line(),
        text::LABEL_LATENCY,
        &text::latency_label(app.last_latency()),
        widgets::muted_style(),
    );
    widgets::render_label_value(
        buf,
        rows.line(),
        text::LABEL_REFRESH,
        &text::last_refresh_label(app.last_refresh(), Instant::now()),
        widgets::muted_style(),
    );

    if let Some(actual) = app.contract().filter(|_| app.contract_mismatch()) {
        let warning = text::contract_mismatch(actual, api::CONTRACT_VERSION);
        let height = widgets::wrapped_height(&warning, width);
        widgets::render_body(buf, rows.take(height), &warning, widgets::warning_style());
    }
    widgets::render_body(
        buf,
        rows.line(),
        text::SETTINGS_KEY_HINT,
        widgets::dim_style(),
    );

    rows.skip(1);
    widgets::render_section(buf, rows.line(), text::SECTION_ACTIONS);

    if app.validation_purpose() == Some(crate::worker::ValidationPurpose::TestConnection) {
        widgets::render_body(
            buf,
            rows.line(),
            text::TESTING_CONNECTION,
            widgets::dim_style(),
        );
    }
    let busy = app.validation_purpose().is_some();
    action_button(
        buf,
        rows.line(),
        app,
        hits,
        ActionId::TestConnection,
        !busy,
        ButtonTone::Primary,
    );
    action_button(
        buf,
        rows.line(),
        app,
        hits,
        ActionId::EditCredentials,
        !busy,
        ButtonTone::Secondary,
    );
    action_button(
        buf,
        rows.line(),
        app,
        hits,
        ActionId::SignOut,
        !busy,
        ButtonTone::Danger,
    );
    action_button(
        buf,
        rows.line(),
        app,
        hits,
        ActionId::Quit,
        !busy,
        ButtonTone::Secondary,
    );

    if let Some(status) = app.settings_status() {
        rows.skip(1);
        let (message, style) = match status {
            SettingsStatus::Ok(message) => (message, widgets::success_style()),
            SettingsStatus::Error(message) => (message, widgets::error_style()),
        };
        let height = widgets::wrapped_height(message, width);
        widgets::render_body(buf, rows.take(height), message, style);
    }
}

/// Render the credential editor sub-page.
pub fn render_credentials(
    buf: &mut Buffer,
    rows: &mut Rows,
    app: &App,
    hits: &mut HitMap,
) -> Option<Position> {
    let state = app.credentials();
    let width = rows.width();
    let mut cursor = None;

    widgets::render_body(
        buf,
        rows.line(),
        text::CREDENTIALS_TITLE,
        widgets::title_style(),
    );
    rows.skip(1);

    let intro_height = widgets::wrapped_height(text::CREDENTIALS_INTRO, width);
    widgets::render_body(
        buf,
        rows.take(intro_height),
        text::CREDENTIALS_INTRO,
        widgets::muted_style(),
    );
    rows.skip(1);

    widgets::render_body(
        buf,
        rows.line(),
        text::field_label(FieldId::CredentialsBaseUrl),
        widgets::section_style(),
    );
    let base_rect = rows.take(3);
    let base_focused = app.focus() == Focus::Field(FieldId::CredentialsBaseUrl);
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
    hits.region(
        Layer::Base,
        base_rect,
        Intent::FocusField {
            field: FieldId::CredentialsBaseUrl,
            click: None,
        },
    );
    if let Some(error) = state.base_url_error() {
        widgets::render_body(buf, rows.line(), error, widgets::error_style());
    }

    rows.skip(1);
    widgets::render_body(
        buf,
        rows.line(),
        text::field_label(FieldId::CredentialsApiKey),
        widgets::section_style(),
    );
    let key_rect = rows.take(3);
    let key_focused = app.focus() == Focus::Field(FieldId::CredentialsApiKey);
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
    hits.region(
        Layer::Base,
        key_rect,
        Intent::FocusField {
            field: FieldId::CredentialsApiKey,
            click: None,
        },
    );
    if let Some(error) = state.api_key_error() {
        widgets::render_body(buf, rows.line(), error, widgets::error_style());
    }
    widgets::render_body(
        buf,
        rows.line(),
        text::CREDENTIALS_KEY_HINT,
        widgets::dim_style(),
    );

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
    let busy = app.validation_purpose() == Some(crate::worker::ValidationPurpose::Credentials);
    if busy {
        widgets::render_body(buf, rows.line(), text::VALIDATING, widgets::dim_style());
    } else {
        action_button(
            buf,
            rows.line(),
            app,
            hits,
            ActionId::SaveCredentials,
            true,
            ButtonTone::Primary,
        );
    }
    action_button(
        buf,
        rows.line(),
        app,
        hits,
        ActionId::Back,
        true,
        ButtonTone::Secondary,
    );

    cursor
}

fn action_button(
    buf: &mut Buffer,
    row: ratatui::layout::Rect,
    app: &App,
    hits: &mut HitMap,
    action: ActionId,
    enabled: bool,
    tone: ButtonTone,
) {
    let label = text::action_label(action);
    let focused = enabled && app.focus() == Focus::Action(action);
    let button = widgets::button_rect(row, row.x, label);
    if enabled {
        widgets::render_button(buf, button, label, focused, tone);
        hits.region(Layer::Base, button, Intent::Activate(action));
    } else {
        widgets::render_button(buf, button, label, false, tone);
    }
}

#[cfg(test)]
#[path = "settings_test.rs"]
mod tests;
