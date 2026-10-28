//! Controller tests for the ChatGPT authentication state machine.
//!
//! A fake backend/session drives begin → URL → finish, status, cancel and
//! logout through channels and semaphores; no real browser, network or sleep.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ratatui_kit::prelude::{Component, ComponentUpdater, Hooks, Props, State, UseState, element};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::chatgpt_auth::{ChatGptAuthBackend, ChatGptAuthSession};
#[derive(Clone, Default)]
struct PlanDraft {
    subject: String,
    client_id: String,
}

impl AuthIdentityDraft for PlanDraft {
    fn needs_identity(&self) -> bool {
        self.subject.is_empty() || self.client_id.is_empty()
    }
    fn identity_mismatch(&self, status: &AccountStatus) -> bool {
        (!self.subject.is_empty() && self.subject != status.subject)
            || (!self.client_id.is_empty() && self.client_id != status.client_id)
    }
    fn set_identity(&mut self, status: &AccountStatus) -> bool {
        self.subject = status.subject.clone();
        self.client_id = status.client_id.clone();
        true
    }
}

struct FakeState {
    finish_results: Mutex<VecDeque<AuthResult<AccountStatus>>>,
    finish_signal: Semaphore,
    status_results: Mutex<VecDeque<AuthResult<Option<AccountStatus>>>>,
    logout_result: Mutex<AuthResult<LogoutOutcome>>,
    logout_signal: Semaphore,
    logins_started: AtomicUsize,
    logouts_started: AtomicUsize,
    status_checks: AtomicUsize,
    opened_targets: Mutex<Vec<AuthTarget>>,
}

impl FakeState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            finish_results: Mutex::new(VecDeque::new()),
            finish_signal: Semaphore::new(0),
            status_results: Mutex::new(VecDeque::new()),
            logout_result: Mutex::new(Ok(LogoutOutcome {
                local_cleared: true,
                remote_revoked: true,
            })),
            logout_signal: Semaphore::new(0),
            logins_started: AtomicUsize::new(0),
            logouts_started: AtomicUsize::new(0),
            status_checks: AtomicUsize::new(0),
            opened_targets: Mutex::new(Vec::new()),
        })
    }

    fn finish(&self, result: AuthResult<AccountStatus>) {
        self.finish_results.lock().unwrap().push_back(result);
        self.finish_signal.add_permits(1);
    }
}

struct FakeBackend {
    state: Arc<FakeState>,
}

impl ChatGptAuthBackend for FakeBackend {
    fn open(&self, target: &AuthTarget) -> AuthResult<Box<dyn ChatGptAuthSession>> {
        self.state
            .opened_targets
            .lock()
            .unwrap()
            .push(target.clone());
        Ok(Box::new(FakeSession {
            state: self.state.clone(),
        }))
    }
}

struct FakeSession {
    state: Arc<FakeState>,
}

#[async_trait]
impl ChatGptAuthSession for FakeSession {
    async fn login(
        &self,
        cancellation: CancellationToken,
        on_url: Box<dyn FnOnce(String) + Send>,
    ) -> AuthResult<AccountStatus> {
        self.state.logins_started.fetch_add(1, Ordering::SeqCst);
        on_url("https://auth.example.test/authorize?state=synthetic".to_owned());
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(AuthError::Cancelled),
            permit = self.state.finish_signal.acquire() => {
                if let Ok(permit) = permit {
                    permit.forget();
                }
                self.state
                    .finish_results
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Err(AuthError::Unavailable))
            }
        }
    }

    async fn status(&self, cancellation: CancellationToken) -> AuthResult<Option<AccountStatus>> {
        self.state.status_checks.fetch_add(1, Ordering::SeqCst);
        if cancellation.is_cancelled() {
            return Err(AuthError::Cancelled);
        }
        self.state
            .status_results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(None))
    }

    async fn logout(&self, cancellation: CancellationToken) -> AuthResult<LogoutOutcome> {
        self.state.logouts_started.fetch_add(1, Ordering::SeqCst);
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {}
            permit = self.state.logout_signal.acquire() => {
                if let Ok(permit) = permit {
                    permit.forget();
                }
            }
        }
        self.state.logout_result.lock().unwrap().clone()
    }
}

fn target() -> AuthTarget {
    AuthTarget::new(PathBuf::from("/tmp/peri-auth-test"), "work")
}

fn scope() -> HostConfigAuthScope {
    HostConfigAuthScope::new("/tmp/peri/settings.json", target())
}

fn other_scope() -> HostConfigAuthScope {
    HostConfigAuthScope::new(
        "/tmp/peri/settings.json",
        AuthTarget::new(PathBuf::from("/tmp/peri-auth-test"), "other"),
    )
}

fn controller() -> (AuthController, Arc<FakeState>) {
    let state = FakeState::new();
    let backend = Arc::new(FakeBackend {
        state: state.clone(),
    });
    let mut controller = AuthController::with_backend(backend);
    controller.sync_scope(scope());
    (controller, state)
}

async fn run_login(
    controller: &mut AuthController,
    session: Box<dyn ChatGptAuthSession>,
    ticket: OperationTicket<HostConfigAuthScope>,
) -> tokio::task::JoinHandle<()> {
    let inbox = controller.inbox.clone();
    tokio::spawn(async move {
        let url_inbox = inbox.clone();
        let url_ticket = ticket.clone();
        let on_url = Box::new(move |url: String| {
            url_inbox
                .lock()
                .unwrap()
                .push((url_ticket.clone(), AuthEvent::AuthorizationUrl(url)));
        });
        let token = ticket.token().clone();
        let result = session.login(token, on_url).await;
        inbox
            .lock()
            .unwrap()
            .push((ticket, AuthEvent::LoginFinished(result)));
    })
}

async fn wait_until(
    controller: &mut AuthController,
    label: &str,
    predicate: impl Fn(&AuthViewState) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            controller.drain();
            if predicate(controller.view()) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {label}"));
}

fn status(subject: &str, client_id: &str, plan_enabled: bool) -> AccountStatus {
    AccountStatus {
        account_ref: "work".into(),
        subject: subject.into(),
        email: Some("synthetic@example.test".into()),
        client_id: client_id.into(),
        plan_enabled,
        expires_at: u64::MAX / 2,
    }
}

#[test]
fn authentication_uses_raw_valid_account_and_never_opens_invalid_target() {
    for invalid in [
        "".to_owned(),
        "work/team".to_owned(),
        " work".to_owned(),
        "work ".to_owned(),
        " ".to_owned(),
        "合成".to_owned(),
        "x".repeat(129),
        format!("{} ", "x".repeat(128)),
    ] {
        let (mut controller, state) = controller();
        controller.sync_scope(HostConfigAuthScope::new(
            "fixture-source",
            AuthTarget::new(std::env::temp_dir(), invalid),
        ));
        assert!(matches!(
            controller.prepare_login(),
            Err(AuthFailure::TargetRequired)
        ));
        assert!(controller.prepare_status(false).is_none());
        assert!(state.opened_targets.lock().unwrap().is_empty());
    }

    let (mut controller, state) = controller();
    let account = "SYNTHETIC_account-42";
    controller.sync_scope(HostConfigAuthScope::new(
        "fixture-source",
        AuthTarget::new(std::env::temp_dir(), account),
    ));
    assert!(controller.prepare_login().is_ok());
    let opened = state.opened_targets.lock().unwrap();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].account, account);
}

/// [回归测试] 后台任务未能 spawn 时必须释放 `prepare_*` 建立的完成门。
#[test]
fn unspawned_request_releases_completion_gate() {
    let (mut controller, _) = controller();
    controller.prepare_login().unwrap();
    assert!(controller.gate.is_pending());

    controller.abort_unspawned(AuthFailure::Unavailable);

    assert!(matches!(
        controller.view(),
        AuthViewState::Failed(AuthFailure::Unavailable)
    ));
    assert!(!controller.gate.is_pending());
    assert!(
        controller.prepare_login().is_ok(),
        "清理后应允许重新发起登录"
    );
}

#[tokio::test]
async fn login_publishes_url_then_signed_in_identity() {
    let (mut controller, state) = controller();
    let mut plan = PlanDraft::default();
    let request = controller.prepare_login().unwrap();
    let task = run_login(&mut controller, request.session, request.ticket).await;

    wait_until(&mut controller, "authorization URL", |view| {
        view.url().is_some()
    })
    .await;
    let url = controller.view().url().unwrap().to_owned();
    assert!(url.starts_with("https://auth.example.test/authorize"));

    state.finish(Ok(status("subject-one", "client-one", true)));
    wait_until(&mut controller, "signed in", |view| {
        matches!(view, AuthViewState::SignedIn(_))
    })
    .await;
    assert!(controller.login_succeeded());
    assert_eq!(state.logins_started.load(Ordering::SeqCst), 1);

    let signed_subject = match controller.view() {
        AuthViewState::SignedIn(signed_in) => signed_in.subject.clone(),
        _ => panic!("expected signed-in"),
    };
    assert_eq!(signed_subject, "subject-one");
    assert!(
        plan.subject.is_empty(),
        "login result waits for the owner to apply identity"
    );
    assert!(controller.use_account(&mut plan, &scope()));
    assert_eq!(plan.subject, "subject-one");
    task.await.unwrap();
}

#[tokio::test]
async fn login_error_can_be_retried_without_stale_identity() {
    let (mut controller, state) = controller();
    let request = controller.prepare_login().unwrap();
    let task = run_login(&mut controller, request.session, request.ticket).await;
    wait_until(&mut controller, "authorization URL", |view| {
        view.url().is_some()
    })
    .await;
    state.finish(Err(AuthError::Unavailable));
    wait_until(&mut controller, "failed login", |view| {
        matches!(view, AuthViewState::Failed(AuthFailure::Unavailable))
    })
    .await;
    task.await.unwrap();

    let request = controller.prepare_login().unwrap();
    let task = run_login(&mut controller, request.session, request.ticket).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.logins_started.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second login task must start");
    wait_until(&mut controller, "authorization URL again", |view| {
        view.url().is_some()
    })
    .await;
    state.finish(Ok(status("subject-two", "client-two", true)));
    wait_until(&mut controller, "second login", |view| {
        matches!(view, AuthViewState::SignedIn(_))
    })
    .await;
    task.await.unwrap();
    assert_eq!(state.logins_started.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn status_identity_is_explicitly_adoptable_but_never_overwrites() {
    let (mut controller, state) = controller();
    state
        .status_results
        .lock()
        .unwrap()
        .push_back(Ok(Some(status(
            "verified-subject",
            "verified-client",
            true,
        ))));
    let request = controller.prepare_status(false).unwrap();
    let token = request.ticket.token().clone();
    let result = request.session.status(token).await;
    controller.receive(request.ticket, AuthEvent::StatusChecked(result));

    let mut plan = PlanDraft {
        subject: "old-subject".into(),
        client_id: "old-client".into(),
    };
    let AuthViewState::SignedIn(view_status) = controller.view() else {
        panic!("expected signed-in");
    };
    let view_subject = view_status.subject.clone();
    let view_client = view_status.client_id.clone();
    assert_eq!(view_subject, "verified-subject");
    assert!(plan.identity_mismatch(&status(&view_subject, &view_client, true)));
    assert_eq!(plan.subject, "old-subject", "status must not overwrite");
    assert!(controller.use_account(&mut plan, &scope()));
    assert_eq!(plan.subject, "verified-subject");
    assert_eq!(plan.client_id, "verified-client");
}

#[tokio::test]
async fn use_account_requires_current_scope() {
    let (mut controller, state) = controller();
    state
        .status_results
        .lock()
        .unwrap()
        .push_back(Ok(Some(status(
            "verified-subject",
            "verified-client",
            true,
        ))));
    let request = controller.prepare_status(false).unwrap();
    let token = request.ticket.token().clone();
    let result = request.session.status(token).await;
    controller.receive(request.ticket, AuthEvent::StatusChecked(result));

    let mut plan = PlanDraft {
        ..Default::default()
    };
    assert!(
        !controller.use_account(&mut plan, &other_scope()),
        "other store/account must never adopt the current view identity"
    );
    assert!(plan.needs_identity());
    assert!(controller.use_account(&mut plan, &scope()));
    assert_eq!(plan.subject, "verified-subject");
}

#[tokio::test]
async fn status_none_means_signed_out() {
    let (mut controller, _state) = controller();
    let request = controller.prepare_status(false).unwrap();
    let token = request.ticket.token().clone();
    let result = request.session.status(token).await;
    assert!(result.unwrap().is_none());
    controller.receive(request.ticket, AuthEvent::StatusChecked(Ok(None)));
    assert!(matches!(controller.view(), AuthViewState::SignedOut));
}

#[tokio::test]
async fn cancelling_login_reconciles_with_a_new_status_ticket_and_rejects_late_result() {
    let (mut controller, state) = controller();
    let request = controller.prepare_login().unwrap();
    let task = run_login(&mut controller, request.session, request.ticket).await;
    wait_until(&mut controller, "authorization URL", |view| {
        view.url().is_some()
    })
    .await;

    assert!(controller.cancel_login());
    assert!(matches!(controller.view(), AuthViewState::Cancelling));
    let reconcile = controller.prepare_status(true).unwrap();
    let token = reconcile.ticket.token().clone();
    state
        .status_results
        .lock()
        .unwrap()
        .push_back(Ok(Some(status("reconciled", "client", true))));
    let result = reconcile.session.status(token).await;
    controller.receive(reconcile.ticket, AuthEvent::StatusChecked(result));
    assert!(matches!(controller.view(), AuthViewState::SignedIn(_)));

    // The cancelled login task must finish and its late result is rejected.
    state.finish(Ok(status("late-subject", "late-client", true)));
    task.await.unwrap();
    controller.drain();
    let AuthViewState::SignedIn(current) = controller.view() else {
        panic!("expected reconciled status");
    };
    assert_eq!(current.subject, "reconciled");
}

#[tokio::test]
async fn reconciliation_failure_reports_unknown_instead_of_signed_out() {
    let (mut controller, _state) = controller();
    let request = controller.prepare_login().unwrap();
    let task = run_login(&mut controller, request.session, request.ticket).await;
    wait_until(&mut controller, "authorization URL", |view| {
        view.url().is_some()
    })
    .await;
    assert!(controller.cancel_login());
    let reconcile = controller.prepare_status(true).unwrap();
    let token = reconcile.ticket.token().clone();
    controller.receive(
        reconcile.ticket,
        AuthEvent::StatusChecked(Err(AuthError::Unavailable)),
    );
    assert!(matches!(
        controller.view(),
        AuthViewState::OutcomeUnknown(OutcomeUnknownReason::WaitCancelled)
    ));
    let _ = token;
    task.await.unwrap();
}

#[tokio::test]
async fn logout_cancel_keeps_the_same_ticket_and_reports_partial_result() {
    let (mut controller, state) = controller();
    *state.logout_result.lock().unwrap() = Ok(LogoutOutcome {
        local_cleared: true,
        remote_revoked: false,
    });
    assert!(controller.open_logout_confirm(target(), vec!["plan".into()]));
    let request = controller.prepare_logout().unwrap();
    let inbox = controller.inbox.clone();
    let task = tokio::spawn(async move {
        let token = request.ticket.token().clone();
        let result = request.session.logout(token).await;
        inbox
            .lock()
            .unwrap()
            .push((request.ticket, AuthEvent::LoggedOut(result)));
    });
    assert!(controller.cancel_logout_wait());
    assert!(matches!(controller.view(), AuthViewState::CancellingLogout));
    state.logout_signal.add_permits(1);
    wait_until(&mut controller, "logout result", |view| {
        matches!(view, AuthViewState::LoggedOut(_))
    })
    .await;
    let AuthViewState::LoggedOut(outcome) = controller.view() else {
        panic!("expected logout outcome");
    };
    assert!(outcome.local_cleared);
    assert!(!outcome.remote_revoked);
    assert_eq!(state.logouts_started.load(Ordering::SeqCst), 1);
    task.await.unwrap();
}

#[tokio::test]
async fn shutdown_cancels_an_in_flight_login_task() {
    let (mut controller, state) = controller();
    let request = controller.prepare_login().unwrap();
    let task = run_login(&mut controller, request.session, request.ticket).await;
    wait_until(&mut controller, "authorization URL", |view| {
        view.url().is_some()
    })
    .await;
    controller.shutdown();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("cancelled login task must finish")
        .unwrap();
    assert_eq!(state.logins_started.load(Ordering::SeqCst), 1);
    assert!(matches!(controller.view(), AuthViewState::Unchecked));
}

#[tokio::test]
async fn target_change_after_cancel_does_not_accept_old_login_result() {
    let (mut controller, state) = controller();
    let request = controller.prepare_login().unwrap();
    let task = run_login(&mut controller, request.session, request.ticket).await;
    wait_until(&mut controller, "authorization URL", |view| {
        view.url().is_some()
    })
    .await;
    assert!(controller.cancel_login());
    // In production the panel immediately starts a reconciliation status query;
    // the target can only change after the wait view leaves the busy state.
    drop(controller.prepare_status(true));
    assert!(controller.sync_scope(other_scope()));
    assert!(matches!(controller.view(), AuthViewState::Unchecked));

    state.finish(Ok(status("old-subject", "old-client", true)));
    task.await.unwrap();
    controller.drain();
    assert!(
        matches!(controller.view(), AuthViewState::Unchecked),
        "late result from the old target must not leak"
    );
}

#[tokio::test]
async fn action_group_skips_use_account_until_identity_differs() {
    let (mut controller, state) = controller();
    state
        .status_results
        .lock()
        .unwrap()
        .push_back(Ok(Some(status(
            "verified-subject",
            "verified-client",
            false,
        ))));
    let request = controller.prepare_status(false).unwrap();
    let token = request.ticket.token().clone();
    let result = request.session.status(token).await;
    controller.receive(request.ticket, AuthEvent::StatusChecked(result));

    let matching = PlanDraft {
        subject: "verified-subject".into(),
        client_id: "verified-client".into(),
    };
    assert!(
        !controller
            .available_actions(&matching)
            .contains(&AuthAction::UseAccount)
    );
    let mut mismatched = matching.clone();
    mismatched.subject = "other-subject".into();
    assert!(
        controller
            .available_actions(&mismatched)
            .contains(&AuthAction::UseAccount)
    );
    controller.cycle_action(true, &mismatched);
    controller.cycle_action(true, &mismatched);
    assert_eq!(
        controller.selected_action(&mismatched),
        Some(AuthAction::UseAccount)
    );
    assert_eq!(state.status_checks.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelled_login_with_no_visible_credentials_is_still_unknown() {
    let (mut controller, _state) = controller();
    let request = controller.prepare_login().unwrap();
    let task = run_login(&mut controller, request.session, request.ticket).await;
    wait_until(&mut controller, "authorization URL", |view| {
        view.url().is_some()
    })
    .await;
    assert!(controller.cancel_login());
    let reconcile = controller.prepare_status(true).unwrap();
    controller.receive(reconcile.ticket, AuthEvent::StatusChecked(Ok(None)));
    assert!(
        matches!(controller.view(), AuthViewState::OutcomeUnknown(_)),
        "a cancelled finish can still persist credentials later; do not claim signed out"
    );
    task.await.unwrap();
}

#[tokio::test]
async fn duplicate_login_or_logout_while_in_flight_is_rejected() {
    let (mut controller, _state) = controller();
    let _login = controller.prepare_login().unwrap();
    assert!(
        controller.prepare_login().is_err(),
        "starting a second login must not issue another request"
    );
    assert!(
        controller.prepare_status(false).is_none(),
        "status is not allowed while the login wait owns the gate"
    );
}

#[tokio::test]
async fn duplicate_logout_while_in_flight_is_rejected() {
    let (mut controller, _state) = controller();
    assert!(controller.open_logout_confirm(target(), vec!["plan".into()]));
    let _logout = controller.prepare_logout().unwrap();
    assert!(
        controller.prepare_logout().is_none(),
        "a second logout while the first is pending must not be sent"
    );
}

// ── dispatch 入口（无 Tokio runtime）──────────────────────────────────────
//
// `State<T>` 没有公开构造 API，只能从真实挂载的组件里取得；探针组件在 update
// 阶段把句柄交给同步 visitor，测试就在没有 runtime 的线程里直接调用 dispatch。

type DispatchProbeVisitor = Box<dyn FnMut(State<AuthController>)>;

thread_local! {
    static DISPATCH_PROBE_VISITOR: std::cell::RefCell<Option<DispatchProbeVisitor>> =
        const { std::cell::RefCell::new(None) };
}

/// 断言 panic 时清空 visitor，避免污染同线程后续用例。
struct DispatchProbeGuard;

impl Drop for DispatchProbeGuard {
    fn drop(&mut self) {
        DISPATCH_PROBE_VISITOR.with(|slot| *slot.borrow_mut() = None);
    }
}

#[derive(Default, Props)]
struct DispatchProbeProps;

struct DispatchProbe;

impl Component for DispatchProbe {
    type Props<'a> = DispatchProbeProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        _props: &mut Self::Props<'_>,
        mut hooks: Hooks,
        _updater: &mut ComponentUpdater,
    ) {
        let auth = hooks.use_state(|| {
            AuthController::with_backend(Arc::new(FakeBackend {
                state: FakeState::new(),
            }))
        });
        DISPATCH_PROBE_VISITOR.with(|slot| {
            if let Some(visitor) = slot.borrow_mut().as_mut() {
                visitor(auth);
            }
        });
    }
}

fn probe_dispatch(visitor: impl FnMut(State<AuthController>) + 'static) {
    let _guard = DispatchProbeGuard;
    DISPATCH_PROBE_VISITOR.with(|slot| *slot.borrow_mut() = Some(Box::new(visitor)));
    let _ = ratatui_kit::test_util::render_frame(element!(DispatchProbe()), 1, 1);
}

/// `dispatch_login` 在没有 runtime 时走 `abort_unspawned`：必须释放完成门，
/// 后续（同一 controller）可以再次发起登录。
#[test]
fn dispatch_login_without_runtime_releases_gate() {
    let _intercept = super::super::actions::DispatchInterceptDisabled::new();
    probe_dispatch(|auth| {
        auth.write().sync_scope(scope());
        assert!(
            !super::super::dispatch_login(auth),
            "无 runtime 的 spawn 必须返回 false"
        );
        assert!(matches!(
            auth.read().view(),
            AuthViewState::Failed(AuthFailure::Unavailable)
        ));
        assert!(!auth.read().gate.is_pending(), "完成门必须已释放");
        assert!(
            auth.write().prepare_login().is_ok(),
            "释放后必须能重新发起登录"
        );
    });
}

/// `dispatch_status` 同样受无 runtime 保护，且不得阻断后续登录。
#[test]
fn dispatch_status_without_runtime_releases_gate() {
    let _intercept = super::super::actions::DispatchInterceptDisabled::new();
    probe_dispatch(|auth| {
        auth.write().sync_scope(scope());
        assert!(!super::super::dispatch_status(auth, false));
        assert!(matches!(
            auth.read().view(),
            AuthViewState::Failed(AuthFailure::Unavailable)
        ));
        assert!(!auth.read().gate.is_pending(), "完成门必须已释放");
        assert!(auth.write().prepare_login().is_ok());
    });
}

/// `dispatch_logout` 同样受无 runtime 保护，且不得阻断后续登录。
#[test]
fn dispatch_logout_without_runtime_releases_gate() {
    let _intercept = super::super::actions::DispatchInterceptDisabled::new();
    probe_dispatch(|auth| {
        auth.write().sync_scope(scope());
        assert!(!super::super::dispatch_logout(auth));
        assert!(matches!(
            auth.read().view(),
            AuthViewState::Failed(AuthFailure::Unavailable)
        ));
        assert!(!auth.read().gate.is_pending(), "完成门必须已释放");
        assert!(auth.write().prepare_login().is_ok());
    });
}
