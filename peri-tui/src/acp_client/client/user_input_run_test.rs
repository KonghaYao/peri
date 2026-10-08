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
async fn test_external_execution_stop_does_not_target_user_input_ticket() {
    let (transport, server) = mpsc_transport_pair();
    let (client, tx, mut rx) = AcpTuiClient::new_interactive(transport);
    client.lifecycle.force_stable("s1", false);
    client.user_input_queue.store(true, Ordering::Release);
    client.spawn_pump(tx);
    server
        .send_notification(
            "peri/agent_event",
            json!({"sessionId":"s1", "event": AcpEvent::ExecutionStarted {
                generation:"g1".into(), request_id:"internal".into(),
            }}),
        )
        .await
        .unwrap();
    assert!(matches!(
        rx.recv().await,
        Some(AcpNotification::AgentEvent { .. })
    ));
    assert!(client.lifecycle.active_execution(false).is_some());
    assert!(client.lifecycle.active_execution(true).is_none());
    client.cancel().await.unwrap();
    assert!(
        matches!(server.recv().await, Some(IncomingMessage::Notification {
        method, params,
    }) if method == "session/cancel" && params == json!({"sessionId":"s1"}))
    );
    client.close();
}

/// [回归测试] 内部执行开始不得绕过 generation、retired request 或选中会话边界。
#[tokio::test]
async fn test_execution_started_rejects_duplicate_generation_and_switched_session() {
    let (transport, server) = mpsc_transport_pair();
    let (client, tx, mut rx) = AcpTuiClient::new_interactive(transport);
    client.lifecycle.force_stable("s1", false);
    client.spawn_pump(tx);
    let started = |session: &str, generation: &str, request: &str| {
        json!({"sessionId":session,"event":AcpEvent::ExecutionStarted {
            generation: generation.into(), request_id: request.into(),
        }})
    };
    server
        .send_notification("peri/agent_event", started("s1", "g1", "A"))
        .await
        .unwrap();
    assert!(matches!(rx.recv().await, Some(AcpNotification::AgentEvent {
        event: AcpEvent::ExecutionStarted { request_id, .. }, ..
    }) if request_id == "A"));
    for event in [
        started("s1", "g1", "A"),
        started("s1", "g2", "B"),
        started("s2", "g1", "B"),
    ] {
        server
            .send_notification("peri/agent_event", event)
            .await
            .unwrap();
    }
    server
        .send_notification("session/update", json!({"sessionId":"s1","test":"barrier"}))
        .await
        .unwrap();
    assert!(matches!(
        rx.recv().await,
        Some(AcpNotification::SessionUpdate { .. })
    ));
    server
        .send_notification(
            "peri/agent_event_done",
            json!({"sessionId":"s1","requestId":"A"}),
        )
        .await
        .unwrap();
    assert!(
        matches!(rx.recv().await, Some(AcpNotification::AgentDone { request_id: Some(id), .. }) if id == "A")
    );
    server
        .send_notification("peri/agent_event", started("s1", "g1", "A"))
        .await
        .unwrap();
    server
        .send_notification(
            "session/update",
            json!({"sessionId":"s1","test":"retired-barrier"}),
        )
        .await
        .unwrap();
    assert!(matches!(
        rx.recv().await,
        Some(AcpNotification::SessionUpdate { .. })
    ));
    let transition = client
        .lifecycle
        .begin_transition(TransitionKind::Load, Some("s2".into()))
        .unwrap();
    client
        .lifecycle
        .commit_stable(transition.generation, "s2".into());
    server
        .send_notification("peri/agent_event", started("s1", "g1", "C"))
        .await
        .unwrap();
    server
        .send_notification("peri/agent_event", started("s2", "g2", "D"))
        .await
        .unwrap();
    assert!(matches!(rx.recv().await, Some(AcpNotification::AgentEvent {
        session_id, event: AcpEvent::ExecutionStarted { request_id, .. },
    }) if session_id == "s2" && request_id == "D"));
}

/// [回归测试] snapshot 尚未经 pump 到达时，真实执行开始本身即可首次绑定 generation。
#[tokio::test]
async fn test_execution_started_before_initial_snapshot_opens_lease() {
    let (transport, server) = mpsc_transport_pair();
    let (client, tx, mut rx) = AcpTuiClient::new_interactive(transport);
    client.lifecycle.force_stable("s1", false);
    client.user_input_queue.store(true, Ordering::Release);
    client.spawn_pump(tx);
    server
        .send_notification(
            "peri/agent_event",
            json!({
                "sessionId":"s1", "event":AcpEvent::ExecutionStarted {
                    generation:"g1".into(), request_id:"internal".into(),
                },
            }),
        )
        .await
        .unwrap();
    assert!(matches!(rx.recv().await, Some(AcpNotification::AgentEvent {
        event: AcpEvent::ExecutionStarted { request_id, .. }, ..
    }) if request_id == "internal"));
    assert!(matches!(
        client.lifecycle.register_reverse(
            ReverseInteractionKind::Permission,
            RequestId::Number(42),
            Some("s1"),
            json!({}),
        ),
        RegisterDecision::Forward(_)
    ));
    assert!(client.lifecycle.bind_user_input_generation("s1", 1, "g1"));
    assert!(
        !client
            .lifecycle
            .bind_user_input_generation("s1", 1, "other")
    );
}

#[tokio::test]
async fn test_execution_started_opens_first_permission_and_ask_until_matching_done() {
    let (transport, server) = mpsc_transport_pair();
    let (client, tx, mut rx) = AcpTuiClient::new_interactive(transport);
    client.lifecycle.force_stable("s1", false);
    client.spawn_pump(tx);
    let server = Arc::new(server);
    server
        .send_notification(
            "peri/agent_event",
            json!({
                "sessionId": "s1", "event_json": serde_json::to_string(&AcpEvent::ExecutionStarted {
                    generation: "mailbox-1".into(), request_id: "run-1".into(),
                }).unwrap(),
            }),
        )
        .await
        .unwrap();
    assert!(matches!(rx.recv().await, Some(AcpNotification::AgentEvent {
        event: AcpEvent::ExecutionStarted { request_id, .. }, ..
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
            client
                .respond_interaction(
                    &owner,
                    json!({"action":"accept","content":{"answer":"yes"}}),
                    "answered".into(),
                )
                .await
                .unwrap();
            let response = request.await.unwrap();
            assert_eq!(response["action"], "accept");
            assert!(matches!(
                rx.recv().await,
                Some(AcpNotification::InteractionTerminal { .. })
            ));
        }
    }
    let request_server = Arc::clone(&server);
    let pending = tokio::spawn(async move {
        request_server
            .send_request("elicitation/create", json!({"sessionId":"s1"}))
            .await
            .unwrap()
    });
    let AcpNotification::Elicitation { owner, .. } = rx.recv().await.unwrap() else {
        panic!("matching done 前应接纳 AskUser")
    };
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
    let response = pending.await.unwrap();
    assert_eq!(response["action"], "cancel");
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
        .open_execution("s1", "mailbox-1", "B", true)
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
        client.lifecycle.active_execution(false),
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
            .open_execution("s1", "old-mailbox", "run-1", true)
            .is_none()
    );
    assert!(
        lifecycle
            .open_execution("s2", "mailbox-1", "run-1", true)
            .is_none()
    );
    assert!(
        lifecycle
            .open_execution("s1", "mailbox-1", "run-1", true)
            .is_some()
    );
    assert!(
        lifecycle
            .open_execution("s1", "mailbox-1", "run-1", true)
            .is_none()
    );
    lifecycle.close_prompt_by_wire_identity("s1", Some("run-1"));
    assert!(
        lifecycle
            .open_execution("s1", "mailbox-1", "run-1", true)
            .is_none()
    );
    assert!(
        lifecycle
            .open_execution("s1", "mailbox-1", "run-2", true)
            .is_some()
    );
}

#[test]
fn test_user_input_run_returning_to_live_session_can_restore_same_run() {
    let lifecycle = bound_lifecycle();
    assert!(
        lifecycle
            .open_execution("s1", "mailbox-1", "run-1", true)
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
            .open_execution("s1", "mailbox-1", "run-1", true)
            .is_some()
    );
    lifecycle.cancel_active_prompt();
    assert!(
        lifecycle
            .open_execution("s1", "mailbox-1", "run-1", true)
            .is_none(),
        "Stop 后迟到快照不得重开交互"
    );
}

#[test]
fn test_user_input_run_new_attempt_retires_old_permission_owner() {
    let lifecycle = bound_lifecycle();
    lifecycle
        .open_execution("s1", "mailbox-1", "run-1", true)
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
        .open_execution("s1", "mailbox-1", "run-2", true)
        .unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].owner, first.owner);
    assert!(!lifecycle.is_pending_owner(&first.owner));
    assert!(
        lifecycle
            .open_execution("s1", "mailbox-1", "run-1", true)
            .is_none()
    );
}

/// [回归测试] 等待锁的本地 B 先开 lease，真实 A 开始后 B 实际开始必须重获 lease。
#[test]
fn test_execution_start_replaces_preopened_legacy_prompt_and_reopens_it() {
    let lifecycle = bound_lifecycle();
    let _pending_legacy = lifecycle.open_prompt(Some("B".into())).unwrap();
    assert!(
        lifecycle
            .open_execution("s1", "mailbox-1", "A", false)
            .is_some()
    );
    let RegisterDecision::Forward(first) = lifecycle.register_reverse(
        ReverseInteractionKind::Permission,
        RequestId::Number(1),
        Some("s1"),
        json!({}),
    ) else {
        panic!("真实 A 应接纳首次审批")
    };
    let claims = lifecycle
        .open_execution("s1", "mailbox-1", "B", false)
        .unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].owner, first.owner);
    assert!(matches!(
        lifecycle.register_reverse(
            ReverseInteractionKind::Elicitation,
            RequestId::Number(2),
            Some("s1"),
            json!({}),
        ),
        RegisterDecision::Forward(_)
    ));
    lifecycle.close_prompt_by_wire_identity("s1", Some("A"));
    assert_eq!(lifecycle.active_execution(false).unwrap().2, "B");
}

#[test]
fn test_execution_started_same_local_request_keeps_existing_prompt_owner() {
    let lifecycle = bound_lifecycle();
    let _local_prompt = lifecycle.open_prompt(Some("direct".into())).unwrap();
    let RegisterDecision::Forward(pending) = lifecycle.register_reverse(
        ReverseInteractionKind::Permission,
        RequestId::Number(1),
        Some("s1"),
        json!({}),
    ) else {
        panic!("本地 prompt 应接纳审批")
    };
    assert!(
        lifecycle
            .open_execution("s1", "mailbox-1", "direct", false)
            .is_none()
    );
    assert!(lifecycle.is_pending_owner(&pending.owner));
    assert_eq!(lifecycle.active_execution(false).unwrap().2, "direct");
    lifecycle.close_prompt_by_wire_identity("s1", Some("direct"));
    assert!(!lifecycle.is_pending_owner(&pending.owner));
    assert!(
        lifecycle
            .open_execution("s1", "mailbox-1", "direct", false)
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
            .open_execution("s1", "mailbox-1", "run-1", true)
            .is_none()
    );
    assert!(
        lifecycle
            .open_execution("s1", "mailbox-2", "run-1", true)
            .is_some()
    );
}
