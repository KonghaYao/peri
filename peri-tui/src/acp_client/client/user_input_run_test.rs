use std::sync::{Arc, atomic::Ordering};
use std::time::Duration;

use peri_acp::event::AcpEvent;
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair, types::RequestId};
use serde_json::json;

use super::*;
use crate::acp_client::interaction_lifecycle::TransitionKind;

fn bound_lifecycle() -> InteractionLifecycle {
    let lifecycle = InteractionLifecycle::new();
    lifecycle.force_stable("s1", false);
    assert!(lifecycle.bind_user_input_generation("s1", 1, "mailbox-1"));
    lifecycle
}

#[tokio::test]
async fn test_user_input_run_wire_opens_permission_and_ask_user_until_matching_done() {
    let (transport, server) = mpsc_transport_pair();
    let (client, tx, mut rx) = AcpTuiClient::new_interactive(transport);
    client.lifecycle.force_stable("s1", false);
    client.user_input_queue.store(true, Ordering::Release);
    assert!(
        client
            .lifecycle
            .bind_user_input_generation("s1", 1, "mailbox-1")
    );
    client.spawn_pump(tx);
    let server = Arc::new(server);
    server.send_notification("peri/agent_event", json!({
        "sessionId": "s1", "event_json": serde_json::to_string(&AcpEvent::UserInputRunStarted {
            generation: "mailbox-1".into(), request_id: "run-1".into(),
        }).unwrap(),
    })).await.unwrap();
    assert!(matches!(rx.recv().await, Some(AcpNotification::AgentEvent {
        event: AcpEvent::UserInputRunStarted { request_id, .. }, ..
    }) if request_id == "run-1"));
    for method in ["session/request_permission", "elicitation/create"] {
        let request_server = Arc::clone(&server);
        let request = tokio::spawn(async move {
            request_server
                .send_request(method, json!({ "sessionId": "s1" }))
                .await
                .unwrap()
        });
        let notification = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let owner = match notification {
            AcpNotification::RequestPermission { owner, .. }
            | AcpNotification::Elicitation { owner, .. } => owner,
            other => panic!("应建立反向交互 owner，实际为 {other:?}"),
        };
        assert_eq!(owner.session_id, "s1");
        if method == "session/request_permission" {
            client
                .respond_interaction(
                    &owner,
                    json!({"outcome":{"outcome":"cancelled"}}),
                    "cancelled".into(),
                )
                .await
                .unwrap();
            request.await.unwrap();
            assert!(matches!(
                rx.recv().await,
                Some(AcpNotification::InteractionTerminal { .. })
            ));
        } else {
            server
                .send_notification(
                    "peri/agent_event_done",
                    json!({"sessionId":"s1","requestId":"old-run"}),
                )
                .await
                .unwrap();
            server
                .send_notification("session/update", json!({"sessionId":"s1","test":"barrier"}))
                .await
                .unwrap();
            assert!(matches!(
                rx.recv().await,
                Some(AcpNotification::SessionUpdate { .. })
            ));
            assert!(
                client.lifecycle.is_pending_owner(&owner),
                "旧 done 不得取消新 AskUser"
            );
            server
                .send_notification(
                    "peri/agent_event_done",
                    json!({"sessionId":"s1","requestId":"run-1"}),
                )
                .await
                .unwrap();
            assert!(
                matches!(rx.recv().await, Some(AcpNotification::InteractionTerminal { owner: ended, .. }) if ended == owner)
            );
            assert!(matches!(
                rx.recv().await,
                Some(AcpNotification::AgentDone { .. })
            ));
            let response = request.await.unwrap();
            assert_eq!(response["action"], "cancel");
        }
    }
    assert!(matches!(
        client.lifecycle.register_reverse(
            ReverseInteractionKind::Permission,
            RequestId::Number(99),
            Some("s1"),
            json!({}),
        ),
        RegisterDecision::Settle { .. }
    ));
}

/// [回归测试] 快照先恢复 B 时，阻塞在 operation gate 后的 A done 不能重置 B。
#[tokio::test]
async fn test_user_input_run_snapshot_before_old_done_keeps_new_run_active() {
    let (transport, server) = mpsc_transport_pair();
    let (client, tx, mut rx) = AcpTuiClient::new_interactive(transport);
    client.lifecycle.force_stable("s1", false);
    client.user_input_queue.store(true, Ordering::Release);
    client
        .lifecycle
        .bind_user_input_generation("s1", 1, "mailbox-1");
    client.spawn_pump(tx);
    let started = |id: &str| AcpEvent::UserInputRunStarted {
        generation: "mailbox-1".into(),
        request_id: id.into(),
    };
    server
        .send_notification(
            "peri/agent_event",
            json!({"sessionId":"s1", "event":started("A")}),
        )
        .await
        .unwrap();
    assert!(matches!(
        rx.recv().await,
        Some(AcpNotification::AgentEvent { .. })
    ));
    let gate = client.lifecycle.operation_gate().lock().await;
    server
        .send_notification(
            "peri/agent_event_done",
            json!({"sessionId":"s1", "requestId":"A"}),
        )
        .await
        .unwrap();
    client
        .lifecycle
        .open_user_input_run("s1", "mailbox-1", "B")
        .unwrap();
    client.flush_buffered(vec![AcpNotification::AgentEvent {
        session_id: "s1".into(),
        event: started("B"),
    }]);
    assert!(matches!(rx.recv().await, Some(AcpNotification::AgentEvent {
        event: AcpEvent::UserInputRunStarted { request_id, .. }, ..
    }) if request_id == "B"));
    drop(gate);
    server
        .send_notification(
            "peri/agent_event",
            json!({"sessionId":"s1", "event":started("B")}),
        )
        .await
        .unwrap();
    server
        .send_notification(
            "session/update",
            json!({"sessionId":"s1", "test":"barrier"}),
        )
        .await
        .unwrap();
    assert!(
        matches!(rx.recv().await, Some(AcpNotification::SessionUpdate { .. })),
        "旧 done 和重复开始都不得再次发布 UI 状态"
    );
    assert_eq!(
        client.lifecycle.active_user_input_run(),
        Some(("s1".into(), "mailbox-1".into(), "B".into()))
    );
    assert!(
        matches!(
            client.lifecycle.register_reverse(
                ReverseInteractionKind::Elicitation,
                RequestId::Number(99),
                Some("s1"),
                json!({}),
            ),
            RegisterDecision::Forward(_)
        ),
        "B 仍接纳 AskUser"
    );
}

#[test]
fn test_user_input_run_rejects_old_generation_and_duplicate_start_after_done() {
    let lifecycle = bound_lifecycle();
    assert!(
        lifecycle
            .open_user_input_run("s1", "old-mailbox", "run-1")
            .is_none()
    );
    assert!(
        lifecycle
            .open_user_input_run("s2", "mailbox-1", "run-1")
            .is_none()
    );
    assert!(
        lifecycle
            .open_user_input_run("s1", "mailbox-1", "run-1")
            .is_some()
    );
    assert!(
        lifecycle
            .open_user_input_run("s1", "mailbox-1", "run-1")
            .is_none()
    );
    lifecycle.close_prompt_by_wire_identity("s1", Some("run-1"));
    assert!(
        lifecycle
            .open_user_input_run("s1", "mailbox-1", "run-1")
            .is_none()
    );
    assert!(
        lifecycle
            .open_user_input_run("s1", "mailbox-1", "run-2")
            .is_some()
    );
}

#[test]
fn test_user_input_run_returning_to_live_session_can_restore_same_run() {
    let lifecycle = bound_lifecycle();
    assert!(
        lifecycle
            .open_user_input_run("s1", "mailbox-1", "run-1")
            .is_some()
    );
    let away = lifecycle
        .begin_transition(TransitionKind::Load, Some("s2".into()))
        .unwrap();
    lifecycle.commit_stable(away.generation, "s2".into());
    let back = lifecycle
        .begin_transition(TransitionKind::Load, Some("s1".into()))
        .unwrap();
    lifecycle.commit_stable(back.generation, "s1".into());
    assert!(
        !lifecycle.bind_user_input_generation("s1", 1, "mailbox-1"),
        "旧快照不能跨 load 边界绑定"
    );
    assert!(lifecycle.bind_user_input_generation("s1", back.generation, "mailbox-1"));
    assert!(
        lifecycle
            .open_user_input_run("s1", "mailbox-1", "run-1")
            .is_some()
    );
    lifecycle.cancel_active_prompt();
    assert!(
        lifecycle
            .open_user_input_run("s1", "mailbox-1", "run-1")
            .is_none(),
        "Stop 后迟到快照不得重开交互"
    );
}

#[test]
fn test_user_input_run_new_attempt_retires_old_permission_owner() {
    let lifecycle = bound_lifecycle();
    lifecycle
        .open_user_input_run("s1", "mailbox-1", "run-1")
        .unwrap();
    let RegisterDecision::Forward(first) = lifecycle.register_reverse(
        ReverseInteractionKind::Permission,
        RequestId::Number(1),
        Some("s1"),
        json!({}),
    ) else {
        panic!("首轮应接纳权限请求")
    };
    let claims = lifecycle
        .open_user_input_run("s1", "mailbox-1", "run-2")
        .unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].owner, first.owner);
    assert!(!lifecycle.is_pending_owner(&first.owner));
    assert!(
        lifecycle
            .open_user_input_run("s1", "mailbox-1", "run-1")
            .is_none()
    );
}

#[test]
fn test_user_input_generation_can_change_only_after_session_boundary() {
    let lifecycle = bound_lifecycle();
    assert!(!lifecycle.bind_user_input_generation("s1", 1, "mailbox-2"));
    let transition = lifecycle
        .begin_transition(TransitionKind::Load, Some("s1".into()))
        .unwrap();
    lifecycle.commit_stable(transition.generation, "s1".into());
    assert!(lifecycle.bind_user_input_generation("s1", transition.generation, "mailbox-2"));
    assert!(
        lifecycle
            .open_user_input_run("s1", "mailbox-1", "run-1")
            .is_none()
    );
    assert!(
        lifecycle
            .open_user_input_run("s1", "mailbox-2", "run-1")
            .is_some()
    );
}
