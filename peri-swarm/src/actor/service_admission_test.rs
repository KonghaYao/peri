use super::*;

#[tokio::test]
async fn concurrent_turn_inputs_cannot_both_write_receipts_while_idle() {
    let fixture = fixture(false).await;
    let id = logical_id();
    let mut first = steer_submission("first", "one");
    first.mode = InputMode::Turn;
    let mut second = steer_submission("second", "two");
    second.mode = InputMode::Turn;

    // 卡住首条 user item 的写入；此时 receipt 已写，但 turn 尚未被 item 占住。
    let seq_guard = fixture.log.lock_seq_for_test().await;
    let first_accept = fixture.service.accept_input(&id, first.clone());
    tokio::pin!(first_accept);
    assert!(futures::poll!(&mut first_accept).is_pending());
    assert!(fixture
        .store
        .swarm_get_input(LOGICAL_ID, &first.input_id)
        .await
        .unwrap()
        .is_some());
    assert!(!fixture.log.has_open_turn());

    let second_accept = fixture.service.accept_input(&id, second.clone());
    tokio::pin!(second_accept);
    assert!(futures::poll!(&mut second_accept).is_pending());
    assert_unwritten(&fixture.store, &second.input_id).await;
    drop(seq_guard);

    let pending = bounded(first_accept, "first acceptance must not deadlock")
        .await
        .expect("first input accepted")
        .pending_turn
        .expect("first input gets the pending turn");
    let error = bounded(second_accept, "second acceptance must not deadlock")
        .await
        .expect_err("a pending turn blocks a competing opener");
    assert_eq!(error.code(), ErrorCode::TurnInProgress);
    assert_unwritten(&fixture.store, &second.input_id).await;
    assert_written(&fixture.store, &first).await;

    open_turn(&fixture.runtime, "req-1", &pending.turn_id).await;
    close_first_turn(&fixture.runtime).await;
    let retried = fixture
        .service
        .accept_input(&id, second.clone())
        .await
        .unwrap();
    assert_eq!(retried.outcome.disposition, Disposition::Inserted);
    assert!(retried.pending_turn.is_some());
    assert_written(&fixture.store, &second).await;
}
