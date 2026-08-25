use peri_acp::transport::types::RequestId;
use serde_json::json;

use super::*;

fn started() -> (InteractionLifecycle, PromptLease) {
    let lifecycle = InteractionLifecycle::new();
    lifecycle.force_stable("s1", false);
    let lease = lifecycle.open_prompt(Some("p1".into())).unwrap();
    (lifecycle, lease)
}

fn register(lifecycle: &InteractionLifecycle, id: RequestId) -> RegisteredReverseRequest {
    match lifecycle.register_reverse(
        ReverseInteractionKind::Permission,
        id,
        Some("s1"),
        json!({"sessionId": "s1"}),
    ) {
        RegisterDecision::Forward(request) => request,
        RegisterDecision::Settle { .. } => panic!("active prompt 应注册 reverse request"),
    }
}

#[test]
fn test_reverse_registers_before_forward_and_owner_is_monotonic() {
    let (lifecycle, _lease) = started();
    let first = register(&lifecycle, RequestId::Number(1));
    let second = register(&lifecycle, RequestId::Number(2));
    assert!(lifecycle.is_pending_owner(&first.owner));
    assert!(second.owner.token > first.owner.token);
}

#[test]
fn test_claim_user_then_lifecycle_is_first_claim_only() {
    let (lifecycle, _lease) = started();
    let request = register(&lifecycle, RequestId::Number(1));
    assert!(
        lifecycle
            .claim(&request.owner, ClaimCause::UserResponse)
            .is_some()
    );
    assert!(
        lifecycle
            .claim(&request.owner, ClaimCause::LifecycleDrain)
            .is_none()
    );
}

#[test]
fn test_claim_lifecycle_then_user_is_first_claim_only() {
    let (lifecycle, _lease) = started();
    let request = register(&lifecycle, RequestId::Number(1));
    assert!(
        lifecycle
            .claim(&request.owner, ClaimCause::LifecycleDrain)
            .is_some()
    );
    assert!(
        lifecycle
            .claim(&request.owner, ClaimCause::UserResponse)
            .is_none()
    );
}

#[test]
fn test_bridge_reject_and_delivery_failure_share_one_claim() {
    let (lifecycle, _lease) = started();
    let request = register(&lifecycle, RequestId::Number(1));
    assert!(
        lifecycle
            .claim(&request.owner, ClaimCause::BridgeReject)
            .is_some()
    );
    assert!(
        lifecycle
            .claim(&request.owner, ClaimCause::BridgeReject)
            .is_none()
    );
}

#[test]
fn test_transport_terminal_drains_without_wire_plan() {
    let (lifecycle, _lease) = started();
    let request = register(&lifecycle, RequestId::Number(1));
    let claims = lifecycle.transport_terminal();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].cause, ClaimCause::TransportTerminal);
    assert!(!lifecycle.is_pending_owner(&request.owner));
}

#[test]
fn test_same_request_id_in_new_generation_has_distinct_owner() {
    let (lifecycle, lease) = started();
    let first = register(&lifecycle, RequestId::Number(1));
    drop(lease);
    let start = lifecycle
        .begin_transition(TransitionKind::Load, Some("s1".into()))
        .unwrap();
    lifecycle.commit_stable(start.generation, "s1".into());
    let _lease = lifecycle.open_prompt(Some("p2".into())).unwrap();
    let second = register(&lifecycle, RequestId::Number(1));
    assert_ne!(first.owner, second.owner);
}

#[test]
fn test_same_token_shape_in_new_client_instance_has_distinct_owner() {
    let (first_lifecycle, _first_lease) = started();
    let (second_lifecycle, _second_lease) = started();
    let first = register(&first_lifecycle, RequestId::Number(1));
    let second = register(&second_lifecycle, RequestId::Number(1));
    assert_ne!(
        first.owner.client_instance_id,
        second.owner.client_instance_id
    );
}

#[test]
fn test_turn_terminal_closes_acceptance_and_next_prompt_reopens() {
    let (lifecycle, lease) = started();
    let first = register(&lifecycle, RequestId::Number(1));
    assert_eq!(lease.finish().len(), 1);
    assert!(matches!(
        lifecycle.register_reverse(
            ReverseInteractionKind::Permission,
            RequestId::Number(2),
            Some("s1"),
            json!({}),
        ),
        RegisterDecision::Settle { .. }
    ));
    let _next = lifecycle.open_prompt(Some("p2".into())).unwrap();
    let second = register(&lifecycle, RequestId::Number(3));
    assert!(second.owner.prompt_epoch > first.owner.prompt_epoch);
}

#[test]
fn test_cancel_closes_prompt_without_changing_generation_and_next_prompt_reopens() {
    let (lifecycle, _lease) = started();
    let first = register(&lifecycle, RequestId::Number(1));
    assert_eq!(lifecycle.cancel_active_prompt().len(), 1);
    let _next = lifecycle.open_prompt(Some("p2".into())).unwrap();
    let second = register(&lifecycle, RequestId::Number(2));
    assert_eq!(first.owner.generation, second.owner.generation);
    assert!(second.owner.prompt_epoch > first.owner.prompt_epoch);
}

#[test]
fn test_load_transition_routes_target_replay_but_rejects_reverse() {
    let lifecycle = InteractionLifecycle::new();
    lifecycle.force_stable("old", false);
    lifecycle
        .begin_transition(TransitionKind::Load, Some("new".into()))
        .unwrap();
    assert!(matches!(
        lifecycle.route_ordinary(
            "new".into(),
            AcpNotification::Other {
                msg: "replay".into()
            }
        ),
        OrdinaryDecision::Forward(_)
    ));
    assert!(matches!(
        lifecycle.register_reverse(
            ReverseInteractionKind::Permission,
            RequestId::Number(1),
            Some("new"),
            json!({}),
        ),
        RegisterDecision::Settle { .. }
    ));
}

#[test]
fn test_new_transition_buffers_only_bounded_ordinary_notifications_fifo() {
    let lifecycle = InteractionLifecycle::new();
    lifecycle.force_stable("old", false);
    let start = lifecycle
        .begin_transition(TransitionKind::New, None)
        .unwrap();
    for index in 0..(NEW_SESSION_EVENT_BUFFER_CAPACITY + 1) {
        assert!(matches!(
            lifecycle.route_ordinary(
                "new".into(),
                AcpNotification::Other {
                    msg: index.to_string()
                },
            ),
            OrdinaryDecision::Buffered
        ));
    }
    let buffered = lifecycle.commit_stable(start.generation, "new".into());
    assert_eq!(buffered.len(), NEW_SESSION_EVENT_BUFFER_CAPACITY);
    for (index, notification) in buffered.into_iter().enumerate() {
        let AcpNotification::Other { msg } = notification else {
            panic!()
        };
        assert_eq!(msg, index.to_string());
    }
}

#[test]
fn test_no_session_does_not_restore_bootstrap_wildcard() {
    let lifecycle = InteractionLifecycle::new();
    lifecycle.force_stable("old", false);
    let start = lifecycle
        .begin_transition(TransitionKind::New, None)
        .unwrap();
    lifecycle.fail_transition(start.generation);
    assert!(matches!(
        lifecycle.route_ordinary("old".into(), AcpNotification::Other { msg: "old".into() }),
        OrdinaryDecision::Drop
    ));
}

#[test]
fn test_delete_noncurrent_preserves_active_pending_generation() {
    let (lifecycle, _lease) = started();
    let request = register(&lifecycle, RequestId::Number(1));
    lifecycle.mark_deleted("other");
    assert!(lifecycle.is_pending_owner(&request.owner));
    assert_eq!(lifecycle.current_session_id().as_deref(), Some("s1"));
}
