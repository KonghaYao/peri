//! Login ChatGPT authentication controller.
//!
//! Owns the auth view state, one completion gate for the frozen target and the
//! bounded inbox used by background tasks. All credential I/O stays behind
//! [`crate::chatgpt_auth`]; results are safe DTOs only.
//!
//! Cancellation semantics:
//! - login/status cancel invalidates the ticket; reconciliation status uses a
//!   new generation, so a late login result cannot write the new view;
//! - logout cancel only stops the remote revocation wait, the same ticket stays
//!   current so the local-clear outcome is still delivered.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use peri_acp::provider::auth::{AccountStatus, AuthError, AuthResult, LogoutOutcome};
use ratatui_kit::prelude::State;

use crate::application::operations::{CompletionGate, OperationTicket, OutcomeUnknownReason};
use crate::chatgpt_auth::{AuthTarget, ChatGptAuthBackend, ChatGptAuthSession, default_backend};
use crate::kit::atoms::AUTH_EVENT_REVISION;
use crate::kit::panels::login::plan::PlanDraft;

#[path = "auth_actions.rs"]
mod auth_actions;

#[cfg(test)]
use auth_actions::intercept_dispatch;
pub(super) use auth_actions::{
    copy_url, notify_key, notify_logout_outcome, notify_text, open_browser,
};
#[cfg(test)]
pub(super) use auth_actions::{take_test_dispatches, take_test_url_actions};

/// Host-scoped request identity: config authority + frozen credentials target.
///
/// Session ids are deliberately not reused here; host configuration does not
/// belong to a conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct HostConfigAuthScope {
    source: String,
    store: PathBuf,
    account: String,
}

impl HostConfigAuthScope {
    pub(super) fn new(source: impl Into<String>, target: AuthTarget) -> Self {
        Self {
            source: source.into(),
            store: target.store,
            account: target.account,
        }
    }

    pub(super) fn target(&self) -> Option<AuthTarget> {
        if self.store.as_os_str().is_empty() || self.account.trim().is_empty() {
            return None;
        }
        Some(AuthTarget::new(
            self.store.clone(),
            self.account.trim().to_owned(),
        ))
    }
}

/// Authentication failure categories. Server-provided descriptions are never
/// displayed because they may contain sensitive content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AuthFailure {
    Storage,
    PlatformUnsupported,
    Unavailable,
    Denied,
    Callback,
    Identity,
    NoPlanScope,
    TargetRequired,
    Ended,
    Generic,
}

impl AuthFailure {
    pub(super) fn i18n_key(self) -> &'static str {
        match self {
            Self::Storage => "login-auth-error-storage",
            Self::PlatformUnsupported => "login-auth-platform-unsupported",
            Self::Unavailable => "login-auth-error-unavailable",
            Self::Denied => "login-auth-error-denied",
            Self::Callback => "login-auth-error-callback",
            Self::Identity => "login-auth-error-identity",
            Self::NoPlanScope => "login-auth-no-plan-scope",
            Self::TargetRequired => "login-auth-target-required",
            Self::Ended => "login-auth-ended",
            Self::Generic => "login-auth-error-generic",
        }
    }

    fn from_error(error: &AuthError) -> Self {
        match error {
            AuthError::Cancelled => Self::Ended,
            AuthError::InvalidReference | AuthError::Storage => Self::Storage,
            AuthError::UnsupportedPlatform => Self::PlatformUnsupported,
            AuthError::Unavailable => Self::Unavailable,
            AuthError::AccessDenied => Self::Denied,
            AuthError::InvalidCallback => Self::Callback,
            AuthError::AccountChanged | AuthError::InvalidIdToken => Self::Identity,
            AuthError::MissingScope => Self::NoPlanScope,
            AuthError::OAuth { code, .. } => {
                // Never log the description or the raw response body.
                tracing::warn!(code = %code, "ChatGPT OAuth request failed");
                Self::Generic
            }
            AuthError::LoginRequired | AuthError::InvalidResponse => Self::Generic,
        }
    }
}

/// Current authentication step and its safe display payload.
#[derive(Clone)]
pub(super) enum AuthViewState {
    Unchecked,
    Checking,
    SignedOut,
    SignedIn(AccountStatus),
    StartingLogin,
    WaitingCallback { url: String },
    Cancelling,
    LoggingOut,
    CancellingLogout,
    LoggedOut(LogoutOutcome),
    OutcomeUnknown(OutcomeUnknownReason),
    Failed(AuthFailure),
}

impl AuthViewState {
    /// Credential-mutating operation in flight. Status checks do not block a
    /// configuration save.
    pub(super) fn blocks_config_save(&self) -> bool {
        matches!(
            self,
            Self::StartingLogin
                | Self::WaitingCallback { .. }
                | Self::Cancelling
                | Self::LoggingOut
                | Self::CancellingLogout
        )
    }

    pub(super) fn url(&self) -> Option<&str> {
        match self {
            Self::WaitingCallback { url } => Some(url),
            _ => None,
        }
    }
}

/// Actions in the plan authentication button group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AuthAction {
    Login,
    Status,
    UseAccount,
    Logout,
    CancelLogout,
}

impl AuthAction {
    pub(super) fn i18n_key(self) -> &'static str {
        match self {
            Self::Login => "login-auth-login",
            Self::Status => "login-auth-status",
            Self::UseAccount => "login-auth-use-account",
            Self::Logout => "login-auth-logout",
            Self::CancelLogout => "login-auth-cancel-logout-wait",
        }
    }
}

/// Buttons in the dedicated authorization view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DetailAction {
    OpenBrowser,
    CopyUrl,
    CancelLogin,
}

impl DetailAction {
    pub(super) fn i18n_key(self) -> &'static str {
        match self {
            Self::OpenBrowser => "login-auth-open-browser",
            Self::CopyUrl => "login-auth-copy-url",
            Self::CancelLogin => "login-auth-cancel",
        }
    }
}

impl DetailAction {
    fn cycle(self, forward: bool) -> Self {
        match (self, forward) {
            (Self::OpenBrowser, true) => Self::CopyUrl,
            (Self::CopyUrl, true) => Self::CancelLogin,
            (Self::CancelLogin, true) => Self::OpenBrowser,
            (Self::OpenBrowser, false) => Self::CancelLogin,
            (Self::CancelLogin, false) => Self::CopyUrl,
            (Self::CopyUrl, false) => Self::OpenBrowser,
        }
    }
}

/// Logout confirmation focus; defaults to cancel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LogoutFocus {
    Cancel,
    Confirm,
}

#[derive(Clone)]
pub(super) struct LogoutConfirm {
    pub(super) target: AuthTarget,
    pub(super) providers: Vec<String>,
    pub(super) focus: LogoutFocus,
}

/// Outcome of one delivered auth event, consumed by the panel owner.
#[derive(Clone)]
pub(super) enum AuthEffect {
    LoginSucceeded,
    LoginFailed(AuthFailure),
    LoginCancelled,
    LoggedOut(LogoutOutcome),
    LogoutUnknown,
    /// Terminal logout result delivered after the owner detached from the wait.
    /// Only the notification is emitted; the current view is left untouched.
    LoggedOutDetached(LogoutOutcome),
    LogoutUnknownDetached,
    StatusChecked,
    StatusFailed(AuthFailure),
    OutcomeUnknown(OutcomeUnknownReason),
}

pub(super) enum AuthEvent {
    AuthorizationUrl(String),
    LoginFinished(AuthResult<AccountStatus>),
    StatusChecked(AuthResult<Option<AccountStatus>>),
    LoggedOut(AuthResult<LogoutOutcome>),
}

type Inbox = Arc<Mutex<Vec<(OperationTicket<HostConfigAuthScope>, AuthEvent)>>>;

/// Prepared async request; the panel dispatches it so tests can intercept.
pub(super) struct AuthRequest {
    pub(super) ticket: OperationTicket<HostConfigAuthScope>,
    session: Box<dyn ChatGptAuthSession>,
}

pub(super) struct AuthController {
    backend: Arc<dyn ChatGptAuthBackend>,
    inbox: Inbox,
    observer: Arc<AtomicBool>,
    gate: CompletionGate<HostConfigAuthScope>,
    scope: HostConfigAuthScope,
    view: AuthViewState,
    login_succeeded: bool,
    action_index: usize,
    detail_focus: DetailAction,
    logout_confirm: Option<LogoutConfirm>,
    status_reconcile: bool,
    /// Logout generations whose owner stopped waiting before the terminal
    /// result. Their results still notify once, but never write the live view.
    detached_logouts: BTreeSet<u64>,
}

impl AuthController {
    pub(super) fn new() -> Self {
        Self::with_backend(default_backend())
    }

    pub(super) fn with_backend(backend: Arc<dyn ChatGptAuthBackend>) -> Self {
        Self {
            backend,
            inbox: Arc::new(Mutex::new(Vec::new())),
            observer: Arc::new(AtomicBool::new(true)),
            gate: CompletionGate::default(),
            scope: HostConfigAuthScope::default(),
            view: AuthViewState::Unchecked,
            login_succeeded: false,
            action_index: 0,
            detail_focus: DetailAction::OpenBrowser,
            logout_confirm: None,
            status_reconcile: false,
            detached_logouts: BTreeSet::new(),
        }
    }

    pub(super) fn view(&self) -> &AuthViewState {
        &self.view
    }

    pub(super) fn scope(&self) -> &HostConfigAuthScope {
        &self.scope
    }

    pub(super) fn login_succeeded(&self) -> bool {
        self.login_succeeded
    }

    pub(super) fn blocks_config_save(&self) -> bool {
        self.view.blocks_config_save()
    }

    pub(super) fn outcome_unknown(&self) -> bool {
        matches!(self.view, AuthViewState::OutcomeUnknown(_))
    }

    /// Adopt a new frozen target. A changed target cancels old work and clears
    /// every target-specific display; same-target calls are no-ops.
    pub(super) fn sync_scope(&mut self, scope: HostConfigAuthScope) -> bool {
        if self.scope == scope {
            return false;
        }
        if self.view.blocks_config_save() {
            return false;
        }
        self.gate.reset_scope(scope.clone());
        self.scope = scope;
        self.view = AuthViewState::Unchecked;
        self.login_succeeded = false;
        self.action_index = 0;
        self.detail_focus = DetailAction::OpenBrowser;
        self.logout_confirm = None;
        self.status_reconcile = false;
        true
    }

    /// Store/account text changed: stop showing stale status until re-checked.
    pub(super) fn invalidate_target(&mut self) {
        if self.view.blocks_config_save() {
            return;
        }
        self.gate.cancel();
        self.view = AuthViewState::Unchecked;
        self.login_succeeded = false;
        self.status_reconcile = false;
    }

    /// No status result for the frozen target yet.
    pub(super) fn needs_status_query(&self) -> bool {
        matches!(self.view, AuthViewState::Unchecked)
    }

    pub(super) fn available_actions(&self, plan: &PlanDraft) -> Vec<AuthAction> {
        if matches!(
            self.view,
            AuthViewState::LoggingOut | AuthViewState::CancellingLogout
        ) {
            return vec![AuthAction::CancelLogout];
        }
        if self.view.blocks_config_save() {
            return Vec::new();
        }
        if self.gate.is_pending() {
            return if matches!(self.view, AuthViewState::Checking) {
                vec![AuthAction::Status]
            } else {
                Vec::new()
            };
        }
        let mut actions = vec![AuthAction::Login, AuthAction::Status];
        if let AuthViewState::SignedIn(status) = &self.view
            && (plan.needs_identity() || plan.identity_mismatch(status))
        {
            actions.push(AuthAction::UseAccount);
        }
        actions.push(AuthAction::Logout);
        actions
    }

    pub(super) fn selected_action(&self, plan: &PlanDraft) -> Option<AuthAction> {
        let actions = self.available_actions(plan);
        if actions.is_empty() {
            return None;
        }
        actions
            .get(self.action_index.min(actions.len() - 1))
            .copied()
    }

    /// 鼠标点击动作行：只有该动作当前可用时才选中，随后由调用方执行同一 Enter 动作。
    pub(super) fn select_action(&mut self, action: AuthAction, plan: &PlanDraft) -> bool {
        let actions = self.available_actions(plan);
        let Some(index) = actions.iter().position(|candidate| *candidate == action) else {
            return false;
        };
        self.action_index = index;
        true
    }

    pub(super) fn cycle_action(&mut self, forward: bool, plan: &PlanDraft) {
        let actions = self.available_actions(plan);
        if actions.is_empty() {
            self.action_index = 0;
            return;
        }
        self.action_index = if forward {
            (self.action_index + 1) % actions.len()
        } else {
            (self.action_index + actions.len() - 1) % actions.len()
        };
    }

    /// Explicitly adopt the verified identity for the current credentials scope.
    ///
    /// Refuses to write an identity read from another store/account: showing a
    /// stale "signed in" must never leak into the draft being saved.
    pub(super) fn use_account(
        &mut self,
        plan: &mut PlanDraft,
        current_scope: &HostConfigAuthScope,
    ) -> bool {
        if &self.scope != current_scope {
            return false;
        }
        let AuthViewState::SignedIn(status) = &self.view else {
            return false;
        };
        plan.set_identity(status)
    }

    pub(super) fn detail_focus(&self) -> DetailAction {
        let actions = self.detail_actions();
        if actions.contains(&self.detail_focus) {
            self.detail_focus
        } else {
            actions
                .first()
                .copied()
                .unwrap_or(DetailAction::CancelLogin)
        }
    }

    pub(super) fn detail_actions(&self) -> Vec<DetailAction> {
        let mut actions = Vec::new();
        if self.view.url().is_some() {
            actions.push(DetailAction::OpenBrowser);
            actions.push(DetailAction::CopyUrl);
        }
        actions.push(DetailAction::CancelLogin);
        actions
    }

    pub(super) fn cycle_detail(&mut self, forward: bool) {
        let actions = self.detail_actions();
        if actions.is_empty() {
            return;
        }
        if !actions.contains(&self.detail_focus) {
            self.detail_focus = actions[0];
            return;
        }
        self.detail_focus = self.detail_focus.cycle(forward);
        if !actions.contains(&self.detail_focus) {
            self.detail_focus = actions[0];
        }
    }

    pub(super) fn set_detail_focus(&mut self, action: DetailAction) {
        if self.detail_actions().contains(&action) {
            self.detail_focus = action;
        }
    }

    pub(super) fn open_logout_confirm(
        &mut self,
        target: AuthTarget,
        providers: Vec<String>,
    ) -> bool {
        if self.view.blocks_config_save() {
            return false;
        }
        self.logout_confirm = Some(LogoutConfirm {
            target,
            providers,
            focus: LogoutFocus::Cancel,
        });
        true
    }

    pub(super) fn logout_confirm(&self) -> Option<&LogoutConfirm> {
        self.logout_confirm.as_ref()
    }

    pub(super) fn logout_confirm_mut(&mut self) -> Option<&mut LogoutConfirm> {
        self.logout_confirm.as_mut()
    }

    pub(super) fn clear_logout_confirm(&mut self) {
        self.logout_confirm = None;
    }

    /// Prepare a login attempt. The caller spawns it.
    pub(super) fn prepare_login(&mut self) -> Result<AuthRequest, AuthFailure> {
        if self.view.blocks_config_save() || self.gate.is_pending() {
            return Err(AuthFailure::Generic);
        }
        let target = match self.scope.target() {
            Some(target) => target,
            None => return Err(self.fail(AuthFailure::TargetRequired)),
        };
        let session = match self.backend.open(&target) {
            Ok(session) => session,
            Err(error) => {
                let failure = AuthFailure::from_error(&error);
                tracing::warn!(?failure, "ChatGPT login backend unavailable");
                return Err(self.fail(failure));
            }
        };
        let ticket = self.gate.begin(self.scope.clone());
        self.view = AuthViewState::StartingLogin;
        self.login_succeeded = false;
        self.status_reconcile = false;
        // No URL yet: the only actionable button is cancel.
        self.detail_focus = DetailAction::CancelLogin;
        Ok(AuthRequest { ticket, session })
    }

    /// Prepare a status query. `reconcile` marks the post-cancel check: a
    /// failure then means the real outcome is unknown, not "signed out".
    pub(super) fn prepare_status(&mut self, reconcile: bool) -> Option<AuthRequest> {
        if matches!(
            self.view,
            AuthViewState::StartingLogin
                | AuthViewState::WaitingCallback { .. }
                | AuthViewState::LoggingOut
                | AuthViewState::CancellingLogout
        ) {
            return None;
        }
        let target = match self.scope.target() {
            Some(target) => target,
            None => {
                if reconcile {
                    self.view = AuthViewState::OutcomeUnknown(OutcomeUnknownReason::WaitCancelled);
                }
                return None;
            }
        };
        let session = match self.backend.open(&target) {
            Ok(session) => session,
            Err(error) => {
                let failure = AuthFailure::from_error(&error);
                tracing::warn!(?failure, "ChatGPT status backend unavailable");
                self.view = if reconcile {
                    AuthViewState::OutcomeUnknown(OutcomeUnknownReason::WaitCancelled)
                } else {
                    AuthViewState::Failed(failure)
                };
                return None;
            }
        };
        let ticket = self.gate.begin(self.scope.clone());
        self.view = AuthViewState::Checking;
        self.status_reconcile = reconcile;
        Some(AuthRequest { ticket, session })
    }

    /// Prepare logout using the frozen confirmation target.
    pub(super) fn prepare_logout(&mut self) -> Option<AuthRequest> {
        if self.view.blocks_config_save() {
            return None;
        }
        let target = self
            .logout_confirm
            .as_ref()
            .map(|confirm| confirm.target.clone())
            .or_else(|| self.scope.target())?;
        let session = match self.backend.open(&target) {
            Ok(session) => session,
            Err(error) => {
                let failure = AuthFailure::from_error(&error);
                tracing::warn!(?failure, "ChatGPT logout backend unavailable");
                self.view = AuthViewState::Failed(failure);
                return None;
            }
        };
        let ticket = self.gate.begin(self.scope.clone());
        self.view = AuthViewState::LoggingOut;
        self.status_reconcile = false;
        self.logout_confirm = None;
        Some(AuthRequest { ticket, session })
    }

    /// Cancel login/status waiting; the caller then issues a reconciliation
    /// status query with a new ticket.
    pub(super) fn cancel_login(&mut self) -> bool {
        if !matches!(
            self.view,
            AuthViewState::StartingLogin | AuthViewState::WaitingCallback { .. }
        ) {
            return false;
        }
        self.gate.cancel();
        self.view = AuthViewState::Cancelling;
        self.status_reconcile = true;
        true
    }

    /// Stop waiting for remote revocation while keeping the logout ticket, so
    /// the local-clear result is still delivered.
    pub(super) fn cancel_logout_wait(&mut self) -> bool {
        if !matches!(self.view, AuthViewState::LoggingOut) {
            return false;
        }
        if !self.gate.cancel_wait() {
            return false;
        }
        self.view = AuthViewState::CancellingLogout;
        true
    }

    /// Drain queued background events, applying only current tickets.
    pub(super) fn drain(&mut self) -> Vec<AuthEffect> {
        let events = {
            let mut inbox = lock_inbox(&self.inbox);
            std::mem::take(&mut *inbox)
        };
        events
            .into_iter()
            .filter_map(|(ticket, event)| self.receive(ticket, event))
            .collect()
    }

    pub(super) fn receive(
        &mut self,
        ticket: OperationTicket<HostConfigAuthScope>,
        event: AuthEvent,
    ) -> Option<AuthEffect> {
        match event {
            AuthEvent::AuthorizationUrl(url) => {
                if !self.gate.is_current(&ticket) {
                    tracing::debug!("dropping stale ChatGPT authorization URL");
                    return None;
                }
                let url = sanitize_url(url);
                self.detail_focus = DetailAction::OpenBrowser;
                self.view = AuthViewState::WaitingCallback { url };
                None
            }
            AuthEvent::LoginFinished(result) => {
                if !self.gate.accept(&ticket) {
                    tracing::debug!("dropping stale ChatGPT login result");
                    return None;
                }
                match result {
                    Ok(status) => {
                        self.login_succeeded = true;
                        self.view = AuthViewState::SignedIn(status.clone());
                        Some(AuthEffect::LoginSucceeded)
                    }
                    Err(AuthError::Cancelled) => {
                        self.view = AuthViewState::Failed(AuthFailure::Ended);
                        Some(AuthEffect::LoginCancelled)
                    }
                    Err(error) => {
                        let failure = AuthFailure::from_error(&error);
                        tracing::warn!(?failure, "ChatGPT login failed");
                        self.view = AuthViewState::Failed(failure);
                        Some(AuthEffect::LoginFailed(failure))
                    }
                }
            }
            AuthEvent::StatusChecked(result) => {
                if !self.gate.accept(&ticket) {
                    tracing::debug!("dropping stale ChatGPT status result");
                    return None;
                }
                let reconcile = std::mem::take(&mut self.status_reconcile);
                match result {
                    Ok(Some(status)) => {
                        self.view = AuthViewState::SignedIn(status.clone());
                        Some(AuthEffect::StatusChecked)
                    }
                    Ok(None) => {
                        self.login_succeeded = false;
                        if reconcile {
                            // The cancelled finish may persist credentials after
                            // this point-in-time read; do not claim "signed out".
                            self.view =
                                AuthViewState::OutcomeUnknown(OutcomeUnknownReason::WaitCancelled);
                            Some(AuthEffect::OutcomeUnknown(
                                OutcomeUnknownReason::WaitCancelled,
                            ))
                        } else {
                            self.view = AuthViewState::SignedOut;
                            Some(AuthEffect::StatusChecked)
                        }
                    }
                    Err(error) => {
                        let failure = AuthFailure::from_error(&error);
                        tracing::warn!(?failure, "ChatGPT status check failed");
                        if reconcile {
                            self.view =
                                AuthViewState::OutcomeUnknown(OutcomeUnknownReason::WaitCancelled);
                            Some(AuthEffect::OutcomeUnknown(
                                OutcomeUnknownReason::WaitCancelled,
                            ))
                        } else {
                            self.view = AuthViewState::Failed(failure);
                            Some(AuthEffect::StatusFailed(failure))
                        }
                    }
                }
            }
            AuthEvent::LoggedOut(result) => {
                let generation = ticket.generation();
                let accepted = self.gate.accept(&ticket);
                let detached = self.detached_logouts.remove(&generation);
                if !accepted && !detached {
                    tracing::debug!("dropping stale ChatGPT logout result");
                    return None;
                }
                match result {
                    Ok(outcome) => {
                        if detached {
                            // The owner already left the wait: notify the local
                            // clear / remote outcome without touching the view.
                            return Some(AuthEffect::LoggedOutDetached(outcome));
                        }
                        self.login_succeeded = false;
                        self.view = AuthViewState::LoggedOut(outcome.clone());
                        Some(AuthEffect::LoggedOut(outcome))
                    }
                    Err(_) => {
                        tracing::warn!("ChatGPT logout result unknown");
                        if detached {
                            return Some(AuthEffect::LogoutUnknownDetached);
                        }
                        self.view =
                            AuthViewState::OutcomeUnknown(OutcomeUnknownReason::WaitCancelled);
                        Some(AuthEffect::LogoutUnknown)
                    }
                }
            }
        }
    }

    fn fail(&mut self, failure: AuthFailure) -> AuthFailure {
        self.view = AuthViewState::Failed(failure);
        failure
    }

    /// Panel close/edit exit: stop all waits. A logout already in flight keeps its
    /// ticket and inbox slot, so the terminal local-clear result still reaches the
    /// owner exactly once (notification only; the view is left untouched).
    pub(super) fn shutdown(&mut self) {
        if matches!(
            self.view,
            AuthViewState::LoggingOut | AuthViewState::CancellingLogout
        ) {
            if let Some(generation) = self.gate.pending_generation() {
                self.detached_logouts.insert(generation);
            }
            self.view = AuthViewState::Unchecked;
            self.login_succeeded = false;
            self.logout_confirm = None;
            self.status_reconcile = false;
            return;
        }
        if self
            .gate
            .pending_generation()
            .is_some_and(|generation| self.detached_logouts.contains(&generation))
        {
            // Repeated exit while the detached logout is still in flight: keep
            // the ticket/inbox alive instead of cancelling the only delivery path.
            return;
        }
        self.gate.cancel();
        self.view = AuthViewState::Unchecked;
        self.login_succeeded = false;
        self.logout_confirm = None;
        self.status_reconcile = false;
        if self.detached_logouts.is_empty() {
            lock_inbox(&self.inbox).clear();
        }
    }

    #[cfg(test)]
    pub(super) fn prepare_logout_for_test(
        &mut self,
    ) -> Option<OperationTicket<HostConfigAuthScope>> {
        self.prepare_logout().map(|request| request.ticket)
    }

    #[cfg(test)]
    pub(super) fn push_event_for_test(
        &self,
        ticket: OperationTicket<HostConfigAuthScope>,
        event: AuthEvent,
    ) {
        queue_into(&self.inbox, ticket, event);
    }

    #[cfg(test)]
    pub(super) fn set_view_for_test(&mut self, view: AuthViewState) {
        self.view = view;
    }
}

fn lock_inbox(
    inbox: &Inbox,
) -> std::sync::MutexGuard<'_, Vec<(OperationTicket<HostConfigAuthScope>, AuthEvent)>> {
    inbox
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Drop for AuthController {
    fn drop(&mut self) {
        // The owner is gone: background logout may still finish local cleanup.
        // Its task reports a safe terminal notification instead of writing state.
        self.observer.store(false, Ordering::SeqCst);
    }
}

/// Keep only the safe transport shape: strip control characters and bound the
/// length shown to the terminal. The full URL is still used for copy/open.
fn sanitize_url(url: String) -> String {
    url.chars()
        .filter(|ch| !ch.is_control())
        .take(16 * 1024)
        .collect()
}

// ── Dispatch (production) ─────────────────────────────────────────────────────

/// Start login for the current frozen target. Returns false when no dispatch
/// happened (busy or invalid target).
pub(super) fn dispatch_login(state: State<AuthController>) -> bool {
    let request = match state.write().prepare_login() {
        Ok(request) => request,
        Err(_) => return false,
    };
    if !spawn_login(state, request) {
        return false;
    }
    true
}

/// Query status. `reconcile` is used after cancelling a login wait.
pub(super) fn dispatch_status(state: State<AuthController>, reconcile: bool) -> bool {
    let request = match state.write().prepare_status(reconcile) {
        Some(request) => request,
        None => return false,
    };
    #[cfg(test)]
    if intercept_dispatch(if reconcile {
        "status-reconcile"
    } else {
        "status"
    }) {
        return true;
    }
    let Some(handle) = runtime_handle() else {
        state.write().fail(AuthFailure::Unavailable);
        return false;
    };
    let inbox = state.read().inbox.clone();
    handle.spawn(async move {
        let token = request.ticket.token().clone();
        let result = request.session.status(token).await;
        queue_into(&inbox, request.ticket, AuthEvent::StatusChecked(result));
    });
    true
}

/// Start logout using the frozen confirmation target.
pub(super) fn dispatch_logout(state: State<AuthController>) -> bool {
    let request = match state.write().prepare_logout() {
        Some(request) => request,
        None => return false,
    };
    if !spawn_logout(state, request) {
        return false;
    }
    true
}

fn spawn_login(state: State<AuthController>, request: AuthRequest) -> bool {
    #[cfg(test)]
    if intercept_dispatch("login") {
        return true;
    }
    let Some(handle) = runtime_handle() else {
        state.write().fail(AuthFailure::Unavailable);
        return false;
    };
    let inbox = state.read().inbox.clone();
    let AuthRequest { ticket, session } = request;
    let url_inbox = inbox.clone();
    let url_ticket = ticket.clone();
    let on_url = Box::new(move |url: String| {
        queue_into(
            &url_inbox,
            url_ticket.clone(),
            AuthEvent::AuthorizationUrl(url),
        );
    });
    handle.spawn(async move {
        let token = ticket.token().clone();
        let result = session.login(token, on_url).await;
        queue_into(&inbox, ticket, AuthEvent::LoginFinished(result));
    });
    true
}

fn spawn_logout(state: State<AuthController>, request: AuthRequest) -> bool {
    #[cfg(test)]
    if intercept_dispatch("logout") {
        return true;
    }
    let Some(handle) = runtime_handle() else {
        state.write().fail(AuthFailure::Unavailable);
        return false;
    };
    let inbox = state.read().inbox.clone();
    let observer = state.read().observer.clone();
    handle.spawn(async move {
        let token = request.ticket.token().clone();
        let result = request.session.logout(token).await;
        if !observer.load(Ordering::SeqCst) {
            // Panel closed: keep the user-visible outcome without touching a
            // different form. Local cleanup is mandatory; remote revocation may
            // be unconfirmed.
            match &result {
                Ok(outcome) => notify_logout_outcome(outcome),
                Err(_) => {
                    tracing::warn!("ChatGPT logout finished after the panel closed");
                    notify_key("login-auth-outcome-unknown", 3);
                }
            }
        }
        queue_into(&inbox, request.ticket, AuthEvent::LoggedOut(result));
    });
    true
}

fn queue_into(inbox: &Inbox, ticket: OperationTicket<HostConfigAuthScope>, event: AuthEvent) {
    lock_inbox(inbox).push((ticket, event));
    AUTH_EVENT_REVISION.set(AUTH_EVENT_REVISION.get().wrapping_add(1));
}

fn runtime_handle() -> Option<tokio::runtime::Handle> {
    tokio::runtime::Handle::try_current().ok()
}

#[cfg(test)]
#[path = "auth_test.rs"]
mod tests;
