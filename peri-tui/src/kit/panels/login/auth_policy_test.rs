//! `auth_policy` 纯函数回归：自动采用、作用域、状态化阻断与 token 新鲜度。

use super::*;
use crate::application::operations::OutcomeUnknownReason;
use crate::chatgpt_auth::{AccountStatus, AuthTarget, LogoutOutcome};

fn scope(account: &str) -> HostConfigAuthScope {
    HostConfigAuthScope::new(
        std::env::temp_dir()
            .join("peri/settings.json")
            .to_string_lossy()
            .into_owned(),
        AuthTarget::new(std::env::temp_dir().join("peri-auth-test"), account),
    )
}

fn plan(subject: &str, client_id: &str) -> PlanDraft {
    PlanDraft {
        store: std::env::temp_dir()
            .join("peri-auth-test")
            .to_string_lossy()
            .into_owned(),
        account: "work".into(),
        subject: subject.into(),
        client_id: client_id.into(),
    }
}

fn status(subject: &str, client_id: &str, plan_enabled: bool) -> AccountStatus {
    AccountStatus {
        account_ref: "work".into(),
        subject: subject.into(),
        email: None,
        client_id: client_id.into(),
        plan_enabled,
        expires_at: 0,
    }
}

#[test]
fn adopt_vacant_identity_for_current_scope() {
    let mut draft = plan("", "");
    let view = AuthViewState::SignedIn(status("verified-subject", "verified-client", true));
    assert!(adopt_if_verified(
        &mut draft,
        &view,
        &scope("work"),
        &scope("work")
    ));
    assert_eq!(draft.subject, "verified-subject");
    assert_eq!(draft.client_id, "verified-client");
}

#[test]
fn adopt_compatible_partial_identity_without_overwrite() {
    let mut draft = plan("verified-subject", "");
    let view = AuthViewState::SignedIn(status("verified-subject", "verified-client", true));
    assert!(adopt_if_verified(
        &mut draft,
        &view,
        &scope("work"),
        &scope("work")
    ));
    assert_eq!(draft.client_id, "verified-client");
}

#[test]
fn do_not_overwrite_different_identity() {
    let mut draft = plan("saved-subject", "saved-client");
    let view = AuthViewState::SignedIn(status("verified-subject", "verified-client", true));
    assert!(!adopt_if_verified(
        &mut draft,
        &view,
        &scope("work"),
        &scope("work")
    ));
    assert_eq!(draft.subject, "saved-subject");
    assert_eq!(draft.client_id, "saved-client");
    assert!(matches!(
        readiness(&draft, &view, &scope("work"), &scope("work")),
        IdentityReadiness::Conflict(_)
    ));
    assert!(save_block(&draft, &view, &scope("work"), &scope("work")).is_none());
}

#[test]
fn do_not_adopt_for_other_scope() {
    let mut draft = plan("", "");
    let view = AuthViewState::SignedIn(status("verified-subject", "verified-client", true));
    assert!(!adopt_if_verified(
        &mut draft,
        &view,
        &scope("work"),
        &scope("other")
    ));
    assert!(draft.needs_identity());
    assert_eq!(
        save_block(&draft, &view, &scope("work"), &scope("other")),
        Some(IdentityBlock::ScopeStale)
    );
}

#[test]
fn do_not_adopt_without_plan_scope() {
    let mut draft = plan("", "");
    let view = AuthViewState::SignedIn(status("verified-subject", "verified-client", false));
    assert!(!adopt_if_verified(
        &mut draft,
        &view,
        &scope("work"),
        &scope("work")
    ));
    assert_eq!(
        save_block(&draft, &view, &scope("work"), &scope("work")),
        Some(IdentityBlock::NoPlanScope)
    );
}

#[test]
fn checked_unchecked_signed_out_failed_mismatch_map_to_blocks() {
    let draft = plan("", "");
    let current = scope("work");
    assert_eq!(
        save_block(&draft, &AuthViewState::Checking, &current, &current),
        Some(IdentityBlock::Checking)
    );
    assert_eq!(
        save_block(&draft, &AuthViewState::Unchecked, &current, &current),
        Some(IdentityBlock::Unchecked)
    );
    assert_eq!(
        save_block(&draft, &AuthViewState::SignedOut, &current, &current),
        Some(IdentityBlock::SignedOut)
    );
    assert_eq!(
        save_block(
            &draft,
            &AuthViewState::Failed(AuthFailure::Unavailable),
            &current,
            &current
        ),
        Some(IdentityBlock::Failed(AuthFailure::Unavailable))
    );
    assert_eq!(
        save_block(
            &draft,
            &AuthViewState::OutcomeUnknown(OutcomeUnknownReason::WaitCancelled),
            &current,
            &current
        ),
        Some(IdentityBlock::OutcomeUnknown)
    );
    assert_eq!(
        save_block(
            &draft,
            &AuthViewState::LoggedOut(LogoutOutcome {
                local_cleared: true,
                remote_revoked: true,
            }),
            &current,
            &current
        ),
        Some(IdentityBlock::LoggedOut)
    );
}

#[test]
fn complete_reference_is_not_blocked_by_signed_out_or_failed_status() {
    let draft = plan("saved-subject", "saved-client");
    let current = scope("work");
    for view in [
        AuthViewState::SignedOut,
        AuthViewState::Failed(AuthFailure::Storage),
        AuthViewState::Checking,
        AuthViewState::Unchecked,
    ] {
        assert_eq!(save_block(&draft, &view, &current, &current), None);
    }
}

#[test]
fn recommended_action_never_logout() {
    let blocks = [
        IdentityBlock::Checking,
        IdentityBlock::Unchecked,
        IdentityBlock::SignedOut,
        IdentityBlock::LoggedOut,
        IdentityBlock::Failed(AuthFailure::Generic),
        IdentityBlock::OutcomeUnknown,
        IdentityBlock::ScopeStale,
        IdentityBlock::NoPlanScope,
        IdentityBlock::Incomplete,
        IdentityBlock::Busy,
    ];
    for block in blocks {
        assert_ne!(block.recommended_action(), AuthAction::Logout);
    }
}

#[test]
fn checking_is_the_only_wait_block() {
    assert!(IdentityBlock::Checking.is_wait());
    assert!(!IdentityBlock::Unchecked.is_wait());
    assert!(!IdentityBlock::SignedOut.is_wait());
}

#[test]
fn token_freshness_boundaries_use_injected_now() {
    let now = 1_000_000;
    assert_eq!(TokenFreshness::classify(0, now), TokenFreshness::Unknown);
    assert_eq!(
        TokenFreshness::classify(now - 1, now),
        TokenFreshness::Expired
    );
    assert_eq!(TokenFreshness::classify(now, now), TokenFreshness::Expired);
    assert_eq!(
        TokenFreshness::classify(now + 299, now),
        TokenFreshness::Expiring
    );
    assert_eq!(
        TokenFreshness::classify(now + 300, now),
        TokenFreshness::Expiring
    );
    assert_eq!(
        TokenFreshness::classify(now + 301, now),
        TokenFreshness::Valid
    );
}
