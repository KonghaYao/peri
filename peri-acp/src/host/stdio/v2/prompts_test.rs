use super::{valid_idempotency_key, Admission, Prompts};
use crate::transport::types::RequestId;
use serde_json::json;

#[test]
fn idempotency_key_is_bounded_by_characters_not_utf8_bytes() {
    assert!(valid_idempotency_key("a"));
    assert!(valid_idempotency_key(&"a".repeat(255)));
    assert!(!valid_idempotency_key(&"a".repeat(256)));
    assert!(!valid_idempotency_key(""));
    // 128 four-byte characters are 512 bytes but still 128 characters.
    assert!(valid_idempotency_key(&"🦀".repeat(128)));
    assert!(valid_idempotency_key(&"🦀".repeat(255)));
    assert!(!valid_idempotency_key(&"🦀".repeat(256)));
}

#[test]
fn keyed_receipt_is_not_acknowledged_before_committed_delivery() {
    let mut prompts = Prompts::default();
    let blocks = json!([{"type":"text","text":"same input"}]);
    let first = RequestId::Number(1);
    let second = RequestId::Number(2);
    let Admission::Submit(input) = prompts
        .admit("session", Some("key"), blocks.clone(), first.clone())
        .expect("submit")
    else {
        panic!("expected initial submission");
    };
    assert!(matches!(
        prompts.admit("session", Some("key"), blocks.clone(), second.clone()),
        Ok(Admission::Pending)
    ));
    assert!(prompts.accepted("other", &input).is_none());

    let (update, requests) = prompts.accepted("session", &input).expect("committed");
    assert_eq!(update["messageId"], input);
    assert_eq!(requests, [first, second]);
    assert!(prompts.accepted("session", &input).is_none());
    assert!(matches!(
        prompts.admit("session", Some("key"), blocks, RequestId::Number(3)),
        Ok(Admission::Accepted(previous)) if previous == input
    ));
    assert!(prompts
        .admit(
            "session",
            Some("key"),
            json!([{"type":"text","text":"different"}]),
            RequestId::Number(4)
        )
        .is_err());
}

#[test]
fn withdrawal_releases_waiters_and_does_not_create_a_receipt() {
    let mut prompts = Prompts::default();
    let blocks = json!([{"type":"text","text":"cancelled input"}]);
    let rid = RequestId::String("origin".into());
    let Admission::Submit(input) = prompts
        .admit("session", Some("key"), blocks.clone(), rid.clone())
        .expect("submit")
    else {
        panic!("expected initial submission");
    };
    assert_eq!(
        prompts.target(&rid),
        Some(("session".into(), input.clone()))
    );
    assert_eq!(prompts.reject(&input), std::slice::from_ref(&rid));
    assert!(prompts.target(&rid).is_none());
    assert!(matches!(
        prompts.admit("session", Some("key"), blocks, rid),
        Ok(Admission::Submit(new)) if new != input
    ));
}

#[test]
fn scoped_rejection_never_settles_another_sessions_prompt() {
    let mut prompts = Prompts::default();
    let rid = RequestId::Number(9);
    let Admission::Submit(input) = prompts
        .admit(
            "session",
            None,
            json!([{"type":"text","text":"x"}]),
            rid.clone(),
        )
        .expect("submit")
    else {
        panic!("expected initial submission");
    };
    assert!(prompts.reject_in_session("other", &input).is_empty());
    assert_eq!(prompts.reject_in_session("session", &input), [rid]);
}
