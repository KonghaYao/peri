//! Form state for onboarding and credential editing.

use crate::config;
use crate::text_field::TextField;

/// Onboarding form state.
#[derive(Clone, Debug)]
pub struct OnboardingState {
    pub(super) base_url: TextField,
    pub(super) api_key: TextField,
    pub(super) base_url_error: Option<String>,
    pub(super) api_key_error: Option<String>,
    pub(super) form_error: Option<String>,
}

impl Default for OnboardingState {
    fn default() -> Self {
        Self {
            base_url: TextField::new(config::DEFAULT_BASE_URL),
            api_key: TextField::new(""),
            base_url_error: None,
            api_key_error: None,
            form_error: None,
        }
    }
}

impl OnboardingState {
    /// Base URL field.
    #[must_use]
    pub fn base_url(&self) -> &TextField {
        &self.base_url
    }

    /// API key field. Never display the value.
    #[must_use]
    pub fn api_key(&self) -> &TextField {
        &self.api_key
    }

    /// Per-field base URL error.
    #[must_use]
    pub fn base_url_error(&self) -> Option<&str> {
        self.base_url_error.as_deref()
    }

    /// Per-field API key error.
    #[must_use]
    pub fn api_key_error(&self) -> Option<&str> {
        self.api_key_error.as_deref()
    }

    /// Form-level validation or save error.
    #[must_use]
    pub fn form_error(&self) -> Option<&str> {
        self.form_error.as_deref()
    }
}

/// Credential editor form state.
#[derive(Clone, Debug, Default)]
pub struct CredentialsState {
    pub(super) base_url: TextField,
    pub(super) api_key: TextField,
    pub(super) base_url_error: Option<String>,
    pub(super) api_key_error: Option<String>,
    pub(super) form_error: Option<String>,
}

impl CredentialsState {
    /// Base URL field.
    #[must_use]
    pub fn base_url(&self) -> &TextField {
        &self.base_url
    }

    /// API key field; blank keeps the stored key.
    #[must_use]
    pub fn api_key(&self) -> &TextField {
        &self.api_key
    }

    /// Per-field base URL error.
    #[must_use]
    pub fn base_url_error(&self) -> Option<&str> {
        self.base_url_error.as_deref()
    }

    /// Per-field API key error.
    #[must_use]
    pub fn api_key_error(&self) -> Option<&str> {
        self.api_key_error.as_deref()
    }

    /// Form-level validation or save error.
    #[must_use]
    pub fn form_error(&self) -> Option<&str> {
        self.form_error.as_deref()
    }
}

/// Credentials captured at submit time so later edits cannot change what is saved.
pub(super) struct PendingCredentials {
    pub(super) base_url: String,
    pub(super) api_key: String,
}
