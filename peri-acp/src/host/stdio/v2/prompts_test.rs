use super::{derive_message_id, valid_idempotency_key, Admission, Prompts};
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
fn derived_message_id_is_stable_per_session_and_key() {
    let id = derive_message_id("6b0d4f8e-0000-7000-8000-000000000001", "key");
    assert_eq!(
        id,
        derive_message_id("6b0d4f8e-0000-7000-8000-000000000001", "key"),
        "同一会话同一 key 必须得到同一消息身份，否则重连后无法去重"
    );
    assert_ne!(
        id,
        derive_message_id("6b0d4f8e-0000-7000-8000-000000000002", "key"),
        "不同会话的同一 key 各自独立"
    );
    assert_ne!(
        id,
        derive_message_id("6b0d4f8e-0000-7000-8000-000000000001", "other"),
        "同会话不同 key 是不同提交"
    );
    let parsed = uuid::Uuid::parse_str(&id).expect("derived id is a UUID");
    assert_eq!(parsed.get_version_num(), 5, "派生身份是 UUIDv5");
}

#[test]
fn unkeyed_submission_uses_a_time_ordered_identity() {
    let mut prompts = Prompts::default();
    let blocks = json!([{"type":"text","text":"no key"}]);
    let Admission::Submit(first) = prompts
        .admit("session", None, blocks.clone(), RequestId::Number(1))
        .expect("submit")
    else {
        panic!("expected initial submission");
    };
    let Admission::Submit(second) = prompts
        .admit("session", None, blocks, RequestId::Number(2))
        .expect("submit")
    else {
        panic!("expected a second submission");
    };
    assert_ne!(first, second, "无 key 的提交不去重");
    assert_eq!(
        uuid::Uuid::parse_str(&first)
            .expect("uuid")
            .get_version_num(),
        7,
        "无 key 仍用 UUIDv7"
    );
}

#[test]
fn keyed_retry_after_acknowledgement_reuses_the_identity() {
    let mut prompts = Prompts::default();
    let blocks = json!([{"type":"text","text":"retry me"}]);
    let Admission::Submit(input) = prompts
        .admit("session", Some("key"), blocks.clone(), RequestId::Number(1))
        .expect("submit")
    else {
        panic!("expected initial submission");
    };
    assert!(prompts.accepted("session", &input).is_some());

    // 没有回执表：重试重新走一遍准入，但身份由派生决定，与首次一致。
    let Admission::Submit(retry) = prompts
        .admit("session", Some("key"), blocks, RequestId::Number(2))
        .expect("retry")
    else {
        panic!("expected the retry to be submitted, not parked");
    };
    assert_eq!(retry, input);
}

#[test]
fn a_keyed_retry_in_flight_joins_the_first_submission() {
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
    // 已应答之后的新请求重新走准入：内容是否冲突由存储的持久事实回答。
    assert!(matches!(
        prompts.admit(
            "session",
            Some("key"),
            json!([{"type":"text","text":"different"}]),
            RequestId::Number(3)
        ),
        Ok(Admission::Submit(retry)) if retry == input
    ));
}

#[test]
fn different_content_under_one_key_in_flight_conflicts() {
    let mut prompts = Prompts::default();
    prompts
        .admit(
            "session",
            Some("key"),
            json!([{"type":"text","text":"first"}]),
            RequestId::Number(1),
        )
        .expect("submit");
    let error = match prompts.admit(
        "session",
        Some("key"),
        json!([{"type":"text","text":"second"}]),
        RequestId::Number(2),
    ) {
        Err(error) => error,
        Ok(_) => panic!("one key cannot describe two submissions"),
    };
    assert_eq!(error.code, -32602);
}

#[test]
fn handed_off_acknowledgement_releases_waiters_and_forgets_the_entry() {
    let mut prompts = Prompts::default();
    let blocks = json!([{"type":"text","text":"already inserted"}]);
    let first = RequestId::Number(11);
    let second = RequestId::Number(12);
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

    assert_eq!(prompts.acknowledge(&input), [first, second]);
    // 条目必须释放：再重试时不能停在等待永远不会到来的交付事件上。
    assert!(matches!(
        prompts.admit("session", Some("key"), blocks, RequestId::Number(13)),
        Ok(Admission::Submit(retry)) if retry == input
    ));
}

#[test]
fn withdrawal_releases_waiters_and_a_retry_is_a_new_submission() {
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
    // 身份仍由 key 派生；存储侧决定撤回后的同一身份是否还能插入。
    assert!(matches!(
        prompts.admit("session", Some("key"), blocks, rid),
        Ok(Admission::Submit(retry)) if retry == input
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
