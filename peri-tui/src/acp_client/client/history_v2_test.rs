//! H5a 客户端测试：V2 initial 只接纳一次、不回退 V1 open；未协商时旧路径不变。

use super::*;
use crate::acp_client::AcpNotification;
use crate::kit::atoms;
use peri_acp::transport::{
    AcpTransport,
    mpsc::{MpscServerTransport, mpsc_transport_pair},
    types::{AcpError, IncomingMessage, RequestId},
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
        *atoms::HISTORY_V2_TX
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
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
    assert!(
        tokio::time::timeout(Duration::from_millis(150), server.recv())
            .await
            .is_err(),
        "同会话旧窗口仍在服务端使用中，失败不得关闭会话"
    );

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
    page["beforeCursor"] = json!("before-1");
    page["afterCursor"] = json!("after-1");
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
    assert!(
        tokio::time::timeout(Duration::from_millis(150), server.recv())
            .await
            .is_err(),
        "恢复旧窗口时不得关闭仍在使用的会话"
    );

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

fn live_params(seq: u64, text: &str) -> Value {
    json!({
        "sessionId": SESSION,
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": text},
            "messageId": "m1",
        },
        "_meta": {
            "peri.delivery": {
                "streamId": "live-1",
                "deliveryId": format!("live-1:{seq}"),
                "eventSeq": seq.to_string(),
            }
        },
    })
}

async fn wait_for_pending(session_id: &str, expected: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while crate::kit::history_v2::live::pending_len(session_id) < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("live updates must be buffered before the load error runs");
}

struct FailedInitialOutcome {
    state: crate::kit::history_v2::HistoryV2State,
    texts: Vec<String>,
    pending_after: usize,
}

/// 真实客户端 + notifier：旧窗口已发布，initial 期间两条旧流更新先进入暂存，
/// 随后 load 返回错误。返回失败收敛后的窗口状态与重放出的文本。
async fn run_failed_initial_with_buffered_old_stream(
    capable_atom: bool,
    observe_local: bool,
) -> FailedInitialOutcome {
    atoms::HISTORY_V2_CAPABLE.set(capable_atom);
    let previous = crate::kit::history_v2::initial::admit_load_response(
        &initial_response_with_item(),
        SESSION,
        peri_acp_types::history_v2::DecimalU64::new(1),
        &HistoryLimits::default(),
    )
    .expect("previous window admitted");
    crate::kit::history_v2::publish_initial(previous.clone());

    // LOCAL_EVENT_TX 是可替换的进程级队列：本用例安装自己的接收端做事件级断言。
    let (local_tx, mut local_rx) = tokio::sync::mpsc::unbounded_channel();
    if observe_local {
        atoms::install_local_event_tx(local_tx);
    }

    let (transport, server) = mpsc_transport_pair();
    let (client, notification_tx, notification_rx) = AcpTuiClient::new_interactive(transport);
    install_v2(&client, true);
    let (bridge_tx, _bridge_rx) = tokio::sync::mpsc::channel(8);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let notifier = crate::kit::acp_notifier::spawn_kit_notifier(
        notification_rx,
        crate::kit::acp_notifier::BridgeSink::direct(bridge_tx),
        shutdown.clone(),
    );

    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session(SESSION, "/tmp", None).await });

    let (id, method, _) = next_request(&server).await;
    assert_eq!(method, "session/load");
    // 候选期间服务端暂停 live 并缓冲；客户端此时仍登记着 pending，通知先到。
    notification_tx
        .send(AcpNotification::SessionUpdate {
            session_id: SESSION.into(),
            params: live_params(1, "a"),
        })
        .await
        .unwrap();
    notification_tx
        .send(AcpNotification::SessionUpdate {
            session_id: SESSION.into(),
            params: live_params(2, "b"),
        })
        .await
        .unwrap();
    wait_for_pending(SESSION, 2).await;

    // initial 失败：服务端先发错误，再把候选期事件补回旧流。
    server
        .send_response(id, Err(AcpError::new(-32603, "initial failed")))
        .await
        .unwrap();
    assert!(load.await.unwrap().is_err());

    let state = crate::kit::history_v2::snapshot();
    let pending_after = crate::kit::history_v2::live::pending_len(SESSION);
    let mut texts = Vec::new();
    if observe_local {
        while let Ok(event) = local_rx.try_recv() {
            if let crate::kit::acp_types::AcpEventData::TextChunk(chunk) = event.event {
                texts.push(chunk.text);
            }
        }
    }

    notifier.abort();
    shutdown.cancel();
    client.close();
    FailedInitialOutcome {
        state,
        texts,
        pending_after,
    }
}

fn assert_failed_initial_replayed(outcome: &FailedInitialOutcome) {
    let state = &outcome.state;
    assert!(state.is_ready(), "旧窗口必须保留: {state:?}");
    assert_eq!(
        state.window().map(|window| window.window_id.as_str()),
        Some("w1")
    );
    let live = &state.window().unwrap().paging.live;
    assert_eq!(
        live.last_applied_seq,
        Some(peri_acp_types::history_v2::DecimalU64::new(2)),
        "补发更新必须按序并入旧窗口"
    );
    assert_eq!(live.gap, None, "连续补发不得被判为缺口");
    assert!(!live.detached);
    assert_eq!(outcome.pending_after, 0, "恢复后暂存必须清空");
    assert!(
        crate::kit::history_v2::live::take_pending(SESSION).is_empty(),
        "恢复后后续事件才能直发"
    );
}

/// P1：initial 失败前进入暂存的旧流补发，必须在错误分支按 seq 并入旧窗口并渲染。
///
/// 只清空暂存会让旧窗口静默缺尾部；下一次更大序号被接纳时才会暴露为 gap。
/// 生产交互路径（V2 协商成功）在 load 起点会清空 V2 状态，旧窗口快照必须早于
/// 会话边界，失败后才能恢复窗口并重放暂存。
#[tokio::test]
#[serial]
async fn failed_initial_replays_buffered_old_stream_updates_in_order() {
    let _guard = prepare();
    atoms::init_atoms();
    let outcome = run_failed_initial_with_buffered_old_stream(true, true).await;
    assert_failed_initial_replayed(&outcome);
    if !outcome.texts.is_empty() {
        assert_eq!(
            outcome.texts,
            vec!["a", "b"],
            "补发更新必须恰好一次、按序渲染"
        );
    }
}

/// 同一回归的保留窗口路径：会话边界未清空 V2 状态时（atom 未标记能力），旧窗口
/// 在失败前一直可见；暂存仍必须并入而不是被 `clear_pending` 静默丢弃。
#[tokio::test]
#[serial]
async fn failed_initial_with_retained_window_replays_pending_updates() {
    let _guard = prepare();
    atoms::init_atoms();
    let outcome = run_failed_initial_with_buffered_old_stream(false, false).await;
    assert_failed_initial_replayed(&outcome);
}

// ── 生产接线回归：真实 pump + notifier + V2 出站 FIFO ──

struct LiveHarness {
    client: AcpTuiClient,
    server: MpscServerTransport,
    bridge_rx: tokio::sync::mpsc::Receiver<crate::kit::acp_types::AcpEventWithEpoch>,
    local_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<crate::kit::acp_types::AcpEventWithEpoch>>,
    shutdown: tokio_util::sync::CancellationToken,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for LiveHarness {
    fn drop(&mut self) {
        self.shutdown.cancel();
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.client.close();
    }
}

/// 真实交互路径：transport pump 负责 `route_ordinary`，notifier 消费通知，
/// V2 出站调度器按 FIFO 发送 load/close/page/body。`HISTORY_V2_CAPABLE` 必须
/// 同时置位，否则会话边界会跳过生产 V2 清理分支，测试就只剩本地回填。
async fn live_harness() -> LiveHarness {
    atoms::init_atoms();
    atoms::HISTORY_V2_CAPABLE.set(true);
    let (transport, server) = mpsc_transport_pair();
    let (client, notification_tx, notification_rx) = AcpTuiClient::new_interactive(transport);
    install_v2(&client, true);
    client.spawn_pump(notification_tx);

    let (history_tx, history_rx) = tokio::sync::mpsc::unbounded_channel();
    *atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(history_tx);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let consumer = crate::kit::history_v2::consumer::spawn_history_v2_consumer(
        client.clone(),
        history_rx,
        shutdown.clone(),
    );

    let (bridge_tx, bridge_rx) = tokio::sync::mpsc::channel(64);
    let notifier = crate::kit::acp_notifier::spawn_kit_notifier(
        notification_rx,
        crate::kit::acp_notifier::BridgeSink::direct(bridge_tx),
        shutdown.clone(),
    );

    // LOCAL_EVENT_TX 是可替换的进程级队列：安装本用例自己的接收端，验证恢复
    // 重放的可见顺序。
    let (local_tx, local_rx) = tokio::sync::mpsc::unbounded_channel();
    atoms::install_local_event_tx(local_tx);

    LiveHarness {
        client,
        server,
        bridge_rx,
        local_rx: Some(local_rx),
        shutdown,
        tasks: vec![consumer, notifier],
    }
}

/// 生产合流 harness：notifier 与恢复重放写入同一条本地 FIFO，由唯一 forwarder
/// 串行送入有界 bridge。forwarder 在显式释放前暂停，复现「本地转发尚未运行或受
/// 背压时，新序号实时通知先到达 bridge」的真实竞态。
struct OrderedLiveHarness {
    client: AcpTuiClient,
    server: MpscServerTransport,
    bridge_rx: tokio::sync::mpsc::Receiver<crate::kit::acp_types::AcpEventWithEpoch>,
    release_forwarder: Option<tokio::sync::oneshot::Sender<()>>,
    shutdown: tokio_util::sync::CancellationToken,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for OrderedLiveHarness {
    fn drop(&mut self) {
        self.shutdown.cancel();
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.client.close();
    }
}

async fn ordered_live_harness() -> OrderedLiveHarness {
    atoms::init_atoms();
    atoms::HISTORY_V2_CAPABLE.set(true);
    let (transport, server) = mpsc_transport_pair();
    let (client, notification_tx, notification_rx) = AcpTuiClient::new_interactive(transport);
    install_v2(&client, true);
    client.spawn_pump(notification_tx);

    let (history_tx, history_rx) = tokio::sync::mpsc::unbounded_channel();
    *atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(history_tx);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let consumer = crate::kit::history_v2::consumer::spawn_history_v2_consumer(
        client.clone(),
        history_rx,
        shutdown.clone(),
    );

    // 容量压到 1：转发落后时，实时通知在入口形成背压，放大乱序窗口。
    let (bridge_tx, bridge_rx) = tokio::sync::mpsc::channel(1);
    let (local_tx, mut local_rx) = tokio::sync::mpsc::unbounded_channel();
    atoms::install_local_event_tx(local_tx.clone());
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let forwarder = tokio::spawn(async move {
        // 暂停到测试释放：模拟 forwarder 尚未被调度 / 正受 bridge 背压。
        let _ = release_rx.await;
        while let Some(event) = local_rx.recv().await {
            if bridge_tx.send(event).await.is_err() {
                break;
            }
        }
    });

    let notifier = crate::kit::acp_notifier::spawn_kit_notifier(
        notification_rx,
        crate::kit::acp_notifier::BridgeSink::ordered(local_tx),
        shutdown.clone(),
    );

    OrderedLiveHarness {
        client,
        server,
        bridge_rx,
        release_forwarder: Some(release_tx),
        shutdown,
        tasks: vec![consumer, forwarder, notifier],
    }
}

fn old_window() -> crate::kit::history_v2::HistoryV2State {
    crate::kit::history_v2::initial::admit_load_response(
        &initial_response_with_item(),
        SESSION,
        peri_acp_types::history_v2::DecimalU64::new(1),
        &HistoryLimits::default(),
    )
    .expect("previous window admitted")
}

async fn wait_for_applied_seq(session_id: &str, expected: u64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = crate::kit::history_v2::snapshot();
            let applied = state
                .window()
                .and_then(|window| window.paging.live.last_applied_seq)
                .map(peri_acp_types::history_v2::DecimalU64::get);
            if state.session_id.as_deref() == Some(session_id) && applied == Some(expected) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("state must reach the expected live sequence");
}

fn text_of(event: &crate::kit::acp_types::AcpEventWithEpoch) -> Option<String> {
    match &event.event {
        crate::kit::acp_types::AcpEventData::TextChunk(chunk) => Some(chunk.text.clone()),
        _ => None,
    }
}

async fn next_bridge_text(
    rx: &mut tokio::sync::mpsc::Receiver<crate::kit::acp_types::AcpEventWithEpoch>,
) -> String {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("notifier must deliver the live update")
            .expect("bridge channel open");
        if let Some(text) = text_of(&event) {
            return text;
        }
    }
}

fn page_with_old_identity() -> Value {
    let mut page = initial_response_with_item()["_meta"]["peri.history"]["page"].clone();
    page["beforeCursor"] = json!("before-2");
    page["items"] = json!([{
        "entryId": "e0",
        "source": {"kind": "own"},
        "contentVersion": "1",
        "durability": {"state": "sealed", "origin": "live", "finalItemSeq": "0"},
        "order": {"region": "own", "ordinal": "0"},
        "subItems": [{
            "kind": "text",
            "subItemId": "e0:0",
            "role": "assistant",
            "text": "older",
        }],
    }]);
    page
}

/// 恢复后的旧窗口身份仍然可用：page 与 body 都按原 window/ticket 上线，
/// 失败路径不得发送 Close。
async fn assert_restored_old_window_page_and_body(server: &MpscServerTransport) {
    assert!(crate::kit::history_v2::consumer::request_older_page_if_due());
    let (id, method, params) = next_request(server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("page"));
    assert_eq!(params["windowId"], json!("w1"));
    assert_eq!(params["snapshotToken"], json!("snapshot-1"));
    assert_eq!(params["readTicket"], json!("ticket-1"));
    server
        .send_response(id, Ok(page_with_old_identity()))
        .await
        .unwrap();

    let binding = crate::kit::history_detail_binding::HistoryV2BodyBinding {
        session_id: SESSION.into(),
        window_id: "w1".into(),
        snapshot_token: "snapshot-1".into(),
        read_ticket: "ticket-1".into(),
        source: peri_acp_types::history_v2::HistorySource::Own,
        entry_id: "e1".into(),
        sub_item_id: "e1:0".into(),
        body_ref: "body-1".into(),
        representation: peri_acp_types::history_v2::HistoryRepresentation::DisplayText,
        media_type: "text/plain; charset=utf-8".into(),
    };
    let body_receiver =
        match crate::kit::history_v2::consumer::submit_body(&binding, 0, 128).unwrap() {
            crate::kit::history_v2::outbound::Submit::Queued(receiver) => receiver,
            crate::kit::history_v2::outbound::Submit::Direct(_) => {
                panic!("body must go through the production FIFO")
            }
        };
    let (id, method, params) = next_request(server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("body"));
    assert_eq!(params["readTicket"], json!("ticket-1"));
    server
        .send_response(id, Ok(json!({"ok": true})))
        .await
        .unwrap();
    assert!(body_receiver.await.unwrap().is_ok());

    assert!(
        tokio::time::timeout(Duration::from_millis(150), server.recv())
            .await
            .is_err(),
        "失败路径不得关闭旧窗口"
    );
}

async fn wait_for_pending_cleared(session_id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while crate::kit::history_v2::live::is_pending(session_id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("recovery must release the pending replay slot");
}

/// 取 bridge 上按到达顺序的文本块，直到 `expected` 条；随后再等一小段确认没有
/// 多余/重复的文本块。
async fn collect_bridge_texts(
    rx: &mut tokio::sync::mpsc::Receiver<crate::kit::acp_types::AcpEventWithEpoch>,
    expected: usize,
) -> Vec<String> {
    let mut texts = Vec::new();
    while texts.len() < expected {
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("bridge delivered only {texts:?}"))
            .expect("bridge channel open");
        if let Some(text) = text_of(&event) {
            texts.push(text);
        }
    }
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
        if let Some(text) = text_of(&event) {
            texts.push(text);
        }
    }
    texts
}

/// P1：initial 失败不得提前关闭旧窗口；失败后 route/pump 必须继续投递旧流通知。
///
/// 生产 FIFO 下，边界若先发 Close，服务端票据在 initial 之前就失效；错误返回后
/// 补发的旧流更新也会因路由停在 `NoSession` 被 pump 丢弃。这里用真实 pump、
/// notifier 与出站调度器覆盖失败前、错误返回后与恢复后的 page/body。
#[tokio::test]
#[serial]
async fn failed_initial_keeps_old_ticket_and_restores_notification_route() {
    let _guard = prepare();
    let mut harness = live_harness().await;
    let previous = old_window();
    crate::kit::history_v2::publish_initial(previous.clone());

    let loader = harness.client.clone();
    let load = tokio::spawn(async move { loader.load_session(SESSION, "/tmp", None).await });
    let (id, method, _) = next_request(&harness.server).await;
    assert_eq!(method, "session/load");
    assert!(
        tokio::time::timeout(Duration::from_millis(150), harness.server.recv())
            .await
            .is_err(),
        "候选 initial 之前不得发送旧窗口 Close"
    );

    // 失败前：旧流更新进入暂存，不越过高水位直接渲染。
    harness
        .server
        .send_notification("session/update", live_params(1, "a"))
        .await
        .unwrap();
    wait_for_pending(SESSION, 1).await;

    harness
        .server
        .send_response(id, Err(AcpError::new(-32603, "initial failed")))
        .await
        .unwrap();
    assert!(load.await.unwrap().is_err());

    assert_eq!(
        harness.client.lifecycle.current_session_id().as_deref(),
        Some(SESSION),
        "失败后通知路由必须交还旧会话"
    );
    wait_for_applied_seq(SESSION, 1).await;
    assert_eq!(
        crate::kit::history_v2::live::pending_len(SESSION),
        0,
        "恢复后暂存必须清空"
    );

    // 错误返回后：真实 pump 必须放行旧流补发/实时通知，且每条只渲染一次。
    harness
        .server
        .send_notification("session/update", live_params(2, "b"))
        .await
        .unwrap();
    harness
        .server
        .send_notification("session/update", live_params(3, "c"))
        .await
        .unwrap();
    wait_for_applied_seq(SESSION, 3).await;
    let first = next_bridge_text(&mut harness.bridge_rx).await;
    let second = next_bridge_text(&mut harness.bridge_rx).await;
    assert_eq!(
        (first.as_str(), second.as_str()),
        ("b", "c"),
        "恢复后实时通知必须按 seq 顺序渲染"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(150), harness.bridge_rx.recv())
            .await
            .is_err(),
        "每条通知只能渲染一次"
    );

    // 旧 window/readTicket 仍然有效：page 与 body 都使用恢复窗口的原身份上线。
    assert_restored_old_window_page_and_body(&harness.server).await;
}

/// P1：恢复重放与实时通知必须在真实 local→bridge 合流点共享同一顺序。
///
/// notifier 与恢复重放都写入 `LOCAL_EVENT_TX`，由唯一 forwarder 串行送入有界
/// bridge；测试在释放前暂停 forwarder，复现「重放尚未到达 bridge，新序号通知已
/// 被接纳」的生产竞态。修复前实时通知直连 bridge，会把 b/c 排在 a 之前；修复后
/// 三者入同一 FIFO，最终渲染必须严格 a→b→c，且无丢失/重复。
#[tokio::test]
#[serial]
async fn recovery_replay_and_live_updates_share_one_ordered_bridge_ingress() {
    let _guard = prepare();
    let mut harness = ordered_live_harness().await;
    crate::kit::history_v2::publish_initial(old_window());

    let loader = harness.client.clone();
    let load = tokio::spawn(async move { loader.load_session(SESSION, "/tmp", None).await });
    let (id, method, _) = next_request(&harness.server).await;
    assert_eq!(method, "session/load");

    // initial 期间的旧流更新进入暂存。
    harness
        .server
        .send_notification("session/update", live_params(1, "a"))
        .await
        .unwrap();
    wait_for_pending(SESSION, 1).await;

    // initial 失败：恢复把 a 按 seq 并入旧窗口并写入本地 FIFO；forwarder 仍暂停。
    harness
        .server
        .send_response(id, Err(AcpError::new(-32603, "initial failed")))
        .await
        .unwrap();
    assert!(load.await.unwrap().is_err());
    assert_eq!(
        harness.client.lifecycle.current_session_id().as_deref(),
        Some(SESSION),
        "失败后通知路由必须交还旧会话"
    );
    wait_for_pending_cleared(SESSION).await;
    assert_eq!(crate::kit::history_v2::live::pending_len(SESSION), 0);

    // 恢复后到达的实时通知（新序号）不得越过尚未转发的重放事件。
    harness
        .server
        .send_notification("session/update", live_params(2, "b"))
        .await
        .unwrap();
    harness
        .server
        .send_notification("session/update", live_params(3, "c"))
        .await
        .unwrap();
    wait_for_applied_seq(SESSION, 3).await;

    // 释放 forwarder：bridge 上的文本必须严格旧→新，且每条恰好一次。
    harness
        .release_forwarder
        .take()
        .expect("forwarder release handle")
        .send(())
        .unwrap();
    let texts = collect_bridge_texts(&mut harness.bridge_rx, 3).await;
    assert_eq!(
        texts,
        vec!["a", "b", "c"],
        "恢复重放与实时通知必须共享同一有序入口"
    );

    // 旧 window/readTicket 仍然有效：page 与 body 都使用恢复窗口的原身份上线。
    assert_restored_old_window_page_and_body(&harness.server).await;
}
///
/// 错误响应后立刻补发的更新可能与恢复并发：它们要么在暂存锁内等待后按 seq 并入，
/// 要么在恢复完成、高水位发布后直接接纳；两种路径都必须恰好接纳一次。
#[tokio::test]
#[serial]
async fn failed_initial_accepts_events_racing_with_recovery_once() {
    let _guard = prepare();
    let mut harness = live_harness().await;
    crate::kit::history_v2::publish_initial(old_window());

    let loader = harness.client.clone();
    let load = tokio::spawn(async move { loader.load_session(SESSION, "/tmp", None).await });
    let (id, method, _) = next_request(&harness.server).await;
    assert_eq!(method, "session/load");
    harness
        .server
        .send_notification("session/update", live_params(1, "a"))
        .await
        .unwrap();
    wait_for_pending(SESSION, 1).await;

    // 先入 wire 的错误响应，再立刻补发恢复期更新；pump 与 load 任务并发推进。
    harness
        .server
        .send_response(id, Err(AcpError::new(-32603, "initial failed")))
        .await
        .unwrap();
    harness
        .server
        .send_notification("session/update", live_params(2, "b"))
        .await
        .unwrap();
    harness
        .server
        .send_notification("session/update", live_params(3, "c"))
        .await
        .unwrap();
    assert!(load.await.unwrap().is_err());

    wait_for_applied_seq(SESSION, 3).await;
    let state = crate::kit::history_v2::snapshot();
    let live = &state.window().expect("restored window").paging.live;
    assert_eq!(live.gap, None, "连续恢复不得被判为缺口");
    assert!(!live.detached, "恢复窗口不得进入 detached");
    assert_eq!(crate::kit::history_v2::live::pending_len(SESSION), 0);

    let mut local_texts = Vec::new();
    let local_rx = harness.local_rx.as_mut().expect("local queue installed");
    while let Ok(event) = local_rx.try_recv() {
        if let Some(text) = text_of(&event) {
            local_texts.push(text);
        }
    }
    let mut bridge_texts = Vec::new();
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(200), harness.bridge_rx.recv()).await
    {
        if let Some(text) = text_of(&event) {
            bridge_texts.push(text);
        }
    }
    let mut observed = local_texts.clone();
    observed.extend(bridge_texts.iter().cloned());
    let mut unique = observed.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        observed.len(),
        "恢复窗口内每条更新必须恰好接纳一次"
    );
    assert!(
        unique
            .iter()
            .all(|text| ["a", "b", "c"].contains(&text.as_str())),
        "恢复窗口只允许这三条更新: {observed:?}"
    );
    assert_eq!(unique, vec!["a", "b", "c"], "恢复重放不得丢失任何一条");
    for channel in [&local_texts[..], &bridge_texts[..]] {
        assert!(
            channel.windows(2).all(|pair| pair[0] < pair[1]),
            "同一投递通道内必须按 seq 顺序渲染: {observed:?}"
        );
    }
}

/// 候选成功路径：旧窗口的 Close 只能在 initial 接纳且发布之后入队。
#[tokio::test]
#[serial]
async fn successful_initial_closes_old_window_after_publication() {
    let _guard = prepare();
    let harness = live_harness().await;
    crate::kit::history_v2::publish_initial(old_window());

    let loader = harness.client.clone();
    let load = tokio::spawn(async move { loader.load_session(SESSION, "/tmp", None).await });
    let (id, method, params) = next_request(&harness.server).await;
    assert_eq!(method, "session/load");
    let load_key = params["_meta"]["peri.history"]["requestKey"].clone();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), harness.server.recv())
            .await
            .is_err(),
        "候选发布前不得发送 Close"
    );

    let mut response = initial_response(true);
    response["_meta"]["peri.history"]["requestKey"] = load_key;
    let page = &mut response["_meta"]["peri.history"]["page"];
    page["windowId"] = json!("w2");
    page["snapshotToken"] = json!("snapshot-2");
    page["readTicket"] = json!("ticket-2");
    harness
        .server
        .send_response(id, Ok(response))
        .await
        .unwrap();
    assert_eq!(load.await.unwrap().unwrap(), SESSION);
    assert_eq!(
        crate::kit::history_v2::snapshot()
            .window()
            .map(|window| window.window_id.as_str()),
        Some("w2")
    );

    let (id, method, params) = next_request(&harness.server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("close"));
    assert_eq!(params["windowId"], json!("w1"));
    harness
        .server
        .send_response(id, Ok(json!({})))
        .await
        .unwrap();
}
