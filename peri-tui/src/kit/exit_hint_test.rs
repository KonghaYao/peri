use serial_test::serial;

use super::*;

#[test]
fn empty_session_id_has_no_hint() {
    crate::i18n::init(None);
    assert!(resume_hint("").is_none());
    assert!(resume_hint("   ").is_none());
}

#[test]
fn hint_carries_resume_command_with_session_id() {
    crate::i18n::init(None);
    let hint = resume_hint("0199abc-def").expect("会话 ID 非空应产生提示");
    assert!(
        hint.contains("peri -r 0199abc-def"),
        "unexpected hint: {hint}"
    );
}

#[test]
fn hint_follows_configured_language() {
    crate::i18n::init(Some("zh-CN"));
    let hint = resume_hint("s-1").expect("会话 ID 非空应产生提示");
    assert!(hint.contains("peri -r s-1"), "unexpected hint: {hint}");
    assert!(hint.contains("恢复"), "unexpected hint: {hint}");
}

#[test]
#[serial]
fn current_hint_requires_active_session() {
    crate::i18n::init(None);
    let saved = atoms::ACTIVE_SESSION_ID.state().read().clone();

    *atoms::ACTIVE_SESSION_ID.state().write() = String::new();
    assert!(current_resume_hint().is_none());

    *atoms::ACTIVE_SESSION_ID.state().write() = "session-42".into();
    let hint = current_resume_hint().expect("有会话应产生提示");
    assert!(
        hint.contains("peri -r session-42"),
        "unexpected hint: {hint}"
    );

    *atoms::ACTIVE_SESSION_ID.state().write() = saved;
}
