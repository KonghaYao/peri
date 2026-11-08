//! 提交车道原语与 load commit 窗口回归。

use super::super::*;
use super::support::*;

use peri_acp::transport::AcpTransport;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// 车道保证同会话内顺序：每条提交都先让出调度器，仍按 push 顺序执行。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_submit_lanes_keep_fifo_within_session() {
    let mut lanes: SubmitLanes<Option<String>> = SubmitLanes::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    for index in 0..10 {
        let tx = tx.clone();
        lanes.push(Some("s".to_string()), async move {
            tokio::task::yield_now().await;
            let _ = tx.send(index);
        });
    }
    drop(tx);

    let mut received = Vec::new();
    for _ in 0..10 {
        received.push(
            tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("lane must drive all submissions")
                .expect("sender stays alive until all work runs"),
        );
    }
    assert_eq!(received, (0..10).collect::<Vec<_>>());
}

/// 车道首条 task panic 只结束它自己，后续消息继续执行。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_submit_lanes_continue_after_tail_panics() {
    let mut lanes: SubmitLanes<Option<String>> = SubmitLanes::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    lanes.push(Some("s".to_string()), async move {
        panic!("simulated submit task abort");
    });
    let follow_tx = tx.clone();
    lanes.push(Some("s".to_string()), async move {
        let _ = follow_tx.send("after");
    });
    drop(tx);

    let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("后续消息不得被前一条的 panic 阻塞");
    assert_eq!(received, Some("after"));
}

/// 不同会话各自推进：慢会话不能拖住另一会话的消息。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_submit_lanes_run_sessions_independently() {
    let mut lanes: SubmitLanes<Option<String>> = SubmitLanes::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();

    let blocked_tx = tx.clone();
    lanes.push(Some("a".to_string()), async move {
        let _ = release_rx.await;
        let _ = blocked_tx.send("a");
    });
    let free_tx = tx.clone();
    lanes.push(Some("b".to_string()), async move {
        let _ = free_tx.send("b");
    });
    drop(tx);

    let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("另一会话不得等待 a 的挂起提交")
        .expect("sender stays alive");
    assert_eq!(first, "b");

    release_tx.send(()).unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("a 释放后应继续执行")
        .expect("sender stays alive");
    assert_eq!(second, "a");
}

/// [回归 review #359] load 已 commit Stable、预约尚未释放时入队的输入，
/// 必须与既有队列共用同一条车道，不能预测 generation+1 另起 Session 车道。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_load_commit_window_stable_input_keeps_existing_lane() {
    let (client, server) = make_client_without_pump();
    client.force_stable_for_test("session-a", false);
    let reservation = Arc::new(client.reserve_session_load());

    // 线程切换路径：load 开始时认领预约，commit 后预约仍由调用方持有。
    let loader = client.clone();
    let held = Arc::clone(&reservation);
    let load = tokio::spawn(async move {
        loader
            .load_reserved_session(held.as_ref(), "session-b", ".", None)
            .await
    });
    let (close_id, close_params) = expect_request(&server, "session/close").await;
    assert_eq!(close_params["sessionId"], "session-a");
    server.send_response(close_id, Ok(json!({}))).await.unwrap();
    let (load_id, load_params) = expect_request(&server, "session/load").await;
    assert_eq!(load_params["sessionId"], "session-b");
    server
        .send_response(load_id, Ok(json!({"sessionId": "session-b"})))
        .await
        .unwrap();
    load.await.unwrap().unwrap();
    wait_for_session(&client, "session-b").await;
    assert_eq!(client.pending_session_load_count(), 1, "预约必须仍被持有");

    // 窗口内入队：route 已是 Stable(B, g)，预约尚未释放。
    let mut cohorts = Cohorts::new();
    let mut lanes: SubmitLanes<LaneKey> = SubmitLanes::new();
    let (window_lane, window_admission) = register_admission(&client, &mut cohorts, &lanes);
    assert_eq!(
        window_admission.binding.as_ref().map(|(id, _)| id.as_str()),
        Some("session-b"),
        "窗口内输入应绑定已落定的 Stable 会话"
    );

    // 既有队列占用车道；释放预约后的输入必须排在其后。
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel();
    let first_done = done_tx.clone();
    lanes.push(window_lane.clone(), async move {
        let _ = release_rx.await;
        let _ = first_done.send("first");
    });

    drop(reservation);
    let (later_lane, later_admission) = register_admission(&client, &mut cohorts, &lanes);
    assert_eq!(later_lane, window_lane, "窗口内与窗口后的输入必须共用车道");
    assert_eq!(
        later_admission.binding.as_ref().map(|(id, _)| id.as_str()),
        Some("session-b")
    );
    lanes.push(later_lane, async move {
        let _ = done_tx.send("second");
    });

    let early = tokio::time::timeout(Duration::from_millis(200), done_rx.recv()).await;
    assert!(early.is_err(), "同车道后续输入不得越过前序: {early:?}");
    release_tx.send(()).unwrap();
    assert_eq!(done_rx.recv().await, Some("first"));
    assert_eq!(done_rx.recv().await, Some("second"));
}
