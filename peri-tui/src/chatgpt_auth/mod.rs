//! Host-side ChatGPT authentication service boundary.
//!
//! The TUI never reads or writes credentials itself. This module exposes a small
//! typed capability boundary over `peri_acp::provider::auth::ChatGptAuthManager`
//! so panels can be tested with a fake backend, plus the shared credential-store
//! path rule used by both `peri auth chatgpt` and `/login`.
//!
//! No token, authorization code or URL is logged here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

pub(crate) use peri_acp::provider::auth::{AccountStatus, AuthResult, LogoutOutcome};

/// Frozen credentials target for one authentication action.
///
/// The account reference is the local alias stored in `provider.auth.account`;
/// it is not an identity proof. Verified identity travels as `AccountStatus`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthTarget {
    pub(crate) store: PathBuf,
    pub(crate) account: String,
}

impl AuthTarget {
    pub(crate) fn new(store: PathBuf, account: impl Into<String>) -> Self {
        Self {
            store,
            account: account.into(),
        }
    }
}

/// Resolve the credential directory used by the CLI and the TUI.
///
/// An explicit path is resolved as given; the default is `credentials/chatgpt`
/// beside the selected settings file. The result is absolute, but this helper
/// does not create or inspect the directory.
pub fn resolve_credential_store(
    settings_path: &Path,
    explicit: Option<PathBuf>,
) -> Result<PathBuf> {
    let path = explicit.unwrap_or_else(|| {
        settings_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("credentials/chatgpt")
    });
    std::path::absolute(path).context("resolve credentials directory")
}

/// One manager-backed login attempt.
///
/// `begin` and `finish` must use the same manager instance because the pending
/// attempt lives inside its credentials-MCP server.
#[async_trait]
pub(crate) trait ChatGptAuthSession: Send + Sync {
    /// Start the loopback attempt, publish the authorization URL once, then
    /// wait for the callback and finish the exchange.
    async fn login(
        &self,
        cancellation: CancellationToken,
        on_url: Box<dyn FnOnce(String) + Send>,
    ) -> AuthResult<AccountStatus>;

    async fn status(&self, cancellation: CancellationToken) -> AuthResult<Option<AccountStatus>>;

    async fn logout(&self, cancellation: CancellationToken) -> AuthResult<LogoutOutcome>;
}

/// Production-replaceable backend factory.
pub(crate) trait ChatGptAuthBackend: Send + Sync {
    fn open(&self, target: &AuthTarget) -> AuthResult<Box<dyn ChatGptAuthSession>>;
}

/// Production backend backed by the credentials MCP capability.
pub(crate) struct NativeChatGptAuth;

impl ChatGptAuthBackend for NativeChatGptAuth {
    fn open(&self, target: &AuthTarget) -> AuthResult<Box<dyn ChatGptAuthSession>> {
        let manager = peri_acp::provider::auth::ChatGptAuthManager::new(
            target.store.clone(),
            target.account.clone(),
        )?;
        Ok(Box::new(NativeSession { manager }))
    }
}

struct NativeSession {
    manager: peri_acp::provider::auth::ChatGptAuthManager,
}

#[async_trait]
impl ChatGptAuthSession for NativeSession {
    async fn login(
        &self,
        cancellation: CancellationToken,
        on_url: Box<dyn FnOnce(String) + Send>,
    ) -> AuthResult<AccountStatus> {
        let pending = self.manager.begin_login(cancellation.clone()).await?;
        on_url(pending.authorization_url().to_owned());
        self.manager.finish_login(pending, cancellation).await
    }

    async fn status(&self, cancellation: CancellationToken) -> AuthResult<Option<AccountStatus>> {
        self.manager.status(cancellation).await
    }

    async fn logout(&self, cancellation: CancellationToken) -> AuthResult<LogoutOutcome> {
        self.manager.logout(cancellation).await
    }
}

/// Backend used by new auth controllers.
pub(crate) fn default_backend() -> Arc<dyn ChatGptAuthBackend> {
    Arc::new(NativeChatGptAuth)
}

/// Safe identity projection used by the shared ChatGPT authentication controller.
///
/// Implementations own their configuration draft; authentication only supplies a
/// verified `AccountStatus` and never reads provider settings or credentials.
pub(crate) trait AuthIdentityDraft {
    fn needs_identity(&self) -> bool;
    fn identity_mismatch(&self, status: &AccountStatus) -> bool;
    fn set_identity(&mut self, status: &AccountStatus) -> bool;
}

mod controller;
pub(crate) use controller::{
    AuthAction, AuthController, AuthEffect, AuthFailure, AuthViewState, DetailAction,
    HostConfigAuthScope, LogoutConfirm, LogoutFocus,
};

#[cfg(test)]
pub(crate) use actions::{take_test_dispatches, take_test_url_actions};
#[cfg(test)]
pub(crate) use controller::AuthEvent;

mod actions;
mod dispatch;
pub(crate) use actions::{copy_url, notify_key, notify_logout_outcome, notify_text, open_browser};
pub(crate) use dispatch::{dispatch_login, dispatch_logout, dispatch_status};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_store_wins_and_is_absolute() {
        let resolved = resolve_credential_store(
            Path::new("/tmp/peri/settings.json"),
            Some("relative".into()),
        )
        .unwrap();
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("relative"));
    }

    #[test]
    fn default_store_sits_beside_settings() {
        let resolved =
            resolve_credential_store(Path::new("/tmp/peri/settings.json"), None).unwrap();
        assert_eq!(resolved, PathBuf::from("/tmp/peri/credentials/chatgpt"));
    }
}
