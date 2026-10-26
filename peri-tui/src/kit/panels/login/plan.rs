//! ChatGPT plan draft: editable store/account plus login-verified identity.
//!
//! Subject and client id are never hand-edited. They are written only by a
//! successful login or by an explicit "use this account" action on a verified
//! status result.

use std::path::{Path, PathBuf};

use peri_acp::provider::config::{ProviderAuthConfig, ProviderConfig};

use crate::chatgpt_auth::AccountStatus;
use crate::i18n;

use super::edit_types::{LoginError, LoginField};

/// A saved plan reference, still expressed with schema path semantics.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlanReference {
    pub(super) store: PathBuf,
    pub(super) account: String,
    pub(super) subject: String,
    pub(super) client_id: String,
}

/// Editable plan fields. Empty identity means "not verified yet".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PlanDraft {
    pub(super) store: String,
    pub(super) account: String,
    pub(super) subject: String,
    pub(super) client_id: String,
}

impl PlanDraft {
    pub(super) fn from_reference(
        store: &Path,
        account: &str,
        subject: &str,
        client_id: &str,
    ) -> Self {
        Self {
            store: store.display().to_string(),
            account: account.to_owned(),
            subject: subject.to_owned(),
            client_id: client_id.to_owned(),
        }
    }

    /// Complete saved reference, used for read-only display and tests.
    #[cfg(test)]
    pub(super) fn reference(&self) -> Option<PlanReference> {
        let store = self.store.trim();
        let account = self.account.trim();
        let subject = self.subject.trim();
        let client_id = self.client_id.trim();
        if store.is_empty() || account.is_empty() || subject.is_empty() || client_id.is_empty() {
            return None;
        }
        Some(PlanReference {
            store: PathBuf::from(store),
            account: account.to_owned(),
            subject: subject.to_owned(),
            client_id: client_id.to_owned(),
        })
    }

    /// Changing the store/account invalidates the previous verified identity.
    pub(super) fn clear_identity(&mut self) {
        self.subject.clear();
        self.client_id.clear();
    }

    /// Adopt a verified identity (login result or explicit "use this account").
    pub(super) fn set_identity(&mut self, status: &AccountStatus) {
        if !status.subject.trim().is_empty() {
            self.subject = status.subject.clone();
        }
        if !status.client_id.trim().is_empty() {
            self.client_id = status.client_id.clone();
        }
    }

    /// Whether the draft identity differs from the verified status.
    pub(super) fn identity_mismatch(&self, status: &AccountStatus) -> bool {
        let subject = self.subject.trim();
        let client_id = self.client_id.trim();
        (!subject.is_empty() && subject != status.subject)
            || (!client_id.is_empty() && client_id != status.client_id)
    }

    pub(super) fn needs_identity(&self) -> bool {
        self.subject.trim().is_empty() || self.client_id.trim().is_empty()
    }

    /// Fill a newly selected plan mode with the host default store/account.
    pub(super) fn ensure_defaults(&mut self, default_store: Option<PathBuf>) {
        if self.store.trim().is_empty()
            && let Some(store) = default_store
        {
            self.store = store.display().to_string();
        }
        if self.account.trim().is_empty() {
            self.account = "default".to_owned();
        }
    }

    /// Serialize into the provider. Core validation still runs afterwards.
    pub(super) fn apply(&self, provider: &mut ProviderConfig) -> Result<(), LoginError> {
        let store = self.store.trim();
        if store.is_empty() || !Path::new(store).is_absolute() {
            return Err(LoginError::field(
                LoginField::PlanStore,
                i18n::tr("login-error-store-absolute"),
            ));
        }
        let account = self.account.trim();
        if !valid_account(account) {
            return Err(LoginError::field(
                LoginField::PlanAccount,
                i18n::tr("login-error-account-reference"),
            ));
        }
        if self.needs_identity() {
            return Err(LoginError::field(
                LoginField::AuthActions,
                i18n::tr("login-auth-identity-required"),
            ));
        }
        provider.auth = Some(ProviderAuthConfig::ChatGptPlan {
            store: PathBuf::from(store),
            account: account.to_owned(),
            subject: self.subject.trim().to_owned(),
            client_id: self.client_id.trim().to_owned(),
        });
        // Plan credentials may only be sent to the official endpoint; an empty
        // base URL resolves to core's official default.
        provider.api_key.clear();
        provider.base_url.clear();
        Ok(())
    }
}

/// Account references become file names inside the credentials store.
fn valid_account(account: &str) -> bool {
    !account.is_empty()
        && account.len() <= 128
        && account
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}
