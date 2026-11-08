//! 会话切换与初始化 cohort 取消/归属回归。

use super::super::*;
use super::support::*;

use crate::kit::atoms::NOTIFICATION;
use peri_acp::transport::AcpTransport;
use serde_json::json;
use serial_test::serial;
use std::time::Duration;

/// [回归 P1] 会话切换后，旧会话排队中的提交不得改投新会话。
///
/// A0 挂起 → A1 排队 → `/clear` 切到 B → 释放 A0。A1 在准入时必须被显式
/// 取消并给出反馈，wire 上不得出现发往 B 的 A1 prompt。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn test_queued_submit_is_cancelled_after_session_switch() {
    crate::kit::atoms::init_atoms();
    // 本用例会真实建会话并写 NOTIFICATION；与其它 serial 用例共用全局态。
    let previous_notification = std::mem::take(&mut *NOTIFICATION.state().write());
    let (client, server) = make_client_without_pump();
    client.force_stable_for_test("session-a", false);
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer = spawn_submit_consumer(client.clone(), rx, ".".into(), shutdown.clone());

    tx.send(agent_text("a0")).unwrap();
    let (a0_id, a0_session, a0_text) = expect_prompt(&server).await;
    assert_eq!((a0_session.as_str(), a0_text.as_str()), ("session-a", "a0"));

    // A1 在 A0 未返回时入队：车道与准入身份都固定为 session-a。
    tx.send(agent_text("a1")).unwrap();
    // 控制请求即时执行：/clear 关闭 A 并建立 B。
    tx.send(SubmitRequest::SessionControl(SessionControlRequest::Clear))
        .unwrap();

    let (close_id, close_params) = expect_request(&server, "session/close").await;
    assert_eq!(close_params["sessionId"], "session-a");
    server.send_response(close_id, Ok(json!({}))).await.unwrap();
    let (new_id, _) = expect_request(&server, "session/new").await;
    server
        .send_response(new_id, Ok(json!({"sessionId": "session-b"})))
        .await
        .unwrap();
    wait_for_session(&client, "session-b").await;

    // 旧会话 turn 结束，A1 被车道放行：准入校验必须拒绝它。
    server
        .send_response(a0_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();

    let stray = tokio::time::timeout(Duration::from_millis(400), server.recv()).await;
    assert!(stray.is_err(), "会话切换后旧提交不得改投新会话: {stray:?}");
    assert!(
        NOTIFICATION.state().read().is_some(),
        "取消排队提交必须有用户可见反馈"
    );

    *NOTIFICATION.state().write() = previous_notification;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
}
/// [回归 P1] 初始化 cohort 落定 A 之后切到 B：m0 取消，m1 必须随 cohort 一起
/// 取消——不得重新 ensure 并把旧消息投到 B。
///
/// 断点卡在「已采纳 A、尚未做准入校验/发送」：这正是旧实现空槽被后来者重新
/// 初始化的窗口。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn test_initializing_cohort_cancels_after_session_switch() {
    crate::kit::atoms::init_atoms();
    let previous_notification = std::mem::take(&mut *NOTIFICATION.state().write());
    let (client, server) = make_client_without_pump();
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();
    admission_probe::install("probe-session-a", probe_tx);

    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer = spawn_submit_consumer(client.clone(), rx, ".".into(), shutdown.clone());

    tx.send(agent_text("m0")).unwrap();
    tx.send(agent_text("m1")).unwrap();

    let (new_id, _) = expect_request(&server, "session/new").await;
    server
        .send_response(new_id, Ok(json!({"sessionId": "probe-session-a"})))
        .await
        .unwrap();
    wait_for_session(&client, "probe-session-a").await;
    let release = tokio::time::timeout(Duration::from_secs(5), probe_rx.recv())
        .await
        .expect("m0 落定身份后应停在断点")
        .expect("probe channel must stay open");

    // 断点期间切到 B：m0 的采纳身份随即失效。
    tx.send(SubmitRequest::SessionControl(SessionControlRequest::Clear))
        .unwrap();
    let (close_id, close_params) = expect_request(&server, "session/close").await;
    assert_eq!(close_params["sessionId"], "probe-session-a");
    server.send_response(close_id, Ok(json!({}))).await.unwrap();
    let (new_b, _) = expect_request(&server, "session/new").await;
    server
        .send_response(new_b, Ok(json!({"sessionId": "probe-session-b"})))
        .await
        .unwrap();
    wait_for_session(&client, "probe-session-b").await;

    release.send(()).unwrap();

    // 两条消息都不得发往 B，也不得再次 session/new（空槽不是重新初始化信号）。
    let stray = tokio::time::timeout(Duration::from_millis(500), server.recv()).await;
    assert!(
        stray.is_err(),
        "初始化 cohort 在切换后不得改投新会话: {stray:?}"
    );
    assert!(
        NOTIFICATION.state().read().is_some(),
        "取消排队提交必须有用户可见反馈"
    );

    admission_probe::clear();
    *NOTIFICATION.state().write() = previous_notification;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
}
/// [回归 P1] 同 ID 重载会换 generation：初始化 cohort 已采纳的 (A, g1) 不得
/// 沿用到 (A, g2)，后续成员同样取消。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn test_initializing_cohort_cancels_after_same_session_reload() {
    crate::kit::atoms::init_atoms();
    let previous_notification = std::mem::take(&mut *NOTIFICATION.state().write());
    let (client, server) = make_client_without_pump();
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();
    admission_probe::install("probe-session-a", probe_tx);

    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer = spawn_submit_consumer(client.clone(), rx, ".".into(), shutdown.clone());

    tx.send(agent_text("m0")).unwrap();
    tx.send(agent_text("m1")).unwrap();

    let (new_id, _) = expect_request(&server, "session/new").await;
    server
        .send_response(new_id, Ok(json!({"sessionId": "probe-session-a"})))
        .await
        .unwrap();
    wait_for_session(&client, "probe-session-a").await;
    let release = tokio::time::timeout(Duration::from_secs(5), probe_rx.recv())
        .await
        .expect("m0 落定身份后应停在断点")
        .expect("probe channel must stay open");

    // 同 ID 重载：session 不变但 generation 前进，旧 cohort 身份作废。
    let loader = client.clone();
    let reload =
        tokio::spawn(async move { loader.load_session("probe-session-a", ".", None).await });
    let (load_id, load_params) = expect_request(&server, "session/load").await;
    assert_eq!(load_params["sessionId"], "probe-session-a");
    server
        .send_response(load_id, Ok(json!({"sessionId": "probe-session-a"})))
        .await
        .unwrap();
    reload.await.unwrap().unwrap();

    release.send(()).unwrap();

    let stray = tokio::time::timeout(Duration::from_millis(500), server.recv()).await;
    assert!(
        stray.is_err(),
        "重载后的新 generation 不得接收旧 cohort 的消息: {stray:?}"
    );
    assert!(
        NOTIFICATION.state().read().is_some(),
        "取消排队提交必须有用户可见反馈"
    );

    admission_probe::clear();
    *NOTIFICATION.state().write() = previous_notification;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
}
/// [回归 P2] 旧会话车道仍活跃时加载 B：加载期间的用户输入属于 B 的 cohort，
/// 不得复用 A 的采纳槽被取消；落定后入队的消息继续排在它之后。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn test_input_during_session_load_keeps_new_cohort() {
    crate::kit::atoms::init_atoms();
    let previous_notification = std::mem::take(&mut *NOTIFICATION.state().write());
    let (client, server) = make_client_without_pump();
    let (tx, rx) = mpsc::unbounded_channel::<SubmitRequest>();
    let shutdown = CancellationToken::new();
    let consumer = spawn_submit_consumer(client.clone(), rx, ".".into(), shutdown.clone());

    // m0 建立 A 并挂起：A 的初始化车道仍然活跃。
    tx.send(agent_text("m0")).unwrap();
    let (new_id, _) = expect_request(&server, "session/new").await;
    server
        .send_response(new_id, Ok(json!({"sessionId": "session-a"})))
        .await
        .unwrap();
    let (_a0_id, a0_session, a0_text) = expect_prompt(&server).await;
    assert_eq!((a0_session.as_str(), a0_text.as_str()), ("session-a", "m0"));

    // 用户选择历史线程 B：预约先于异步 load 消费，覆盖调度窗口。
    let reservation = client.reserve_session_load();
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session("session-b", ".", None).await });
    let (close_id, close_params) = expect_request(&server, "session/close").await;
    assert_eq!(close_params["sessionId"], "session-a");
    server.send_response(close_id, Ok(json!({}))).await.unwrap();
    let (load_id, load_params) = expect_request(&server, "session/load").await;
    assert_eq!(load_params["sessionId"], "session-b");

    // 加载尚未完成时的用户输入：属于 B 的 cohort，不能落回 A 的车道。
    tx.send(agent_text("m1")).unwrap();

    server
        .send_response(load_id, Ok(json!({"sessionId": "session-b"})))
        .await
        .unwrap();
    load.await.unwrap().unwrap();
    drop(reservation);
    wait_for_session(&client, "session-b").await;

    // 落定后入队的消息必须排在 m1 之后（同一 cohort 车道）。
    tx.send(agent_text("m2")).unwrap();

    let (b1_id, b1_session, b1_text) = expect_prompt(&server).await;
    assert_eq!(
        (b1_session.as_str(), b1_text.as_str()),
        ("session-b", "m1"),
        "加载期间的输入必须投递到新会话"
    );
    let overlap = tokio::time::timeout(Duration::from_millis(250), server.recv()).await;
    assert!(overlap.is_err(), "m1 未完成时不得出现 m2: {overlap:?}");
    server
        .send_response(b1_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();

    let (b2_id, b2_session, b2_text) = expect_prompt(&server).await;
    assert_eq!((b2_session.as_str(), b2_text.as_str()), ("session-b", "m2"));
    server
        .send_response(b2_id, Ok(json!({"stopReason": "end_turn"})))
        .await
        .unwrap();

    *NOTIFICATION.state().write() = previous_notification;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_millis(500), consumer).await;
}
