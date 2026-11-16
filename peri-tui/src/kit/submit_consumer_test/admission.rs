//! 会话操作身份与认领原子性回归。

use super::super::CancellationToken;
use super::super::LaneKey;
use super::super::SubmitRequest;
use super::super::admission_probe;
use super::super::mpsc;

use peri_acp::transport::AcpTransport;
use serde_json::json;
use serial_test::serial;
use std::sync::Arc;
use std::time::Duration;

/// [回归 review #359] A 上连续预约 B、C：m0 在两者都未开始时入队，m1 在 C
/// 进入 Transitioning 后入队。两者必须以最新预约 C 为同一条有序车道，且
/// m0 的 prompt 未返回时 m1 不得上 wire。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn test_two_reserved_loads_keep_single_lane() {
    let (client, server) = super::support::make_client_without_pump();
    client.force_stable_for_test("session-a", false);
    let reservation_b = Arc::new(client.reserve_session_load());
    let reservation_c = Arc::new(client.reserve_session_load());

    let (registration_tx, mut registration_rx) = mpsc::unbounded_channel();
    admission_probe::install_registration_probe(registration_tx);

    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer =
        super::super::spawn_submit_consumer(client.clone(), rx, ".".into(), shutdown.clone());

    // 两次预约都还未开始：m0 必须等用户最新的决定 C，而不是预测 B 的代际。
    tx.send(super::support::agent_text("reserved-m0")).expect(
        "two reserved loads keep single lane: fixture channel must deliver its expected message",
    );
    let m0_lane = next_registration(&mut registration_rx, "reserved-m0").await;

    // B 的 load 先完成并释放预约。
    let loader_b = client.clone();
    let held_b = Arc::clone(&reservation_b);
    let load_b = tokio::spawn(async move {
        loader_b
            .load_reserved_session(held_b.as_ref(), "session-b", ".", None)
            .await
    });
    let (close_a, close_a_params) = super::support::expect_request(&server, "session/close").await;
    assert_eq!(close_a_params["sessionId"], "session-a");
    server.send_response(close_a, Ok(json!({}))).await.expect(
        "two reserved loads keep single lane: fixture task and request must complete successfully",
    );
    let (load_b_id, load_b_params) = super::support::expect_request(&server, "session/load").await;
    assert_eq!(load_b_params["sessionId"], "session-b");
    server
        .send_response(load_b_id, Ok(json!({"sessionId": "session-b"})))
        .await
        .expect("two reserved loads keep single lane: fixture task and request must complete successfully");
    load_b.await.expect("two reserved loads keep single lane: fixture task and request must complete successfully").expect("two reserved loads keep single lane: fixture task and request must complete successfully");
    drop(reservation_b);
    super::support::wait_for_session(&client, "session-b").await;

    // C 的 load 只进入 Transitioning：此时 m1 入队。
    let loader_c = client.clone();
    let held_c = Arc::clone(&reservation_c);
    let load_c = tokio::spawn(async move {
        loader_c
            .load_reserved_session(held_c.as_ref(), "session-c", ".", None)
            .await
    });
    let (close_b, close_b_params) = super::support::expect_request(&server, "session/close").await;
    assert_eq!(close_b_params["sessionId"], "session-b");
    server.send_response(close_b, Ok(json!({}))).await.expect(
        "two reserved loads keep single lane: fixture task and request must complete successfully",
    );
    let (load_c_id, load_c_params) = super::support::expect_request(&server, "session/load").await;
    assert_eq!(load_c_params["sessionId"], "session-c");

    tx.send(super::support::agent_text("reserved-m1")).expect(
        "two reserved loads keep single lane: fixture channel must deliver its expected message",
    );
    let m1_lane = next_registration(&mut registration_rx, "reserved-m1").await;
    assert_eq!(
        m0_lane, m1_lane,
        "同一目标操作的所有输入必须共用一条有序车道"
    );

    // 放行 C 并释放预约：m0/m1 都应落到 C。
    server
        .send_response(load_c_id, Ok(json!({"sessionId": "session-c"})))
        .await
        .expect("two reserved loads keep single lane: fixture task and request must complete successfully");
    load_c.await.expect("two reserved loads keep single lane: fixture task and request must complete successfully").expect("two reserved loads keep single lane: fixture task and request must complete successfully");
    drop(reservation_c);
    super::support::wait_for_session(&client, "session-c").await;

    let (m0_id, m0_session, m0_text) = super::support::expect_prompt(&server).await;
    assert_eq!(
        (m0_session.as_str(), m0_text.as_str()),
        ("session-c", "reserved-m0")
    );
    let overlap = tokio::time::timeout(Duration::from_millis(400), server.recv()).await;
    assert!(
        overlap.is_err(),
        "m0 prompt 未返回时 m1 不得上 wire: {overlap:?}"
    );
    server
        .send_response(m0_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .expect("two reserved loads keep single lane: fixture task and request must complete successfully");
    let (m1_id, m1_session, m1_text) = super::support::expect_prompt(&server).await;
    assert_eq!(
        (m1_session.as_str(), m1_text.as_str()),
        ("session-c", "reserved-m1")
    );
    server
        .send_response(m1_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .expect("two reserved loads keep single lane: fixture task and request must complete successfully");

    admission_probe::clear_registration_probe();
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
}

/// [回归 review #359] 认领与路由切换必须共享同一原子观察边界。
///
/// 屏障卡在「预约已认领、路由尚未切换」：此时入队的输入不得绑定旧 Stable A。
/// 旧的分步实现会在这里读到「已认领 + 旧 Stable A」，把 m0 绑定到 A 后取消；
/// 原子实现下准入快照必须等到 Transitioning，随后 m0 落到 B 且不丢。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn test_claimed_reservation_is_atomic_with_route_switch() {
    let (client, server) = super::support::make_client_without_pump();
    client.force_stable_for_test("session-a", false);
    let reservation = Arc::new(client.reserve_session_load());

    let (claim_tx, claim_rx) = std::sync::mpsc::channel();
    client.install_transition_claim_hook(claim_tx);

    let loader = client.clone();
    let held = Arc::clone(&reservation);
    let load = tokio::spawn(async move {
        loader
            .load_reserved_session(held.as_ref(), "session-b", ".", None)
            .await
    });
    let release = claim_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("load 必须在认领后停在屏障");

    let (registration_tx, mut registration_rx) = mpsc::unbounded_channel();
    admission_probe::install_registration_probe(registration_tx);
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer =
        super::super::spawn_submit_consumer(client.clone(), rx, ".".into(), shutdown.clone());

    // 屏障期间提交：旧实现会立刻完成分道并绑定 Stable A。
    tx.send(super::support::agent_text("atomic-m0")).expect("claimed reservation is atomic with route switch: fixture channel must deliver its expected message");
    let early = registration_within(
        &mut registration_rx,
        "atomic-m0",
        Duration::from_millis(250),
    )
    .await;
    assert!(
        early.is_none(),
        "认领后的准入快照必须与路由切换原子: {early:?}"
    );

    release.send(()).expect("claimed reservation is atomic with route switch: fixture channel must deliver its expected message");
    let m0_lane = next_registration(&mut registration_rx, "atomic-m0").await;
    assert!(
        matches!(m0_lane, LaneKey::Cohort(_)),
        "输入必须等待这次 load 的目标操作，而不是绑定旧会话: {m0_lane:?}"
    );

    // 放行切换：close A → load B。
    let (close_id, close_params) = super::support::expect_request(&server, "session/close").await;
    assert_eq!(close_params["sessionId"], "session-a");
    server.send_response(close_id, Ok(json!({}))).await.expect("claimed reservation is atomic with route switch: fixture task and request must complete successfully");
    let (load_id, load_params) = super::support::expect_request(&server, "session/load").await;
    assert_eq!(load_params["sessionId"], "session-b");
    server
        .send_response(load_id, Ok(json!({"sessionId": "session-b"})))
        .await
        .expect("claimed reservation is atomic with route switch: fixture task and request must complete successfully");
    load.await.expect("claimed reservation is atomic with route switch: fixture task and request must complete successfully").expect("claimed reservation is atomic with route switch: fixture task and request must complete successfully");
    drop(reservation);
    super::support::wait_for_session(&client, "session-b").await;

    let (m0_id, m0_session, m0_text) = super::support::expect_prompt(&server).await;
    assert_eq!(
        (m0_session.as_str(), m0_text.as_str()),
        ("session-b", "atomic-m0"),
        "预约认领后入队的输入不得丢"
    );
    server
        .send_response(m0_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .expect("claimed reservation is atomic with route switch: fixture task and request must complete successfully");

    admission_probe::clear_registration_probe();
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
}

/// 注册探针是进程级共享的：并行测试的标签会混入同一通道，按唯一标签过滤。
async fn next_registration(
    rx: &mut mpsc::UnboundedReceiver<(String, LaneKey)>,
    expected: &str,
) -> LaneKey {
    registration_within(rx, expected, Duration::from_secs(5))
        .await
        .unwrap_or_else(|| panic!("registration probe must report {expected}"))
}

async fn registration_within(
    rx: &mut mpsc::UnboundedReceiver<(String, LaneKey)>,
    expected: &str,
    within: Duration,
) -> Option<LaneKey> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some((label, lane))) if label == expected => return Some(lane),
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return None,
        }
    }
}
