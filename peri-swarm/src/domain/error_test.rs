use super::*;

#[test]
fn acp_error_codes_map_to_the_http_contract() {
    let conflict = SwarmError::from_acp_error(-32010);
    assert_eq!(conflict.kind(), ErrorKind::Conflict);
    assert_eq!(conflict.code(), ErrorCode::SessionConflict);
    assert_eq!(conflict.details()["acpCode"], serde_json::json!(-32010));

    let capability = SwarmError::from_acp_error(-32601);
    assert_eq!(capability.kind(), ErrorKind::Unavailable);
    assert_eq!(capability.code(), ErrorCode::CapabilityUnavailable);

    let parameters = SwarmError::from_acp_error(-32602);
    assert_eq!(parameters.kind(), ErrorKind::BadRequest);
    assert_eq!(parameters.code(), ErrorCode::InvalidInput);

    let other = SwarmError::from_acp_error(-32000);
    assert_eq!(other.kind(), ErrorKind::BadGateway);
    assert_eq!(other.code(), ErrorCode::AgentError);
    assert_eq!(other.details()["acpCode"], serde_json::json!(-32000));
}

#[test]
fn error_codes_are_stable_strings() {
    assert_eq!(ErrorCode::UnknownAgent.as_str(), "unknown_agent");
    assert_eq!(ErrorCode::TurnInProgress.as_str(), "turn_in_progress");
    assert_eq!(ErrorCode::ConflictingCursor.as_str(), "conflicting_cursor");
    assert_eq!(
        ErrorCode::SessionCreationPending.as_str(),
        "session_creation_pending"
    );
    assert_eq!(ErrorCode::SeqConflict.as_str(), "seq_conflict");
}

#[test]
fn constructors_keep_actions_diagnosable() {
    let busy = SwarmError::turn_in_progress();
    assert_eq!(busy.details()["suggest"], serde_json::json!("steer"));

    let unauthorized = SwarmError::unauthorized();
    assert_eq!(unauthorized.kind(), ErrorKind::Unauthorized);
    assert!(unauthorized.details().is_null());

    let unsupported = SwarmError::unsupported_environment("container");
    assert_eq!(unsupported.kind(), ErrorKind::Unprocessable);
    assert_eq!(
        unsupported.details()["supported"],
        serde_json::json!(["none", "openai_hosted"])
    );
}
