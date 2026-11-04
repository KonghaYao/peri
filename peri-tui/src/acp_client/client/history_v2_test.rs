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
        atoms::HISTORY_V2_CAPABLE.set(false);
        atoms::HISTORY_V2_LIMITS.set(HistoryLimits::default());
        crate::kit::history_v2::reset_state();
    }
}

fn prepare() -> HistoryAtomsGuard {
    crate::kit::history_v2::reset_state();
    crate::kit::history_v2::consumer::reset_request_key_for_test();
    HistoryAtomsGuard
}

/// V2 状态由连接协商事实决定；不触碰进程级 atom，避免并行用例串线。
fn install_v2(client: &AcpTuiClient, capable: bool) {
    client.install_history_v2_for_test(capable, HistoryLimits::default());
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
async fn new_session_without_active_v2_does_not_open_history() {
    let _guard = prepare();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    // 未协商：请求过 V2，但服务端没有选择。
    install_v2(&client, false);
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
        "未协商 V2 时不得发送 _peri/history open"
    );
    client.close();
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
    assert!(!client.lifecycle.has_session(), "失败不得提交 Stable");
    client.close();
}

#[tokio::test]
#[serial]
async fn failed_initial_keeps_the_previous_window_and_delivery_identity() {
    let _guard = prepare();
    // 已有可用窗口：initial 是候选，失败不得覆盖已发布事实。
    let previous = crate::kit::history_v2::initial::admit_load_response(
        &initial_response(true),
        SESSION,
        peri_acp_types::history_v2::DecimalU64::new(1),
        &HistoryLimits::default(),
    )
    .expect("previous window admitted");
    crate::kit::history_v2::publish_initial(previous.clone());

    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    install_v2(&client, true);
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(SESSION, "/tmp", None).await });

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/load");
    respond_ok(&server, id).await;
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/close", "候选会话仍需回收");
    respond_ok(&server, id).await;

    assert!(load.await.unwrap().is_err());
    let state = crate::kit::history_v2::snapshot();
    assert_eq!(
        state, previous,
        "initial 失败不得清空旧 HISTORY_V2_STATE 或改变 live/delivery 身份"
    );
    assert!(state.is_ready());
    assert_eq!(state.delivery_id.as_deref(), Some("delivery-1"));
    assert_eq!(
        state.window().map(|window| window.window_id.as_str()),
        Some("w1")
    );
    client.close();
}

fn initial_response_with_item() -> Value {
    let mut response = initial_response(true);
    let page = &mut response["_meta"]["peri.history"]["page"];
    page["items"] = json!([{
        "entryId": "e1",
        "source": {"kind": "own"},
        "contentVersion": "1",
        "durability": {"state": "sealed", "origin": "live", "finalItemSeq": "1"},
        "order": {"region": "own", "ordinal": "1"},
        "subItems": [{
            "kind": "text",
            "subItemId": "e1:0",
            "role": "assistant",
            "text": "old view",
        }],
    }]);
    page["coverage"] = json!({
        "kind": "interval",
        "from": {"kind": "head"},
        "to": {"kind": "tail"},
    });
    response
}

#[tokio::test]
#[serial]
async fn failed_initial_reprojects_the_previous_same_session_view() {
    let _guard = prepare();
    atoms::init_atoms();
    crate::kit::history_v2::reset_state();
    let previous = crate::kit::history_v2::initial::admit_load_response(
        &initial_response_with_item(),
        SESSION,
        peri_acp_types::history_v2::DecimalU64::new(1),
        &HistoryLimits::default(),
    )
    .expect("previous window admitted");
    crate::kit::history_v2::publish_initial(previous.clone());
    let notifications_before = crate::kit::history_v2::consumer::notify_count();

    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new_interactive(transport);
    install_v2(&client, true);
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(SESSION, "/tmp", None).await });

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/load");
    respond_ok(&server, id).await;
    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/close");
    respond_ok(&server, id).await;

    assert!(load.await.unwrap().is_err());
    assert_eq!(
        atoms::ACTIVE_SESSION_ID.state().read().as_str(),
        SESSION,
        "同会话失败必须保留 load 起点已投影的会话边界"
    );
    assert_eq!(
        crate::kit::history_v2::snapshot(),
        previous,
        "失败路径不得清空旧窗口"
    );
    assert!(
        crate::kit::history_v2::consumer::notify_count() > notifications_before,
        "失败路径必须主动重投影旧视图"
    );

    // 桥接任务在 reset 之后会接收重投影事件；直接走同一投影入口验证可见结果。
    crate::kit::acp_events::push_view_models_for_reset();
    let mut bridge = crate::kit::acp_events::acp_events_test::make_fold_test_state();
    bridge.active_session_id = SESSION.into();
    crate::kit::acp_events::dispatch_and_notify(
        &mut bridge,
        &crate::kit::acp_events::history_v2::view_event(previous.clone()),
    );
    assert!(!bridge.committed.is_empty(), "旧条目必须回到 committed");
    assert!(
        crate::kit::atoms::HISTORY_V2_PREFIX_LEN.get() > 0,
        "旧视图前缀必须保留"
    );
    assert!(
        !crate::kit::atoms::VIEW_MODELS
            .state()
            .read()
            .items
            .is_empty(),
        "VIEW_MODELS 必须重新包含旧条目"
    );
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
async fn new_session_on_v2_connection_completes_without_legacy_window() {
    let _guard = prepare();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    install_v2(&client, true);
    let creator = client.clone();
    let new_session = tokio::spawn(async move { creator.new_session("/tmp", None).await });

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/new");
    server
        .send_response(id, Ok(json!({"sessionId": SESSION})))
        .await
        .unwrap();

    assert_eq!(new_session.await.unwrap().unwrap(), SESSION);
    client.close();
}
