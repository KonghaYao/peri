use super::*;
use crate::acp_client::interaction_lifecycle::TransitionKind;
use crate::kit::atoms;
use peri_acp::transport::{
    mpsc::{MpscServerTransport, mpsc_transport_pair},
    types::{IncomingMessage, RequestId},
};
use serial_test::serial;
use std::sync::atomic::Ordering;
use std::time::Duration;

const WINDOW_SESSION: &str = "window-target";
const WINDOW_SENTINEL_SESSION: &str = "window-sentinel";
const WINDOW_SENTINEL_SEQ: u64 = 41;

/// 窗口开关与 consumer 状态是进程全局的：用例串行执行并在结束时恢复默认值。
struct WindowAtomsGuard;

impl Drop for WindowAtomsGuard {
    fn drop(&mut self) {
        atoms::HISTORY_WINDOW_V1.set(false);
        atoms::HISTORY_WINDOW_CAPABLE.set(false);
        crate::kit::history_page_consumer::reset_window_paging_state();
    }
}

/// 打开窗口路径并放一个哨兵：enqueue 会把状态重置成目标会话，未 enqueue 则保持哨兵。
fn enable_window_path() -> WindowAtomsGuard {
    atoms::HISTORY_WINDOW_V1.set(true);
    atoms::HISTORY_WINDOW_CAPABLE.set(true);
    crate::kit::history_page_consumer::reset_window_paging_state();
    {
        let handle = atoms::HISTORY_WINDOW_PAGING.state();
        let mut state = handle.write();
        state.session_id = WINDOW_SENTINEL_SESSION.to_owned();
        state.next_request_seq = WINDOW_SENTINEL_SEQ;
    }
    WindowAtomsGuard
}

fn window_state() -> (String, u64) {
    let handle = atoms::HISTORY_WINDOW_PAGING.state();
    let state = handle.read();
    (state.session_id.clone(), state.next_request_seq)
}

fn snapshot_payload() -> Value {
    json!({
        "sessionId": WINDOW_SESSION,
        "generation": "g",
        "revision": 1,
        "items": [],
    })
}

async fn next_request(server: &MpscServerTransport) -> (RequestId, String, Value) {
    let message = tokio::time::timeout(Duration::from_secs(5), server.recv())
        .await
        .expect("expected a client request")
        .expect("transport must stay open");
    match message {
        IncomingMessage::Request { id, method, params } => (id, method, params),
        other => panic!("expected request, got {other:?}"),
    }
}

/// 初始化成功之后才 enqueue `history.open`：快照请求发出时尚未入队，成功后才入队一次。
#[tokio::test]
#[serial]
async fn window_open_is_enqueued_only_after_session_initialization_succeeds() {
    let _guard = enable_window_path();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.user_input_queue.store(true, Ordering::Release);
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(WINDOW_SESSION, "/tmp", None).await });

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/load");
    server.send_response(id, Ok(json!({}))).await.unwrap();

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/input/snapshot");
    assert_eq!(
        window_state(),
        (WINDOW_SENTINEL_SESSION.to_owned(), WINDOW_SENTINEL_SEQ),
        "用户输入快照完成前不得 enqueue 窗口 open"
    );
    server
        .send_response(id, Ok(snapshot_payload()))
        .await
        .unwrap();

    assert_eq!(load.await.unwrap().unwrap(), WINDOW_SESSION);
    assert_eq!(
        window_state(),
        (WINDOW_SESSION.to_owned(), 1),
        "初始化成功后恰好 enqueue 一次 open（重置状态并推进请求序号）"
    );
    client.close();
}

/// 初始化失败不得 enqueue：哨兵状态保持不变（Headless 不触发 session boundary 重置）。
#[tokio::test]
#[serial]
async fn failed_session_initialization_does_not_enqueue_window_open() {
    let _guard = enable_window_path();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.user_input_queue.store(true, Ordering::Release);
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(WINDOW_SESSION, "/tmp", None).await });

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/load");
    server.send_response(id, Ok(json!({}))).await.unwrap();

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/input/snapshot");
    server
        .send_response(id, Err(AcpError::new(-32603, "snapshot failed")))
        .await
        .unwrap();

    // 初始化失败分支会关闭本会话；响应它，load 才能返回错误。
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/close");
    server.send_response(id, Ok(json!({}))).await.unwrap();

    let error = load
        .await
        .unwrap()
        .expect_err("initialization failure must fail the load");
    assert_eq!(error.code, -32603);
    assert_eq!(
        window_state(),
        (WINDOW_SENTINEL_SESSION.to_owned(), WINDOW_SENTINEL_SEQ),
        "初始化失败不得 enqueue 窗口 open"
    );
    client.close();
}

/// 新建会话与恢复会话统一：初始化成功后恰好 enqueue 一次 `history.open`。
#[tokio::test]
#[serial]
async fn new_session_enqueues_window_open_after_initialization_succeeds() {
    let _guard = enable_window_path();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.user_input_queue.store(true, Ordering::Release);
    let creator = client.clone();
    let create = tokio::spawn(async move { creator.new_session("/tmp", None).await });

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/new");
    server
        .send_response(id, Ok(json!({ "sessionId": WINDOW_SESSION })))
        .await
        .unwrap();

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/input/snapshot");
    assert_eq!(
        window_state(),
        (WINDOW_SENTINEL_SESSION.to_owned(), WINDOW_SENTINEL_SEQ),
        "用户输入快照完成前不得 enqueue 窗口 open"
    );
    server
        .send_response(id, Ok(snapshot_payload()))
        .await
        .unwrap();

    assert_eq!(create.await.unwrap().unwrap(), WINDOW_SESSION);
    assert_eq!(
        window_state(),
        (WINDOW_SESSION.to_owned(), 1),
        "新建会话初始化成功后恰好 enqueue 一次 open"
    );
    client.close();
}

/// 新建会话初始化失败同样不得 enqueue，保持哨兵状态。
#[tokio::test]
#[serial]
async fn failed_new_session_does_not_enqueue_window_open() {
    let _guard = enable_window_path();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.user_input_queue.store(true, Ordering::Release);
    let creator = client.clone();
    let create = tokio::spawn(async move { creator.new_session("/tmp", None).await });

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/new");
    server
        .send_response(id, Ok(json!({ "sessionId": WINDOW_SESSION })))
        .await
        .unwrap();

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/input/snapshot");
    server
        .send_response(id, Err(AcpError::new(-32603, "snapshot failed")))
        .await
        .unwrap();

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/close");
    server.send_response(id, Ok(json!({}))).await.unwrap();

    let error = create
        .await
        .unwrap()
        .expect_err("initialization failure must fail the new session");
    assert_eq!(error.code, -32603);
    assert_eq!(
        window_state(),
        (WINDOW_SENTINEL_SESSION.to_owned(), WINDOW_SENTINEL_SEQ),
        "新建会话初始化失败不得 enqueue 窗口 open"
    );
    client.close();
}

#[tokio::test]
async fn session_request_does_not_hold_operation_gate_while_waiting_for_wire_response() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let identity = client.stable_session_identity().unwrap();
    let requester = client.clone();
    let task = tokio::spawn(async move {
        requester
            .send_session_request(&identity, "cron/list", json!({"sessionId":"session-a"}))
            .await
    });
    let Some(IncomingMessage::Request { id, .. }) = server.recv().await else {
        panic!("request")
    };
    let gate = client.lifecycle.operation_gate();
    let operation = tokio::time::timeout(std::time::Duration::from_millis(100), gate.lock())
        .await
        .expect("network wait cannot own operation gate");
    drop(operation);
    server
        .send_response(id, Ok(json!({"jobs":[]})))
        .await
        .unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn reloading_same_session_retires_previous_request_identity() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.force_stable_for_test("session-a", false);
    let old = client.stable_session_identity().unwrap();
    let transition = client
        .lifecycle
        .begin_transition(TransitionKind::Load, Some("session-a".into()))
        .unwrap();
    client
        .lifecycle
        .commit_stable(transition.generation, "session-a".into());
    assert_ne!(client.stable_session_identity().unwrap(), old);
    assert!(
        client
            .send_session_request(
                &old,
                "cron/remove",
                json!({"sessionId":"session-a","id":"job"})
            )
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), server.recv())
            .await
            .is_err()
    );
}
