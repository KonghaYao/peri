//! H5a 客户端测试：V2 initial 只接纳一次、不回退 V1 open；未协商时旧路径不变。

use super::*;
use crate::kit::atoms;
use peri_acp::transport::{
    mpsc::{MpscServerTransport, mpsc_transport_pair},
    types::{IncomingMessage, RequestId},
};
use peri_acp_types::history_v2::{HISTORY_V2_VERSION, HistoryLimits};
use serde_json::{Value, json};
use serial_test::serial;
use std::time::Duration;

const SESSION: &str = "v2-target";

/// V2 开关、协商事实与 V1 consumer 状态都是进程全局：串行执行并在结束时复位。
struct HistoryAtomsGuard;

impl Drop for HistoryAtomsGuard {
    fn drop(&mut self) {
        atoms::HISTORY_V2.set(false);
        atoms::HISTORY_V2_CAPABLE.set(false);
        atoms::HISTORY_V2_LIMITS.set(HistoryLimits::default());
        atoms::HISTORY_WINDOW_V1.set(false);
        atoms::HISTORY_WINDOW_CAPABLE.set(false);
        crate::kit::history_v2::reset_state();
        crate::kit::history_page_consumer::reset_window_paging_state();
    }
}

fn prepare() -> HistoryAtomsGuard {
    crate::kit::history_v2::reset_state();
    crate::kit::history_v2::consumer::reset_request_key_for_test();
    crate::kit::history_page_consumer::reset_window_paging_state();
    HistoryAtomsGuard
}

/// 打开 V1 窗口路径，让「V2 分支误调 enqueue」成为可观察事实。
///
/// 必须在发送 load/new 响应前调用：V2 连接自身不读这些 atom，但并行的非串行
/// 用例会因它们为 true 而 enqueue 各自会话。断言只认本用例唯一的 `SESSION`，
/// 不依赖全局 paging 快照——否则任一并行用例的合法 enqueue 都会造成假失败。
fn enable_v1_window_path() {
    atoms::HISTORY_WINDOW_V1.set(true);
    atoms::HISTORY_WINDOW_CAPABLE.set(true);
    crate::kit::history_page_consumer::reset_window_paging_state();
}

/// 当前 paging 身份；空串表示没有 enqueue 过 open。
fn v1_open_target() -> Option<String> {
    let state = atoms::HISTORY_WINDOW_PAGING.state();
    let session_id = state.read().session_id.clone();
    (!session_id.is_empty()).then_some(session_id)
}

/// V2 开关按连接安装；不触碰进程级 beta atom，避免并行用例串线。
fn install_v2(client: &AcpTuiClient, capable: bool) {
    client.install_history_v2_for_test(true, capable, HistoryLimits::default());
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

fn initial_response(execution_ready: bool) -> Value {
    json!({
        "_meta": {
            "peri.history": {
                "version": HISTORY_V2_VERSION,
                "kind": "initial",
                "requestKey": "1",
                "executionReady": execution_ready,
                "runtimeGeneration": "runtime-1",
                "deliveryId": "delivery-1",
                "liveBoundary": {"streamId": "live-1", "afterSeq": "0"},
                "page": {
                    "version": HISTORY_V2_VERSION,
                    "kind": "window",
                    "sessionId": SESSION,
                    "windowId": "w1",
                    "snapshotToken": "snapshot-1",
                    "historyRevision": "3",
                    "committedTail": null,
                    "projectionVersion": 1,
                    "readTicket": "ticket-1",
                    "beforeCursor": null,
                    "afterCursor": null,
                    "items": [],
                    "coverage": {"kind": "emptySnapshot"},
                    "progress": {
                        "reason": "complete",
                        "scannedEntries": 0,
                        "consumedEntries": 0,
                    },
                },
            }
        }
    })
}

async fn respond_ok(server: &MpscServerTransport, id: RequestId) {
    server.send_response(id, Ok(json!({}))).await.unwrap();
}

#[tokio::test]
#[serial]
async fn new_session_without_active_v2_does_not_open_history_window() {
    for capable in [false, true] {
        let _guard = prepare();
        let (transport, server) = mpsc_transport_pair();
        let (client, _, _) = AcpTuiClient::new(transport);
        if capable {
            // 未协商：请求过 V2，但服务端没有选择。
            install_v2(&client, false);
        }
        let creator = client.clone();
        let task = tokio::spawn(async move { creator.new_session("/tmp", None).await });

        let (id, method, _) = next_request(&server).await;
        assert_eq!(method, "session/new");
        server
            .send_response(id, Ok(json!({"sessionId": SESSION})))
            .await
            .unwrap();
        assert_eq!(task.await.unwrap().unwrap(), SESSION);

        let unexpected = tokio::time::timeout(Duration::from_millis(100), server.recv()).await;
        assert!(
            unexpected.is_err(),
            "beta 关闭或未协商时不得发送 _peri/history open"
        );
        client.close();
    }
}

#[tokio::test]
#[serial]
async fn load_initial_establishes_v2_state_without_v1_open() {
    let _guard = prepare();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    install_v2(&client, true);
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(SESSION, "/tmp", None).await });

    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/load");
    assert_eq!(params["sessionId"], SESSION);
    let initial = &params["_meta"]["peri.history"];
    assert_eq!(initial["version"], json!(HISTORY_V2_VERSION));
    assert_eq!(initial["action"], json!("initial"));
    assert_eq!(initial["requestKey"], json!("1"));
    assert_eq!(
        initial["limit"],
        json!(HistoryLimits::default().default_items)
    );
    for legacy in ["cursor", "history", "loadGeneration"] {
        assert!(
            params.get(legacy).is_none(),
            "V2 load 不得携带旧字段 {legacy}"
        );
    }
    assert!(
        params["_meta"].get("peri.historyWindowV1").is_none(),
        "V2 initial 请求不得混入 V1 窗口字段"
    );

    enable_v1_window_path();
    server
        .send_response(id, Ok(initial_response(true)))
        .await
        .unwrap();
    assert_eq!(load.await.unwrap().unwrap(), SESSION);

    let state = crate::kit::history_v2::snapshot();
    assert!(state.is_ready());
    assert!(state.is_executable());
    assert_eq!(state.session_id.as_deref(), Some(SESSION));
    let window = state.window().expect("initial window admitted");
    assert_eq!(window.window_id, "w1");
    assert_eq!(window.snapshot_token, "snapshot-1");
    assert_eq!(window.read_ticket, "ticket-1");
    assert_eq!(window.before_cursor, None);
    assert_eq!(window.after_cursor, None);
    assert_ne!(
        v1_open_target().as_deref(),
        Some(SESSION),
        "V2 首屏不得再 enqueue V1 window open"
    );
    client.close();
}

#[tokio::test]
#[serial]
async fn execution_not_ready_keeps_history_ready_and_blocks_execution() {
    let _guard = prepare();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    install_v2(&client, true);
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(SESSION, "/tmp", None).await });

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/load");
    server
        .send_response(id, Ok(initial_response(false)))
        .await
        .unwrap();
    assert_eq!(load.await.unwrap().unwrap(), SESSION);

    let state = crate::kit::history_v2::snapshot();
    assert!(state.is_ready(), "历史阅读保持可用");
    assert!(!state.is_executable(), "执行未就绪");
    assert!(
        client.restore_error.lock().unwrap().is_some(),
        "执行未就绪必须阻断 prompt 准入"
    );
    client.close();
}

#[tokio::test]
#[serial]
async fn missing_initial_fails_explicitly_and_publishes_failure() {
    let _guard = prepare();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    install_v2(&client, true);
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(SESSION, "/tmp", None).await });

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/load");
    enable_v1_window_path();
    respond_ok(&server, id).await;

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/close", "失败后必须回收已装配会话");
    respond_ok(&server, id).await;

    let error = load.await.unwrap().unwrap_err();
    assert_eq!(error.code, -32603);
    assert!(error.to_string().contains("history_initial_required"));
    let state = crate::kit::history_v2::snapshot();
    assert!(!state.is_ready());
    assert!(!state.is_executable());
    assert_eq!(
        state.failure.as_ref().map(|failure| failure.reason),
        Some(crate::kit::history_v2::initial::InitialRejection::Missing)
    );
    assert_ne!(
        v1_open_target().as_deref(),
        Some(SESSION),
        "失败不得触发 V1 open"
    );
    assert!(!client.lifecycle.has_session(), "失败不得提交 Stable");
    client.close();
}

#[tokio::test]
#[serial]
async fn mixed_v1_fields_in_initial_fail_without_execution() {
    let _guard = prepare();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    install_v2(&client, true);
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(SESSION, "/tmp", None).await });

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/load");
    let mut response = initial_response(true);
    response["nextCursor"] = json!("v1-cursor");
    server.send_response(id, Ok(response)).await.unwrap();

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/close");
    respond_ok(&server, id).await;

    let error = load.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("history_initial_mixed_fields"));
    let state = crate::kit::history_v2::snapshot();
    assert!(!state.is_ready());
    assert!(!state.is_executable());
    assert!(!client.lifecycle.has_session());
    client.close();
}

#[tokio::test]
#[serial]
async fn v2_requested_but_not_selected_keeps_legacy_path() {
    let _guard = prepare();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    install_v2(&client, false);
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(SESSION, "/tmp", None).await });

    let (id, method, params) = next_request(&server).await;
    assert_eq!(method, "session/load");
    assert!(
        params["_meta"].get("peri.history").is_none(),
        "未协商 V2 时不得发送 initial"
    );
    enable_v1_window_path();
    respond_ok(&server, id).await;

    assert_eq!(load.await.unwrap().unwrap(), SESSION);
    assert!(
        !crate::kit::history_v2::snapshot().is_ready(),
        "未协商 V2 不得建立 V2 状态"
    );
    assert_eq!(
        v1_open_target().as_deref(),
        Some(SESSION),
        "未协商 V2 时旧 V1 路径照常 enqueue open"
    );
    client.close();
}

#[tokio::test]
#[serial]
async fn new_session_skips_v1_open_on_v2_connection() {
    let _guard = prepare();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    install_v2(&client, true);
    let creator = client.clone();
    let new_session = tokio::spawn(async move { creator.new_session("/tmp", None).await });

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/new");
    enable_v1_window_path();
    server
        .send_response(id, Ok(json!({"sessionId": SESSION})))
        .await
        .unwrap();

    assert_eq!(new_session.await.unwrap().unwrap(), SESSION);
    assert_ne!(
        v1_open_target().as_deref(),
        Some(SESSION),
        "V2 连接不得发送 V1 open"
    );
    client.close();
}
