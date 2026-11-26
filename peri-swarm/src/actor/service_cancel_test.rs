use super::*;
use crate::domain::TurnStatus;

#[tokio::test]
async fn cancel_sends_acp_notification_and_the_terminal_settles_pending_approvals() {
    let fixture = fixture(false).await;
    let approval_id = pending_approval(&fixture).await;
    fixture.runtime.turn.send_replace(Some(TurnSlot {
        request_id: "prompt-1".to_owned(),
        started_at: "2026-01-01T00:00:00Z".to_owned(),
    }));
    let mut events = fixture.runtime.channel.subscribe();
    let mut control = steer_submission("0192f0c5-0000-7000-8000-000000000071", "cancel");
    control.kind = InputKind::Control;
    control.mode = InputMode::Turn;
    let service = Arc::clone(&fixture.service);
    let request = tokio::spawn(async move { service.accept_input(&logical_id(), control).await });
    let notification = bounded(fixture.server.recv(), "cancel did not reach ACP transport")
        .await
        .expect("notification");
    let IncomingMessage::Notification { method, params } = notification else {
        panic!("expected a cancellation notification, got {notification:?}");
    };
    assert_eq!(method, "session/cancel");
    assert_eq!(params, json!({"sessionId": "sess-approval-test"}));

    // 模拟宿主报告取消终态；通知本身不会替宿主写终态。
    fixture
        .log
        .finish_turn("prompt-1", TurnOutcome::Completed("cancelled".into()))
        .await;
    fixture.runtime.turn.send_replace(None);
    let accepted = bounded(request, "cancel acceptance did not return")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(accepted.outcome.disposition, Disposition::Inserted);
    assert!(accepted.pending_turn.is_none());
    let turn = fixture
        .store
        .swarm_get_turn(LOGICAL_ID, "turn_1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(turn.status, TurnStatus::Cancelled);
    let mut observed = Vec::new();
    while let Ok(event) = events.try_recv() {
        observed.push(event);
    }
    let (_, required) = requires_action_snapshot(&observed).expect("settlement snapshot");
    assert!(required.is_empty());
    let rows = approval_rows(&fixture, &approval_id).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].payload["state"], "settled");
    assert_eq!(rows[1].payload["reason"], "turn_ended");
}
