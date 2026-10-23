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

fn ordinary(lifecycle: &InteractionLifecycle, msg: &str) -> OrdinaryDecision {
    lifecycle.route_ordinary("new".into(), AcpNotification::Other { msg: msg.into() })
}

fn buffered_messages(lifecycle: &InteractionLifecycle) -> Vec<String> {
    lifecycle
        .take_buffered_batch()
        .expect("pending 期间应能取出缓存批次")
        .into_iter()
        .map(|notification| {
            let AcpNotification::Other { msg } = notification else {
                panic!("unexpected notification")
            };
            msg
        })
        .collect()
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
            ordinary(&lifecycle, &index.to_string()),
            OrdinaryDecision::Buffered
        ));
    }
    lifecycle.commit_stable(start.generation, "new".into());
    let buffered = lifecycle
        .take_buffered_batch()
        .expect("commit 保留的缓存应在 flush 时取出");
    assert_eq!(buffered.len(), NEW_SESSION_EVENT_BUFFER_CAPACITY);
    for (index, notification) in buffered.into_iter().enumerate() {
        let AcpNotification::Other { msg } = notification else {
            panic!()
        };
        assert_eq!(msg, index.to_string());
    }
    assert!(lifecycle.take_buffered_batch().is_none());
}

/// commit 只切路由、不取缓存：初始化等待期间的新通知继续入队，flush 按旧→新投递。
#[test]
fn test_commit_stable_keeps_buffering_until_flush_in_wire_order() {
    let lifecycle = InteractionLifecycle::new();
    lifecycle.force_stable("old", false);
    let start = lifecycle
        .begin_transition(TransitionKind::New, None)
        .unwrap();
    assert!(matches!(
        ordinary(&lifecycle, "old-1"),
        OrdinaryDecision::Buffered
    ));
    assert!(matches!(
        ordinary(&lifecycle, "old-2"),
        OrdinaryDecision::Buffered
    ));
    lifecycle.commit_stable(start.generation, "new".into());
    assert!(matches!(
        ordinary(&lifecycle, "new-1"),
        OrdinaryDecision::Buffered
    ));
    assert_eq!(
        buffered_messages(&lifecycle),
        vec!["old-1", "old-2", "new-1"],
        "flush 必须保持 wire 顺序"
    );
    // 批次取出后 pending 仍为真：投递期间到达的通知进入下一批。
    assert!(matches!(
        ordinary(&lifecycle, "new-2"),
        OrdinaryDecision::Buffered
    ));
    assert_eq!(buffered_messages(&lifecycle), vec!["new-2"]);
    assert!(lifecycle.take_buffered_batch().is_none());
    assert!(matches!(
        ordinary(&lifecycle, "live"),
        OrdinaryDecision::Forward(_)
    ));
}

#[test]
fn test_fail_transition_clears_buffer_and_pending() {
    let lifecycle = InteractionLifecycle::new();
    lifecycle.force_stable("old", false);
    let start = lifecycle
        .begin_transition(TransitionKind::New, None)
        .unwrap();
    assert!(matches!(
        ordinary(&lifecycle, "stale"),
        OrdinaryDecision::Buffered
    ));
    lifecycle.fail_transition(start.generation);
    // force_stable 不触碰 flush 状态：若 fail_transition 未清缓存/pending，
    // 下一条通知会被当成待 flush 的缓存帧，而不是即时 Forward。
    lifecycle.force_stable("new", false);
    assert!(matches!(
        ordinary(&lifecycle, "live"),
        OrdinaryDecision::Forward(_)
    ));
    assert!(lifecycle.take_buffered_batch().is_none());
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

#[test]
fn cancelled_execution_closes_interactions_but_waits_for_one_terminal() {
    for managed in [false, true] {
        let lifecycle = InteractionLifecycle::new();
        lifecycle.force_stable("s1", false);
        assert!(lifecycle.bind_user_input_generation("s1", 1, "g1"));
        lifecycle
            .open_execution("s1", "g1", "run-1", managed)
            .unwrap();
        let request = register(&lifecycle, RequestId::Number(1));
        assert_eq!(lifecycle.cancel_active_prompt().len(), 1);
        assert!(!lifecycle.is_pending_owner(&request.owner));
        assert!(lifecycle.active_execution(false).is_none());
        assert!(
            lifecycle
                .open_execution("s1", "g1", "run-1", managed)
                .is_none()
        );
        assert!(lifecycle.should_forward_prompt_terminal("s1", Some("run-1")));
        lifecycle.close_prompt_by_wire_identity("s1", Some("run-1"));
        assert!(!lifecycle.should_forward_prompt_terminal("s1", Some("run-1")));
    }
}

#[test]
fn cancelled_old_execution_terminal_cannot_end_the_next_execution() {
    let lifecycle = InteractionLifecycle::new();
    lifecycle.force_stable("s1", false);
    assert!(lifecycle.bind_user_input_generation("s1", 1, "g1"));
    lifecycle.open_execution("s1", "g1", "run-1", true).unwrap();
    lifecycle.cancel_active_prompt();
    lifecycle.open_execution("s1", "g1", "run-2", true).unwrap();
    assert!(!lifecycle.should_forward_prompt_terminal("s1", Some("run-1")));
    lifecycle.cancel_active_prompt();
    assert!(!lifecycle.should_forward_prompt_terminal("s1", Some("run-1")));
    assert!(lifecycle.should_forward_prompt_terminal("s1", Some("run-2")));
    lifecycle.close_prompt_by_wire_identity("s1", Some("run-1"));
    assert!(lifecycle.should_forward_prompt_terminal("s1", Some("run-2")));
}

#[test]
fn repeated_cancel_does_not_discard_the_pending_terminal() {
    let lifecycle = InteractionLifecycle::new();
    lifecycle.force_stable("s1", false);
    assert!(lifecycle.bind_user_input_generation("s1", 1, "g1"));
    lifecycle.open_execution("s1", "g1", "run-1", true).unwrap();
    lifecycle.cancel_active_prompt();
    lifecycle.cancel_active_prompt();
    assert!(lifecycle.should_forward_prompt_terminal("s1", Some("run-1")));
    lifecycle.close_prompt_by_wire_identity("s1", Some("run-1"));
    assert!(!lifecycle.should_forward_prompt_terminal("s1", Some("run-1")));
}

#[test]
fn cancelled_local_prompt_lease_does_not_acknowledge_the_execution_terminal() {
    let lifecycle = InteractionLifecycle::new();
    lifecycle.force_stable("s1", false);
    let lease = lifecycle.open_prompt(Some("run-1".into())).unwrap();
    assert!(lifecycle.bind_user_input_generation("s1", 1, "g1"));
    assert!(
        lifecycle
            .open_execution("s1", "g1", "run-1", false)
            .is_none()
    );
    lifecycle.cancel_active_prompt();
    lease.finish();
    assert!(lifecycle.should_forward_prompt_terminal("s1", Some("run-1")));
    lifecycle.close_prompt_by_wire_identity("s1", Some("run-1"));
    assert!(!lifecycle.should_forward_prompt_terminal("s1", Some("run-1")));
}
