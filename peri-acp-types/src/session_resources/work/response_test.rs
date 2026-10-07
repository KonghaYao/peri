use super::super::response::validate_response_intents;
use super::*;
use crate::messages::{BaseMessage, ToolCallRequest};

fn response_and_intent() -> (BaseMessage, InvocationIntent) {
    let arguments = serde_json::json!({"path":"example.rs","content":"exact"});
    let mut invocation = intent("first");
    invocation.arguments = EvidenceWrite {
        session_id: "session".into(),
        storage_scope: "workspace".into(),
        payload_id: "raw-arguments".into(),
        encoding: 1,
        bytes: serde_json::to_vec(&arguments).unwrap(),
    }
    .reference()
    .unwrap();
    invocation.arguments_digest = invocation.arguments.sha256.clone();
    let response = BaseMessage::ai_with_tool_calls(
        "response",
        vec![ToolCallRequest::new(
            &invocation.tool_call_id,
            &invocation.tool_name,
            arguments,
        )],
    );
    (response, invocation)
}

#[test]
fn every_response_tool_call_requires_its_exact_committed_intent() {
    let (response, invocation) = response_and_intent();
    validate_response_intents(&response, std::slice::from_ref(&invocation)).unwrap();
    assert_eq!(
        validate_response_intents(&response, &[]),
        Err(WorkRejection::Conflict)
    );
    for changed in ["identity", "name", "arguments", "length"] {
        let mut changed_intent = invocation.clone();
        match changed {
            "identity" => changed_intent.tool_call_id = "another-call".into(),
            "name" => changed_intent.tool_name = "another-tool".into(),
            "arguments" => {
                changed_intent.arguments.sha256 = "0".repeat(64);
                changed_intent.arguments_digest = changed_intent.arguments.sha256.clone();
            }
            "length" => changed_intent.arguments.byte_length += 1,
            _ => unreachable!(),
        }
        assert_eq!(
            validate_response_intents(&response, &[changed_intent]),
            Err(WorkRejection::Conflict)
        );
    }
}

#[test]
fn text_only_response_cannot_dispatch_an_unmentioned_intent() {
    let (_, invocation) = response_and_intent();
    validate_response_intents(&BaseMessage::ai("answer"), &[]).unwrap();
    assert_eq!(
        validate_response_intents(&BaseMessage::ai("answer"), &[invocation]),
        Err(WorkRejection::Conflict)
    );
    assert_eq!(
        validate_response_intents(&BaseMessage::human("not an assistant"), &[]),
        Err(WorkRejection::Conflict)
    );
}

#[test]
fn duplicated_response_calls_and_intents_cannot_cross_the_dispatch_barrier() {
    let (mut response, invocation) = response_and_intent();
    if let BaseMessage::Ai { tool_calls, .. } = &mut response {
        tool_calls.push(tool_calls[0].clone());
    }
    assert_eq!(
        validate_response_intents(&response, &[invocation.clone(), invocation]),
        Err(WorkRejection::Conflict)
    );
}

#[test]
fn immutable_response_envelope_must_match_the_canonical_message_identity() {
    let (message, invocation) = response_and_intent();
    let mut response = payload("response", "assistant");
    let canonical = crate::store::PersistedPayload::Message(message);
    assert_eq!(
        validate_reason_response_intents(&response, &canonical, std::slice::from_ref(&invocation)),
        Err(WorkRejection::Conflict)
    );
    response.message_id = canonical.id();
    validate_reason_response_intents(&response, &canonical, std::slice::from_ref(&invocation))
        .unwrap();
    response.role = "user".into();
    assert_eq!(
        validate_reason_response_intents(&response, &canonical, &[invocation]),
        Err(WorkRejection::Conflict)
    );
}
