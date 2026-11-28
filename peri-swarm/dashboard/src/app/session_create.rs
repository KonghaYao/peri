//! Create-session form state and local validation.
//!
//! Validation runs before any request. Only the fields the user filled are
//! sent; blank optional fields are omitted so the gateway can apply its
//! defaults.

use std::collections::BTreeMap;

use peri_tui_components::textarea::TextAreaState;
use serde_json::Value;

use crate::api::CreateSessionSpec;
use crate::text_field::TextField;
use crate::ui::text;

/// Maximum logical id length accepted by the gateway.
pub const LOGICAL_ID_MAX: usize = 128;

/// One metadata key/value row.
#[derive(Clone, Debug, Default)]
pub struct MetadataRow {
    pub(crate) key: TextField,
    pub(crate) value: TextField,
}

impl MetadataRow {
    /// Metadata key field.
    #[must_use]
    pub fn key(&self) -> &TextField {
        &self.key
    }

    /// Metadata value field.
    #[must_use]
    pub fn value(&self) -> &TextField {
        &self.value
    }
}

/// Create form state; a fresh value is used for every new submission.
#[derive(Clone, Debug, Default)]
pub struct CreateFormState {
    pub(crate) logical_id: TextField,
    pub(crate) model: TextField,
    pub(crate) cwd: TextField,
    pub(crate) instructions: TextAreaState,
    pub(crate) metadata: Vec<MetadataRow>,
    pub(crate) advanced_open: bool,
    pub(crate) tools: TextAreaState,
    pub(crate) environment: TextAreaState,
    pub(crate) prompt_cache_options: TextAreaState,
    pub(crate) logical_id_error: Option<String>,
    pub(crate) metadata_error: Option<String>,
    pub(crate) tools_error: Option<String>,
    pub(crate) environment_error: Option<String>,
    pub(crate) prompt_cache_error: Option<String>,
    pub(crate) form_error: Option<String>,
    pub(crate) submitting: bool,
}

impl CreateFormState {
    /// Logical id field.
    #[must_use]
    pub fn logical_id(&self) -> &TextField {
        &self.logical_id
    }

    /// Model field.
    #[must_use]
    pub fn model(&self) -> &TextField {
        &self.model
    }

    /// Working-directory field.
    #[must_use]
    pub fn cwd(&self) -> &TextField {
        &self.cwd
    }

    /// Instructions area.
    #[must_use]
    pub fn instructions(&self) -> &TextAreaState {
        &self.instructions
    }

    /// Metadata rows.
    #[must_use]
    pub fn metadata(&self) -> &[MetadataRow] {
        &self.metadata
    }

    /// Whether the advanced JSON section is expanded.
    #[must_use]
    pub fn advanced_open(&self) -> bool {
        self.advanced_open
    }

    /// Advanced `tools` area.
    #[must_use]
    pub fn tools(&self) -> &TextAreaState {
        &self.tools
    }

    /// Advanced `environment` area.
    #[must_use]
    pub fn environment(&self) -> &TextAreaState {
        &self.environment
    }

    /// Advanced `prompt_cache_options` area.
    #[must_use]
    pub fn prompt_cache_options(&self) -> &TextAreaState {
        &self.prompt_cache_options
    }

    /// Per-field logical id error.
    #[must_use]
    pub fn logical_id_error(&self) -> Option<&str> {
        self.logical_id_error.as_deref()
    }

    /// Metadata row error.
    #[must_use]
    pub fn metadata_error(&self) -> Option<&str> {
        self.metadata_error.as_deref()
    }

    /// Advanced `tools` error.
    #[must_use]
    pub fn tools_error(&self) -> Option<&str> {
        self.tools_error.as_deref()
    }

    /// Advanced `environment` error.
    #[must_use]
    pub fn environment_error(&self) -> Option<&str> {
        self.environment_error.as_deref()
    }

    /// Advanced `prompt_cache_options` error.
    #[must_use]
    pub fn prompt_cache_error(&self) -> Option<&str> {
        self.prompt_cache_error.as_deref()
    }

    /// Form-level error (submit failure).
    #[must_use]
    pub fn form_error(&self) -> Option<&str> {
        self.form_error.as_deref()
    }

    /// Whether a create request is in flight.
    #[must_use]
    pub fn submitting(&self) -> bool {
        self.submitting
    }

    /// Append an empty metadata row.
    pub(crate) fn add_row(&mut self) {
        self.metadata.push(MetadataRow::default());
        self.metadata_error = None;
    }

    /// Remove a metadata row.
    pub(crate) fn remove_row(&mut self, index: usize) {
        if index < self.metadata.len() {
            self.metadata.remove(index);
        }
        self.metadata_error = None;
    }

    /// Expand or collapse the advanced JSON section.
    pub(crate) fn toggle_advanced(&mut self) {
        self.advanced_open = !self.advanced_open;
    }

    /// Clear every error before a new validation pass.
    pub(crate) fn clear_errors(&mut self) {
        self.logical_id_error = None;
        self.metadata_error = None;
        self.tools_error = None;
        self.environment_error = None;
        self.prompt_cache_error = None;
        self.form_error = None;
    }

    /// Validate the form and build the request fields.
    ///
    /// Returns `None` after populating the inline errors when anything is
    /// invalid. `stream` is added by the API body builder.
    pub(crate) fn build_spec(&mut self) -> Option<CreateSessionSpec> {
        self.clear_errors();
        let logical_id = self.logical_id.value().trim().to_owned();
        if !logical_id.is_empty() && !valid_logical_id(&logical_id) {
            self.logical_id_error = Some(text::ERR_LOGICAL_ID_INVALID.to_owned());
        }

        let mut metadata = BTreeMap::new();
        if let Err(error) = collect_metadata(&self.metadata, &mut metadata) {
            self.metadata_error = Some(error.to_owned());
        }

        let tools = parse_advanced(
            text::field_label(crate::app::FieldId::CreateTools),
            &self.tools,
            &mut self.tools_error,
        );
        let environment = parse_advanced(
            text::field_label(crate::app::FieldId::CreateEnvironment),
            &self.environment,
            &mut self.environment_error,
        );
        let prompt_cache = parse_advanced(
            text::field_label(crate::app::FieldId::CreatePromptCache),
            &self.prompt_cache_options,
            &mut self.prompt_cache_error,
        );

        if self.logical_id_error.is_some()
            || self.metadata_error.is_some()
            || self.tools_error.is_some()
            || self.environment_error.is_some()
            || self.prompt_cache_error.is_some()
        {
            return None;
        }

        Some(CreateSessionSpec {
            logical_id: non_blank(&logical_id),
            cwd: non_blank(self.cwd.value()),
            model: non_blank(self.model.value()),
            instructions: non_blank(self.instructions.text.as_str()),
            metadata,
            tools: tools.flatten(),
            environment: environment.flatten(),
            prompt_cache_options: prompt_cache.flatten(),
        })
    }
}

/// Collect metadata rows into a map, or report the first problem.
///
/// Blank keys with a value and duplicate keys are rejected; a fully blank row
/// is ignored. Shared by the create and edit forms.
pub(crate) fn collect_metadata(
    rows: &[MetadataRow],
    metadata: &mut BTreeMap<String, String>,
) -> Result<(), &'static str> {
    for row in rows {
        let key = row.key.value().trim().to_owned();
        let value = row.value.value().trim().to_owned();
        if key.is_empty() {
            if !value.is_empty() {
                return Err(text::ERR_METADATA_KEY_REQUIRED);
            }
            continue;
        }
        if metadata.insert(key, value).is_some() {
            return Err(text::ERR_METADATA_KEY_DUPLICATE);
        }
    }
    Ok(())
}

/// Whether a non-empty logical id matches `^[A-Za-z0-9_-]{1,128}$`.
#[must_use]
pub fn valid_logical_id(value: &str) -> bool {
    let length = value.chars().count();
    (1..=LOGICAL_ID_MAX).contains(&length)
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

fn non_blank(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Parse one advanced JSON area.
///
/// Blank means "omit"; an explicit `null` also means "omit" for create. Both
/// return `Some(None)` so the caller can distinguish omission from a failure.
fn parse_advanced(
    label: &str,
    area: &TextAreaState,
    error: &mut Option<String>,
) -> Option<Option<Value>> {
    let text = area.text.trim();
    if text.is_empty() {
        return Some(None);
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Null) => Some(None),
        Ok(value) => Some(Some(value)),
        Err(parse) => {
            *error = Some(text::advanced_json_invalid(label, &parse.to_string()));
            None
        }
    }
}

#[cfg(test)]
#[path = "session_create_test.rs"]
mod tests;
