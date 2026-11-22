//! setup pass 决策纯函数的单元测试。

use super::{SetupPassOutcome, setup_pass_outcome, setup_pass_required};

#[test]
fn setup_pass_required_covers_needs_setup_and_setup_only() {
    assert!(setup_pass_required(true, false));
    assert!(setup_pass_required(false, true));
    assert!(setup_pass_required(true, true));
}

#[test]
fn configured_user_skips_the_setup_pass() {
    assert!(
        !setup_pass_required(false, false),
        "a configured user without --setup must not enter the wizard"
    );
}

#[test]
fn setup_pass_outcome_completed_continues_or_exits() {
    assert_eq!(
        setup_pass_outcome(false, true),
        SetupPassOutcome::Continue,
        "first-run completion must attach ACP and enter the normal shell"
    );
    assert_eq!(
        setup_pass_outcome(true, true),
        SetupPassOutcome::ExitAfterSetup,
        "setup-only completion must exit without attaching ACP"
    );
}

#[test]
fn setup_pass_outcome_incomplete_cancels_or_fails() {
    assert_eq!(
        setup_pass_outcome(false, false),
        SetupPassOutcome::Cancelled,
        "regular first-run cancellation keeps the existing successful exit"
    );
    assert_eq!(
        setup_pass_outcome(true, false),
        SetupPassOutcome::SetupIncomplete,
        "setup-only cancellation must surface as a failure"
    );
}
