use super::*;

#[test]
fn valid_logical_ids_are_accepted() {
    let longest = "x".repeat(128);
    for value in ["a", "agent-01", "A_b-9", longest.as_str()] {
        assert_eq!(LogicalId::parse(value).expect("valid").as_str(), value);
    }
}

#[test]
fn invalid_logical_ids_are_rejected() {
    let too_long = "x".repeat(129);
    for value in [
        "",
        "with space",
        "slash/name",
        "dot.name",
        too_long.as_str(),
    ] {
        let error = LogicalId::parse(value).expect_err("invalid");
        assert_eq!(error.code(), crate::domain::ErrorCode::InvalidLogicalId);
    }
}

#[test]
fn generated_ids_are_valid_and_unique() {
    let first = LogicalId::generate();
    let second = LogicalId::generate();
    assert_ne!(first, second);
    assert!(first.as_str().starts_with("agent-"));
    LogicalId::parse(first.as_str()).expect("generated id is valid");
}

#[test]
fn idempotency_key_derivation_is_deterministic() {
    let first = LogicalId::derived_from_idempotency_key("request-key");
    let second = LogicalId::derived_from_idempotency_key("request-key");
    let other = LogicalId::derived_from_idempotency_key("other-key");
    assert_eq!(first, second);
    assert_ne!(first, other);
    assert!(first.as_str().starts_with("agent-"));
}

#[test]
fn actor_key_keeps_two_segments() {
    let key = ActorKey::new(LogicalId::parse("agent-01").expect("id"));
    assert_eq!(key.segments(), ["agent", "agent-01"]);
    assert_eq!(key.storage_key(), "agent/agent-01");
    assert_eq!(ActorKey::ACTOR_NAME, "swarm-session");
}

#[test]
fn root_session_ids_are_uuids() {
    let id = new_root_session_id();
    uuid::Uuid::parse_str(&id).expect("uuid");
}
