//! Create-session form: fields, metadata rows, advanced JSON and submit.
//!
//! The form only holds user input and inline validation errors. Submitting goes
//! through [`crate::app::Intent::SubmitCreate`] so the runtime, not the view,
//! owns the worker request and the idempotency key.

use peri_tui_components::button::ButtonTone;
use peri_tui_components::textarea::{render_text_area_with_cursor, TextAreaStyles};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};

use crate::app::{ActionId, App, FieldId, Focus, Intent, TextAreaClick};
use crate::ui::hit::{HitMap, Layer};
use crate::ui::text;
use crate::ui::widgets::{self, Rows};

/// Render the create form body.
pub fn render(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) -> Option<Position> {
    let form = app.create_form();
    let mut cursor = None;

    widgets::render_body(buf, rows.line(), text::CREATE_TITLE, widgets::title_style());
    rows.skip(1);
    let intro_height = widgets::wrapped_height(text::CREATE_INTRO, rows.width());
    widgets::render_body(
        buf,
        rows.take(intro_height),
        text::CREATE_INTRO,
        widgets::muted_style(),
    );
    rows.skip(1);

    cursor = single_field(
        buf,
        rows,
        app,
        hits,
        FieldId::CreateLogicalId,
        text::CREATE_LOGICAL_ID_PLACEHOLDER,
        form.logical_id_error(),
    )
    .or(cursor);
    for (field, placeholder) in [
        (FieldId::CreateModel, text::CREATE_MODEL_PLACEHOLDER),
        (FieldId::CreateCwd, text::CREATE_CWD_PLACEHOLDER),
    ] {
        cursor = single_field(buf, rows, app, hits, field, placeholder, None).or(cursor);
    }
    multiline_field(
        buf,
        rows,
        app,
        hits,
        FieldId::CreateInstructions,
        text::CREATE_INSTRUCTIONS_PLACEHOLDER,
        6,
    );

    render_metadata(buf, rows, app, hits);
    render_advanced(buf, rows, app, hits);

    if let Some(error) = form.form_error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    rows.skip(1);
    if form.submitting() {
        widgets::render_body(
            buf,
            rows.line(),
            text::CREATE_SUBMITTING,
            widgets::dim_style(),
        );
    } else {
        action_button(
            buf,
            rows.line(),
            app,
            hits,
            ActionId::CreateSubmit,
            ButtonTone::Primary,
        );
    }
    action_button(
        buf,
        rows.line(),
        app,
        hits,
        ActionId::Back,
        ButtonTone::Secondary,
    );
    cursor
}

fn render_metadata(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let form = app.create_form();
    rows.skip(1);
    widgets::render_section(buf, rows.line(), text::SECTION_METADATA);
    if form.metadata().is_empty() {
        widgets::render_body(buf, rows.line(), text::METADATA_EMPTY, widgets::dim_style());
    }
    for (index, row) in form.metadata().iter().enumerate() {
        let band = rows.take(3);
        render_metadata_band(buf, band, app, hits, index, row.key(), row.value());
        let button_row = rows.line();
        let label = text::REMOVE_ROW;
        let rect = widgets::button_rect(button_row, button_row.x, label, ButtonTone::Danger);
        widgets::render_button(buf, rect, label, false, ButtonTone::Danger);
        hits.region(Layer::Base, rect, Intent::RemoveMetadataRow(index));
    }
    if let Some(error) = form.metadata_error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    action_button(
        buf,
        rows.line(),
        app,
        hits,
        ActionId::AddMetadata,
        ButtonTone::Secondary,
    );
}

fn render_metadata_band(
    buf: &mut Buffer,
    band: Rect,
    app: &App,
    hits: &mut HitMap,
    index: usize,
    key: &crate::text_field::TextField,
    value: &crate::text_field::TextField,
) {
    if band.height == 0 || band.width < 4 {
        return;
    }
    let key_width = band.width.saturating_sub(1) * 2 / 5;
    let key_rect = Rect::new(band.x, band.y, key_width, band.height);
    let value_rect = Rect::new(
        band.x.saturating_add(key_width).saturating_add(1),
        band.y,
        band.width.saturating_sub(key_width).saturating_sub(1),
        band.height,
    );
    let key_field = FieldId::CreateMetadataKey(index);
    let value_field = FieldId::CreateMetadataValue(index);
    widgets::render_field(
        buf,
        key_rect,
        key,
        app.focus() == Focus::Field(key_field),
        false,
        text::METADATA_KEY_PLACEHOLDER,
    );
    widgets::render_field(
        buf,
        value_rect,
        value,
        app.focus() == Focus::Field(value_field),
        false,
        text::METADATA_VALUE_PLACEHOLDER,
    );
    hits.region(
        Layer::Base,
        key_rect,
        Intent::FocusField {
            field: key_field,
            click: None,
        },
    );
    hits.region(
        Layer::Base,
        value_rect,
        Intent::FocusField {
            field: value_field,
            click: None,
        },
    );
}

fn render_advanced(buf: &mut Buffer, rows: &mut Rows, app: &App, hits: &mut HitMap) {
    let form = app.create_form();
    rows.skip(1);
    widgets::render_section(buf, rows.line(), text::SECTION_ADVANCED);
    let label = if form.advanced_open() {
        text::HIDE_ADVANCED
    } else {
        text::SHOW_ADVANCED
    };
    let row = rows.line();
    let rect = widgets::button_rect(row, row.x, label, ButtonTone::Secondary);
    widgets::render_button(
        buf,
        rect,
        label,
        app.focus() == Focus::Action(ActionId::ToggleAdvanced),
        ButtonTone::Secondary,
    );
    hits.region(
        Layer::Base,
        rect,
        Intent::Activate(ActionId::ToggleAdvanced),
    );
    if !form.advanced_open() {
        return;
    }
    multiline_field(
        buf,
        rows,
        app,
        hits,
        FieldId::CreateTools,
        text::CREATE_TOOLS_PLACEHOLDER,
        5,
    );
    if let Some(error) = form.tools_error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    multiline_field(
        buf,
        rows,
        app,
        hits,
        FieldId::CreateEnvironment,
        text::CREATE_ENVIRONMENT_PLACEHOLDER,
        5,
    );
    if let Some(error) = form.environment_error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    multiline_field(
        buf,
        rows,
        app,
        hits,
        FieldId::CreatePromptCache,
        text::CREATE_PROMPT_CACHE_PLACEHOLDER,
        5,
    );
    if let Some(error) = form.prompt_cache_error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
}

fn single_field(
    buf: &mut Buffer,
    rows: &mut Rows,
    app: &App,
    hits: &mut HitMap,
    field: FieldId,
    placeholder: &str,
    error: Option<&str>,
) -> Option<Position> {
    widgets::render_body(
        buf,
        rows.line(),
        text::field_label(field),
        widgets::section_style(),
    );
    let value = app.field(field)?;
    let rect = rows.take(3);
    let cursor = widgets::render_field(
        buf,
        rect,
        value,
        app.focus() == Focus::Field(field),
        false,
        placeholder,
    );
    hits.region(Layer::Base, rect, Intent::FocusField { field, click: None });
    if let Some(error) = error {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    cursor
}

fn multiline_field(
    buf: &mut Buffer,
    rows: &mut Rows,
    app: &App,
    hits: &mut HitMap,
    field: FieldId,
    placeholder: &str,
    height: u16,
) {
    widgets::render_body(
        buf,
        rows.line(),
        text::field_label(field),
        widgets::section_style(),
    );
    let Some(state) = app.text_area(field) else {
        return;
    };
    let rect = rows.take(height.saturating_add(2));
    let focused = app.focus() == Focus::Field(field);
    let styles = TextAreaStyles {
        cursor: widgets::selected_style(),
        selection: widgets::selected_style(),
        placeholder: widgets::dim_style(),
        base: widgets::muted_style(),
    };
    render_text_area_with_cursor(
        buf,
        rect,
        state,
        focused,
        widgets::border_style(focused),
        styles,
        Some(placeholder),
    );
    hits.region(
        Layer::Base,
        rect,
        Intent::FocusTextAreaAt {
            field,
            click: TextAreaClick {
                row: 0,
                column: 0,
                width: rect.width.saturating_sub(2),
                height: rect.height.saturating_sub(2),
            },
        },
    );
}

fn action_button(
    buf: &mut Buffer,
    row: Rect,
    app: &App,
    hits: &mut HitMap,
    action: ActionId,
    tone: ButtonTone,
) {
    let label = text::action_label(action);
    let rect = widgets::button_rect(row, row.x, label, tone);
    widgets::render_button(buf, rect, label, app.focus() == Focus::Action(action), tone);
    hits.region(Layer::Base, rect, Intent::Activate(action));
}

fn render_wrapped(buf: &mut Buffer, rows: &mut Rows, body: &str, style: ratatui::style::Style) {
    let height = widgets::wrapped_height(body, rows.width());
    widgets::render_body(buf, rows.take(height), body, style);
}

/// Render the edit form body.
///
/// The edit form shares the create widgets but sends an update: model,
/// metadata and prompt-cache JSON. Instructions are read-only because the
/// gateway update endpoint rejects them.
pub fn render_edit(
    buf: &mut Buffer,
    rows: &mut Rows,
    app: &App,
    hits: &mut HitMap,
) -> Option<Position> {
    let form = app.edit_form();
    let mut cursor = None;

    widgets::render_body(buf, rows.line(), text::EDIT_TITLE, widgets::title_style());
    rows.skip(1);
    render_wrapped(buf, rows, text::EDIT_INTRO, widgets::muted_style());
    rows.skip(1);

    cursor = single_field(
        buf,
        rows,
        app,
        hits,
        FieldId::EditModel,
        text::EDIT_MODEL_PLACEHOLDER,
        form.model_error(),
    )
    .or(cursor);

    rows.skip(1);
    widgets::render_section(buf, rows.line(), text::SECTION_METADATA);
    if form.metadata().is_empty() {
        widgets::render_body(buf, rows.line(), text::METADATA_EMPTY, widgets::dim_style());
    }
    for (index, row) in form.metadata().iter().enumerate() {
        let band = rows.take(3);
        render_edit_metadata_band(buf, band, app, hits, index, row.key(), row.value());
        let button_row = rows.line();
        let label = text::REMOVE_ROW;
        let rect = widgets::button_rect(button_row, button_row.x, label, ButtonTone::Danger);
        widgets::render_button(buf, rect, label, false, ButtonTone::Danger);
        hits.region(Layer::Base, rect, Intent::RemoveMetadataRow(index));
    }
    if let Some(error) = form.metadata_error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }
    action_button(
        buf,
        rows.line(),
        app,
        hits,
        ActionId::AddMetadata,
        ButtonTone::Secondary,
    );

    rows.skip(1);
    widgets::render_section(buf, rows.line(), text::SECTION_ADVANCED);
    multiline_field(
        buf,
        rows,
        app,
        hits,
        FieldId::EditPromptCache,
        text::EDIT_PROMPT_CACHE_PLACEHOLDER,
        5,
    );
    if let Some(error) = form.prompt_cache_error() {
        render_wrapped(buf, rows, error, widgets::error_style());
    }

    rows.skip(1);
    widgets::render_section(buf, rows.line(), text::LABEL_INSTRUCTIONS);
    match app
        .detail()
        .session()
        .and_then(|session| session.agent.instructions.as_deref())
    {
        Some(instructions) if !instructions.trim().is_empty() => {
            render_wrapped(buf, rows, instructions, widgets::muted_style());
        }
        _ => {
            widgets::render_body(buf, rows.line(), text::VALUE_NOT_SET, widgets::dim_style());
        }
    }
    render_wrapped(
        buf,
        rows,
        text::EDIT_INSTRUCTIONS_READONLY,
        widgets::dim_style(),
    );

    if let Some(error) = form.form_error() {
        render_wrapped(buf, rows, error, widgets::error_style());
        if form.offer_cancel() {
            let row = rows.line();
            let label = text::action_label(ActionId::CancelTurn);
            let rect = widgets::button_rect(row, row.x, label, ButtonTone::Danger);
            widgets::render_button(buf, rect, label, false, ButtonTone::Danger);
            hits.region(Layer::Base, rect, Intent::OpenCancelTurn);
        }
    }
    rows.skip(1);
    if form.submitting() {
        widgets::render_body(
            buf,
            rows.line(),
            text::EDIT_SUBMITTING,
            widgets::dim_style(),
        );
    } else {
        action_button(
            buf,
            rows.line(),
            app,
            hits,
            ActionId::EditSubmit,
            ButtonTone::Primary,
        );
    }
    action_button(
        buf,
        rows.line(),
        app,
        hits,
        ActionId::Back,
        ButtonTone::Secondary,
    );
    cursor
}

fn render_edit_metadata_band(
    buf: &mut Buffer,
    band: Rect,
    app: &App,
    hits: &mut HitMap,
    index: usize,
    key: &crate::text_field::TextField,
    value: &crate::text_field::TextField,
) {
    if band.height == 0 || band.width < 4 {
        return;
    }
    let key_width = band.width.saturating_sub(1) * 2 / 5;
    let key_rect = Rect::new(band.x, band.y, key_width, band.height);
    let value_rect = Rect::new(
        band.x.saturating_add(key_width).saturating_add(1),
        band.y,
        band.width.saturating_sub(key_width).saturating_sub(1),
        band.height,
    );
    let key_field = FieldId::EditMetadataKey(index);
    let value_field = FieldId::EditMetadataValue(index);
    widgets::render_field(
        buf,
        key_rect,
        key,
        app.focus() == Focus::Field(key_field),
        false,
        text::METADATA_KEY_PLACEHOLDER,
    );
    widgets::render_field(
        buf,
        value_rect,
        value,
        app.focus() == Focus::Field(value_field),
        false,
        text::METADATA_VALUE_PLACEHOLDER,
    );
    hits.region(
        Layer::Base,
        key_rect,
        Intent::FocusField {
            field: key_field,
            click: None,
        },
    );
    hits.region(
        Layer::Base,
        value_rect,
        Intent::FocusField {
            field: value_field,
            click: None,
        },
    );
}

#[cfg(test)]
#[path = "session_form_test.rs"]
mod tests;
