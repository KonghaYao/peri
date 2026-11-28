//! Form state for onboarding, credential editing, and observability settings.

use crate::config::{self, ObservabilityConfig};
use crate::text_field::TextField;

use super::types::SettingsStatus;

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

/// OpenObserve connection editor state.
///
/// The password field starts empty even when a connection is already stored:
/// the saved password is never rendered back into the form.
#[derive(Clone, Debug)]
pub struct ObservabilityState {
    pub(super) base_url: TextField,
    pub(super) org: TextField,
    pub(super) user: TextField,
    pub(super) password: TextField,
    pub(super) base_url_error: Option<String>,
    pub(super) org_error: Option<String>,
    pub(super) user_error: Option<String>,
    pub(super) password_error: Option<String>,
    pub(super) form_error: Option<String>,
    pub(super) status: Option<SettingsStatus>,
}

impl Default for ObservabilityState {
    fn default() -> Self {
        Self::new(None)
    }
}

impl ObservabilityState {
    /// Prefill from the stored connection, keeping the password blank.
    #[must_use]
    pub fn new(existing: Option<&ObservabilityConfig>) -> Self {
        let base_url = existing.map_or(
            config::DEFAULT_OBSERVABILITY_BASE_URL,
            ObservabilityConfig::base_url,
        );
        let org = existing.map_or(config::DEFAULT_OBSERVABILITY_ORG, ObservabilityConfig::org);
        let user = existing.map_or("", ObservabilityConfig::user);
        Self {
            base_url: TextField::new(base_url),
            org: TextField::new(org),
            user: TextField::new(user),
            password: TextField::new(""),
            base_url_error: None,
            org_error: None,
            user_error: None,
            password_error: None,
            form_error: None,
            status: None,
        }
    }

    /// Base URL field.
    #[must_use]
    pub fn base_url(&self) -> &TextField {
        &self.base_url
    }

    /// Organization field.
    #[must_use]
    pub fn org(&self) -> &TextField {
        &self.org
    }

    /// User field.
    #[must_use]
    pub fn user(&self) -> &TextField {
        &self.user
    }

    /// Password field. Never display the value.
    #[must_use]
    pub fn password(&self) -> &TextField {
        &self.password
    }

    /// Per-field base URL error.
    #[must_use]
    pub fn base_url_error(&self) -> Option<&str> {
        self.base_url_error.as_deref()
    }

    /// Per-field organization error.
    #[must_use]
    pub fn org_error(&self) -> Option<&str> {
        self.org_error.as_deref()
    }

    /// Per-field user error.
    #[must_use]
    pub fn user_error(&self) -> Option<&str> {
        self.user_error.as_deref()
    }

    /// Per-field password error.
    #[must_use]
    pub fn password_error(&self) -> Option<&str> {
        self.password_error.as_deref()
    }

    /// Form-level save error.
    #[must_use]
    pub fn form_error(&self) -> Option<&str> {
        self.form_error.as_deref()
    }

    /// Last test result.
    #[must_use]
    pub fn status(&self) -> Option<&SettingsStatus> {
        self.status.as_ref()
    }
}
