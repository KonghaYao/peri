//! Edit-session form state and local validation.
//!
//! The edit form reuses the create form's widgets but sends a different body:
//! the gateway's update endpoint nests the model under `agent.model`, accepts
//! `metadata` and `prompt_cache_options`, and rejects `instructions` with
//! `deny_unknown_fields`. Instructions therefore stay read-only here.
//!
//! Prompt-cache JSON differs from create: blank omits the field (keep the
//! current value) while a literal `null` clears it.

use std::collections::BTreeMap;

use peri_tui_components::textarea::TextAreaState;
use serde_json::Value;

use crate::api::{update_session_body, IdempotencyKey, Session, UpdateSessionSpec};
use crate::text_field::TextField;
use crate::ui::text;

use super::session_create::{collect_metadata, MetadataRow};

/// Edit form state; rebuilt every time the page opens.
#[derive(Clone, Debug, Default)]
pub struct EditFormState {
    pub(crate) model: TextField,
    pub(crate) metadata: Vec<MetadataRow>,
    pub(crate) prompt_cache_options: TextAreaState,
    pub(crate) model_error: Option<String>,
    pub(crate) metadata_error: Option<String>,
    pub(crate) prompt_cache_error: Option<String>,
    pub(crate) form_error: Option<String>,
    /// The last failure means a running turn must be cancelled first.
    pub(crate) offer_cancel: bool,
    pub(crate) submitting: bool,
    /// Body + key of the submission that is on the wire, for a same-body retry.
    pub(crate) pending: Option<(String, IdempotencyKey)>,
}

impl EditFormState {
    /// Prefill from the last good snapshot.
    #[must_use]
    pub fn from_session(session: &Session) -> Self {
        let metadata = session
            .metadata
            .iter()
            .map(|(key, value)| MetadataRow {
                key: TextField::new(key.as_str()),
                value: TextField::new(value.as_str()),
            })
            .collect();
        let prompt_cache_text = session
            .prompt_cache_options
            .as_ref()
            .and_then(|options| {
                serde_json::to_string(&serde_json::json!({ "ttl": options.ttl })).ok()
            })
            .unwrap_or_default();
        let mut prompt_cache_options = TextAreaState::default();
        prompt_cache_options.text = prompt_cache_text;
        prompt_cache_options.cursor = prompt_cache_options.text.chars().count();
        prompt_cache_options.placeholder = text::EDIT_PROMPT_CACHE_PLACEHOLDER.to_owned();
        Self {
            model: TextField::new(session.agent.model.as_deref().unwrap_or_default()),
            metadata,
            prompt_cache_options,
            ..Self::default()
        }
    }

    /// Model field.
    #[must_use]
    pub fn model(&self) -> &TextField {
        &self.model
    }

    /// Metadata rows.
    #[must_use]
    pub fn metadata(&self) -> &[MetadataRow] {
        &self.metadata
    }

    /// Prompt-cache JSON area.
    #[must_use]
    pub fn prompt_cache_options(&self) -> &TextAreaState {
        &self.prompt_cache_options
    }

    /// Model error.
    #[must_use]
    pub fn model_error(&self) -> Option<&str> {
        self.model_error.as_deref()
    }

    /// Metadata error.
    #[must_use]
    pub fn metadata_error(&self) -> Option<&str> {
        self.metadata_error.as_deref()
    }

    /// Prompt-cache error.
    #[must_use]
    pub fn prompt_cache_error(&self) -> Option<&str> {
        self.prompt_cache_error.as_deref()
    }

    /// Form-level error (submit failure).
    #[must_use]
    pub fn form_error(&self) -> Option<&str> {
        self.form_error.as_deref()
    }

    /// Whether the failure offers the cancel-turn action.
    #[must_use]
    pub fn offer_cancel(&self) -> bool {
        self.offer_cancel
    }

    /// Whether a save is in flight.
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

    /// Clear every error before a new validation pass.
    pub(crate) fn clear_errors(&mut self) {
        self.model_error = None;
        self.metadata_error = None;
        self.prompt_cache_error = None;
        self.form_error = None;
        self.offer_cancel = false;
    }

    /// Validate the form and build the update fields.
    ///
    /// The model is required: the gateway cannot derive it on update.
    pub(crate) fn build_spec(&mut self) -> Option<UpdateSessionSpec> {
        self.clear_errors();
        let model = self.model.value().trim().to_owned();
        if model.is_empty() {
            self.model_error = Some(text::ERR_MODEL_REQUIRED.to_owned());
        }
        let mut metadata = BTreeMap::new();
        if let Err(error) = collect_metadata(&self.metadata, &mut metadata) {
            self.metadata_error = Some(error.to_owned());
        }
        let prompt_cache =
            match parse_prompt_cache(&self.prompt_cache_options, &mut self.prompt_cache_error) {
                Some(value) => value,
                None => {
                    return None;
                }
            };
        if self.model_error.is_some() || self.metadata_error.is_some() {
            return None;
        }
        Some(UpdateSessionSpec {
            model: Some(model),
            // The rows always mirror the form, so an empty map clears metadata.
            metadata: Some(metadata),
            prompt_cache_options: prompt_cache,
        })
    }

    /// Prepare one submission, reusing the key when the body is unchanged.
    pub(crate) fn prepare_submission(&mut self) -> Option<(UpdateSessionSpec, IdempotencyKey)> {
        let spec = self.build_spec()?;
        let body = update_session_body(&spec).ok()?;
        let body_text = serde_json::to_string(&body).unwrap_or_default();
        let key = match &self.pending {
            Some((pending_body, key)) if *pending_body == body_text => key.clone(),
            _ => {
                let key = IdempotencyKey::new();
                self.pending = Some((body_text, key.clone()));
                key
            }
        };
        self.submitting = true;
        Some((spec, key))
    }

    /// Record a finished attempt.
    pub(crate) fn finish(&mut self, success: bool) {
        self.submitting = false;
        if success {
            self.pending = None;
        }
    }
}

/// Parse the prompt-cache area for an update.
///
/// `Some(None)` keeps the field, `Some(Some(Value::Null))` clears it and
/// `Some(Some(object))` replaces it. `None` reports a parse failure.
fn parse_prompt_cache(area: &TextAreaState, error: &mut Option<String>) -> Option<Option<Value>> {
    let text = area.text.trim();
    if text.is_empty() {
        return Some(None);
    }
    match serde_json::from_str::<Value>(text) {
        Ok(value) => Some(Some(value)),
        Err(parse) => {
            *error = Some(text::advanced_json_invalid(
                text::field_label(crate::app::FieldId::EditPromptCache),
                &parse.to_string(),
            ));
            None
        }
    }
}

#[cfg(test)]
#[path = "session_edit_test.rs"]
mod tests;
