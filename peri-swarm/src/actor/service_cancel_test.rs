use super::*;
use crate::domain::{InputState, TurnStatus};

use super::super::set_cancel_settle_wait;

const CANCEL_ID: &str = "0192f0c5-0000-7000-8000-000000000071";

fn cancel_submission(input_id: &str) -> InputSubmission {
    let mut control = steer_submission(input_id, "cancel");
    control.kind = InputKind::Control;
    control.mode = InputMode::Turn;
    control
}

fn running(request_id: &str) -> Option<TurnSlot> {
    Some(TurnSlot {
        request_id: request_id.to_owned(),
        started_at: "2026-01-01T00:00:00Z".to_owned(),
    })
}

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

#[tokio::test]
async fn a_cancel_that_never_reaches_the_host_writes_no_receipt() {
    // 关闭的传输：`session/cancel` 下发失败。
    let fixture = fixture(true).await;
    fixture.runtime.turn.send_replace(running("prompt-1"));
    let before = fixture
        .store
        .swarm_last_seq(LOGICAL_ID)
        .await
        .expect("last seq");

    let error = fixture
        .service
        .accept_input(&logical_id(), cancel_submission(CANCEL_ID))
        .await
        .expect_err("a closed transport cannot deliver the cancel");
    // HTTP 503：文案必须说“取消未送达”，不能声称“已受理”。
    assert_eq!(error.kind(), ErrorKind::Unavailable);
    assert_eq!(error.code(), ErrorCode::QueueUnavailable);
    assert!(
        error.message().contains("cancellation"),
        "{}",
        error.message()
    );
    assert_eq!(error.details()["operation"], "session/cancel");
    // 没有留下“已受理”的持久收据：同键可以重新受理。
    assert!(fixture
        .store
        .swarm_get_input(LOGICAL_ID, CANCEL_ID)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        fixture
            .store
            .swarm_last_seq(LOGICAL_ID)
            .await
            .expect("last seq"),
        before
    );
    assert!(fixture.runtime.turn.borrow().is_some(), "turn slot kept");
}

#[tokio::test]
async fn a_timed_out_cancel_is_delivered_again_on_the_same_key_without_new_items() {
    let fixture = fixture(false).await;
    // 宿主收到取消但不应答终态：等待窗口到期后仍必须以 202 返回。
    set_cancel_settle_wait(Duration::from_millis(50));
    fixture.runtime.turn.send_replace(running("prompt-1"));
    let before = fixture
        .store
        .swarm_last_seq(LOGICAL_ID)
        .await
        .expect("last seq");

    let accepted = bounded(
        fixture
            .service
            .accept_input(&logical_id(), cancel_submission(CANCEL_ID)),
        "cancel acceptance",
    )
    .await
    .expect("delivered cancel is accepted");
    assert_eq!(accepted.outcome.disposition, Disposition::Inserted);
    let first = bounded(fixture.server.recv(), "first session/cancel")
        .await
        .expect("notification");
    assert!(
        matches!(first, IncomingMessage::Notification { ref method, .. } if method == "session/cancel"),
        "{first:?}"
    );
    // 已送达但宿主未确认：收据存在且仍是 accepted。
    let receipt = fixture
        .store
        .swarm_get_input(LOGICAL_ID, CANCEL_ID)
        .await
        .unwrap()
        .expect("receipt");
    assert_eq!(receipt.state, InputState::Accepted);
    assert_eq!(
        fixture.runtime.turn.borrow().as_ref().unwrap().request_id,
        "prompt-1"
    );

    // 同键重放：目标 turn 仍在 slot → 重新下发 session/cancel，返回 existing。
    let replay = bounded(
        fixture
            .service
            .accept_input(&logical_id(), cancel_submission(CANCEL_ID)),
        "cancel replay",
    )
    .await
    .expect("replay");
    assert_eq!(replay.outcome.disposition, Disposition::Existing);
    let second = bounded(fixture.server.recv(), "re-delivered session/cancel")
        .await
        .expect("notification");
    assert!(
        matches!(second, IncomingMessage::Notification { ref method, .. } if method == "session/cancel"),
        "{second:?}"
    );
    // 重试不新增 item。
    assert_eq!(
        fixture
            .store
            .swarm_last_seq(LOGICAL_ID)
            .await
            .expect("last seq"),
        before
    );

    // 目标 turn 离开 slot 后，后来的 turn 不会被同键重放误取消。
    fixture.runtime.turn.send_replace(running("prompt-2"));
    let replay = bounded(
        fixture
            .service
            .accept_input(&logical_id(), cancel_submission(CANCEL_ID)),
        "cancel replay after the turn ended",
    )
    .await
    .expect("replay");
    assert_eq!(replay.outcome.disposition, Disposition::Existing);
    let quiet = tokio::time::timeout(Duration::from_millis(100), fixture.server.recv()).await;
    assert!(
        quiet.is_err(),
        "a later turn must not be cancelled: {quiet:?}"
    );
    assert_eq!(
        fixture
            .store
            .swarm_last_seq(LOGICAL_ID)
            .await
            .expect("last seq"),
        before
    );
    set_cancel_settle_wait(Duration::from_secs(0));
}
