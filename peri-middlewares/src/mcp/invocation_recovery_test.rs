use super::*;
use peri_acp_types::session_resources::work::{
    InvocationRecord, InvocationStatus, WorkQuery, WorkState,
};
use peri_acp_types::session_resources::ControlState;
use sha2::{Digest, Sha256};

fn snapshot() -> WorkSnapshot {
    let arguments_json = "{\"command\":\"sleep 30\"}".to_owned();
    let intent = InvocationIntent {
        invocation_id: "invocation-1".into(),
        tool_call_id: "call-1".into(),
        tool_name: "mcp__workspace__Bash".into(),
        effective_tool_name: "mcp__workspace__Bash".into(),
        effective_arguments_json: arguments_json.clone(),
        effective_arguments_digest: format!("{:x}", Sha256::digest(arguments_json.as_bytes())),
        arguments_digest: format!("{:x}", Sha256::digest(arguments_json.as_bytes())),
        arguments_json,
        owner_identity: "authenticated-owner-1".into(),
        scope_id: "direct-child".into(),
        scope_epoch: Some(7),
        authorization_ref: "trusted-policy-1".into(),
        recovery_locator: "trusted-connection-1".into(),
    };
    let binding = TaskBinding {
        invocation_id: intent.invocation_id.clone(),
        owner_identity: intent.owner_identity.clone(),
        owner_task_id: "shell-1".into(),
        initiator_session_id: "direct-child".into(),
        recipient_lifecycle: 1,
        recovery_locator: intent.recovery_locator.clone(),
        authorization_ref: intent.authorization_ref.clone(),
    };
    let mut state = WorkState::default();
    state.invocations.insert(
        intent.invocation_id.clone(),
        InvocationRecord {
            intent,
            recipient_lifecycle: 1,
            work_id: Some("act-1".into()),
            status: InvocationStatus::DispatchAccepted,
            outcome: None,
            unknown_reason: None,
        },
    );
    state
        .task_bindings
        .insert(binding.invocation_id.clone(), binding);
    WorkSnapshot::from_state(
        &WorkQuery {
            session_id: "direct-child".into(),
            limit: 0,
        },
        ControlState::default(),
        state,
    )
}

#[test]
fn cold_binding_preserves_exact_direct_initiator_and_owner_epoch() {
    let mut snapshot = snapshot();
    snapshot.control.lifecycle = 2;
    let recovered = recover_task_binding(&snapshot, "authenticated-owner-1", "shell-1").unwrap();
    assert_eq!(recovered.binding.initiator_session_id, "direct-child");
    assert_eq!(recovered.binding.recipient_lifecycle, 1);
    assert_eq!(recovered.intent.scope_epoch, Some(7));
}

#[test]
fn unknown_owner_task_never_falls_back_to_root_or_current_session() {
    let snapshot = snapshot();
    assert!(recover_task_binding(&snapshot, "unknown-owner", "shell-1").is_err());
    assert!(recover_task_binding(&snapshot, "authenticated-owner-1", "unknown-task").is_err());
}

#[test]
fn root_catalog_cannot_adopt_child_binding() {
    let mut snapshot = snapshot();
    snapshot.session_id = "root".into();
    assert!(recover_task_binding(&snapshot, "authenticated-owner-1", "shell-1").is_err());
}

#[test]
fn task_binding_without_original_intent_is_unroutable() {
    let mut snapshot = snapshot();
    snapshot.state.invocations.clear();
    assert!(recover_task_binding(&snapshot, "authenticated-owner-1", "shell-1").is_err());
}

#[test]
fn conflicting_owner_authorization_or_scope_is_unroutable() {
    for changed in ["authorization", "owner", "scope", "locator"] {
        let mut snapshot = snapshot();
        let record = snapshot.state.invocations.get_mut("invocation-1").unwrap();
        match changed {
            "authorization" => record.intent.authorization_ref = "other-policy".into(),
            "owner" => record.intent.owner_identity = "other-owner".into(),
            "scope" => record.intent.scope_id = "root".into(),
            "locator" => record.intent.recovery_locator = "untrusted-connection".into(),
            _ => unreachable!(),
        }
        assert!(recover_task_binding(&snapshot, "authenticated-owner-1", "shell-1").is_err());
    }
}

#[test]
fn ambiguous_task_identity_cannot_choose_a_current_binding() {
    let mut snapshot = snapshot();
    let mut duplicate = snapshot.state.task_bindings["invocation-1"].clone();
    duplicate.invocation_id = "invocation-2".into();
    snapshot
        .state
        .task_bindings
        .insert(duplicate.invocation_id.clone(), duplicate);
    assert!(recover_task_binding(&snapshot, "authenticated-owner-1", "shell-1").is_err());
}
