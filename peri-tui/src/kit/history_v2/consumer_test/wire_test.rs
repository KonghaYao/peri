//! 真实出站顺序：记录型 mock client 与异步顺序/超时用例。

use peri_acp::transport::AcpTransport;

use super::*;

fn wire_key(value: &Value) -> u64 {
    value
        .as_str()
        .expect("DecimalU64 must serialize as a string")
        .parse()
        .expect("canonical decimal key")
}

fn initial_response_for(
    session: &str,
    window: &str,
    before_cursor: &str,
    request_key: &str,
) -> Value {
    let mut value = initial_value();
    value["_meta"]["peri.history"]["requestKey"] = json!(request_key);
    let page = &mut value["_meta"]["peri.history"]["page"];
    page["sessionId"] = json!(session);
    page["windowId"] = json!(window);
    page["snapshotToken"] = json!(format!("snapshot-{window}"));
    page["readTicket"] = json!(format!("ticket-{window}"));
    page["beforeCursor"] = json!(before_cursor);
    value
}

fn page_response_for(session: &str, window: &str, before_cursor: &str, items: Value) -> Value {
    let mut value = page_json(Some(before_cursor), items);
    value["sessionId"] = json!(session);
    value["windowId"] = json!(window);
    value["snapshotToken"] = json!(format!("snapshot-{window}"));
    value["readTicket"] = json!(format!("ticket-{window}"));
    value
}

async fn next_wire_request(
    server: &peri_acp::transport::mpsc::MpscServerTransport,
) -> (peri_acp::transport::types::RequestId, String, Value) {
    use peri_acp::transport::types::IncomingMessage;
    let message = tokio::time::timeout(std::time::Duration::from_secs(5), server.recv())
        .await
        .expect("expected a wire request")
        .expect("transport must stay open");
    match message {
        IncomingMessage::Request { id, method, params } => (id, method, params),
        other => panic!("expected request, got {other:?}"),
    }
}

async fn respond_wire(
    server: &peri_acp::transport::mpsc::MpscServerTransport,
    id: peri_acp::transport::types::RequestId,
    result: Value,
) {
    server.send_response(id, Ok(result)).await.unwrap();
}

/// 所有 V2 RPC 的线上 key 必须严格递增，且入队顺序 = 发送顺序。
///
/// 顺序：initial → Close(旧窗口) → page → body → open → Close(被替换窗口) → seek。
/// body 若绕过队列，会在 page 响应前到达服务端；这里用超时断言它没有。
#[tokio::test]
#[serial]
async fn wire_requests_follow_enqueue_order_across_all_v2_rpcs() {
    let _guard = atomics_guard();
    let limits = HistoryLimits::default();
    reset_request_key_for_test();

    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = crate::acp_client::client::AcpTuiClient::new(transport);
    client.install_history_v2_for_test(true, true, limits.clone());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tx);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let consumer = spawn_history_v2_consumer(client.clone(), rx, shutdown.clone());

    // 已发布的旧窗口：initial 成功前必须保持可用，成功后才会 Close。
    publish(&admitted());

    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session("s1", "/tmp", None).await });

    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, "session/load");
    let initial_key = wire_key(&params["_meta"]["peri.history"]["requestKey"]);
    assert_eq!(initial_key, 1, "initial 是第一个操作");
    respond_wire(
        &server,
        id,
        initial_response_for("s1", "w2", "before-1", "1"),
    )
    .await;
    load.await.unwrap().unwrap();
    let loaded_state = crate::kit::history_v2::snapshot();
    assert!(loaded_state.is_ready());
    assert_eq!(
        loaded_state
            .window()
            .map(|window| window.window_id.as_str()),
        Some("w2"),
        "initial 成功后替换为候选窗口"
    );
    assert_eq!(loaded_state.delivery_id.as_deref(), Some("delivery-51"));

    // initial 接纳成功后才关闭旧窗口，且 key 晚于 initial。
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("close"));
    let close_old_key = wire_key(&params["requestKey"]);
    assert!(close_old_key > initial_key);
    assert_eq!(params["windowId"], json!("w1"));
    respond_wire(&server, id, json!({})).await;

    // page 先入队，body 紧随其后；body 在 page 响应前不得上线。
    assert!(request_older_page_if_due());
    let state = crate::kit::history_v2::snapshot();
    let window = state.window().expect("initial window");
    let binding = crate::kit::history_window::HistoryV2BodyBinding {
        session_id: "s1".into(),
        window_id: window.window_id.clone(),
        snapshot_token: window.snapshot_token.clone(),
        read_ticket: window.read_ticket.clone(),
        source: peri_acp_types::history_v2::HistorySource::Own,
        entry_id: "s1:e2".into(),
        sub_item_id: "s1:e2:0".into(),
        body_ref: "body-1".into(),
        representation: peri_acp_types::history_v2::HistoryRepresentation::DisplayText,
        media_type: "text/plain; charset=utf-8".into(),
    };
    let body_receiver = match submit_body(&binding, 0, 128).expect("scheduler installed") {
        crate::kit::history_v2::outbound::Submit::Queued(receiver) => receiver,
        crate::kit::history_v2::outbound::Submit::Direct(_) => {
            panic!("body must be queued, not sent directly")
        }
    };
    assert!(request_open_latest("s1"));

    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("page"));
    let page_key = wire_key(&params["requestKey"]);
    assert!(page_key > close_old_key);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), server.recv())
            .await
            .is_err(),
        "body/open 不得在 page 响应前越过队列"
    );
    respond_wire(
        &server,
        id,
        page_response_for("s1", "w2", "before-2", json!([item("e0", 1, "older")])),
    )
    .await;

    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("body"));
    let body_key = wire_key(&params["requestKey"]);
    assert!(body_key > page_key, "body 必须排在 page 之后");
    respond_wire(&server, id, json!({})).await;
    assert!(body_receiver.await.unwrap().is_ok());

    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("open"));
    let open_key = wire_key(&params["requestKey"]);
    assert!(open_key > body_key);
    respond_wire(
        &server,
        id,
        page_response_for("s1", "w3", "before-3", json!([item("e9", 9, "open")])),
    )
    .await;

    // Open 接纳成功后才为被替换窗口入队 Close。
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("close"));
    let close_replaced_key = wire_key(&params["requestKey"]);
    assert!(close_replaced_key > open_key, "Close key 必须晚于 Open key");
    assert_eq!(params["windowId"], json!("w2"));
    respond_wire(&server, id, json!({})).await;

    let anchor = HistoryEntryAnchor {
        entry_id: "s1:e9".into(),
        source: peri_acp_types::history_v2::HistorySource::Own,
    };
    assert!(request_seek_if_due(anchor));
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("seek"));
    let seek_key = wire_key(&params["requestKey"]);
    assert!(seek_key > close_replaced_key);
    respond_wire(
        &server,
        id,
        page_response_for("s1", "w3", "before-3", json!([item("e9", 9, "open")])),
    )
    .await;

    shutdown.cancel();
    consumer.abort();
    client.close();
}

/// reset/会话切换后，旧 session 的 Open 成功响应不得在空状态上重建窗口。
#[tokio::test]
#[serial]
async fn late_open_success_after_reset_is_discarded() {
    let _guard = atomics_guard();
    reset_request_key_for_test();

    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = crate::acp_client::client::AcpTuiClient::new(transport);
    client.install_history_v2_for_test(true, true, HistoryLimits::default());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tx);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let consumer = spawn_history_v2_consumer(client.clone(), rx, shutdown.clone());

    publish(&admitted());
    assert!(request_reopen(true));
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("open"));
    assert_eq!(params["sessionId"], json!("s1"));

    // 会话边界：清空窗口并作废在途认领。
    crate::kit::history_v2::reset_state();
    reset_consumer_state();

    // 迟到的成功响应携带旧窗口页面；它没有仍然有效的 claim，必须被丢弃。
    respond_wire(
        &server,
        id,
        page_response_for("s1", "w1", "before-1", json!([item("e2", 5, "late")])),
    )
    .await;

    // 用新的 s2 Open 作为“旧响应已处理”的屏障。
    assert!(request_open_latest("s2"));
    let (_, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("open"));
    assert_eq!(params["sessionId"], json!("s2"));
    assert_eq!(
        crate::kit::history_v2::snapshot(),
        HistoryV2State::default(),
        "旧会话的 Open 成功响应不得复活窗口"
    );

    shutdown.cancel();
    consumer.abort();
    client.close();
}

/// 调度器已关闭时，page/seek 已登记的在途与预留必须回滚。
#[test]
#[serial]
fn page_and_seek_send_failure_roll_back_in_flight_state() {
    let _guard = atomics_guard();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    drop(rx);
    *atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tx);
    publish(&admitted());

    assert!(!request_older_page_if_due(), "队列关闭时不得报告已入队");
    {
        let state = crate::kit::history_v2::snapshot();
        let paging = &state.window().expect("window").paging;
        assert!(paging.in_flight.is_none(), "page 在途必须回滚");
        assert_eq!(paging.reserved_bytes, 0, "page 预留必须回滚");
    }

    assert!(!request_seek_if_due(HistoryEntryAnchor {
        entry_id: "s1:e2".into(),
        source: peri_acp_types::history_v2::HistorySource::Own,
    }));
    {
        let state = crate::kit::history_v2::snapshot();
        let paging = &state.window().expect("window").paging;
        assert!(paging.in_flight.is_none(), "seek 在途必须回滚");
        assert_eq!(paging.reserved_bytes, 0, "seek 预留必须回滚");
    }
}

/// 窗口响应阻塞消费队列时，body 与 `session/load` 都不得越过已入队请求。
///
/// 顺序由「requestKey 分配 + 入队」同一临界区保证；这个用例覆盖生产调用点
/// （load 走 `submit_load`，详情走 `submit_body`），而不仅是队列辅助函数。
#[tokio::test]
#[serial]
async fn blocked_queue_keeps_body_and_load_in_enqueue_order() {
    let _guard = atomics_guard();
    reset_request_key_for_test();
    let limits = HistoryLimits::default();
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = crate::acp_client::client::AcpTuiClient::new(transport);
    client.install_history_v2_for_test(true, true, limits.clone());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tx);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let consumer = spawn_history_v2_consumer(client.clone(), rx, shutdown.clone());

    // K：占住消费者的窗口请求；响应返回前 FIFO 不再发送后续命令。
    let blocking = match crate::kit::history_v2::outbound::enqueue_window(|request_key| {
        Some(HistoryV2ConsumerRequest::Close {
            session_id: "s1".into(),
            request_key,
            window_id: "w0".into(),
            snapshot_token: "snap-0".into(),
            read_ticket: "ticket-0".into(),
        })
    }) {
        crate::kit::history_v2::outbound::WindowEnqueue::Sent(key) => key,
        other => panic!("blocking request must enqueue: {other:?}"),
    };
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("close"));
    let blocking_key = wire_key(&params["requestKey"]);
    assert_eq!(blocking_key, blocking.get());

    // K+1：V2 load；K+2：详情 body。两者都在 K 未结算时提交。
    let (load_key, load_submission) = crate::kit::history_v2::outbound::submit_load(|request_key| {
        json!({
            "sessionId": "s1",
            "_meta": {
                peri_acp_types::history_v2::HISTORY_META_KEY:
                    crate::kit::history_v2::initial::initial_request_value(&limits, request_key),
            },
        })
    })
    .expect("scheduler installed");
    let load_receiver = match load_submission {
        crate::kit::history_v2::outbound::Submit::Queued(receiver) => receiver,
        crate::kit::history_v2::outbound::Submit::Direct(_) => {
            panic!("load must be queued, not sent directly")
        }
    };
    let binding = crate::kit::history_window::HistoryV2BodyBinding {
        session_id: "s1".into(),
        window_id: "w0".into(),
        snapshot_token: "snap-0".into(),
        read_ticket: "ticket-0".into(),
        source: peri_acp_types::history_v2::HistorySource::Own,
        entry_id: "s1:e2".into(),
        sub_item_id: "s1:e2:0".into(),
        body_ref: "body-1".into(),
        representation: peri_acp_types::history_v2::HistoryRepresentation::DisplayText,
        media_type: "text/plain; charset=utf-8".into(),
    };
    let body_receiver = match submit_body(&binding, 0, 128).expect("scheduler installed") {
        crate::kit::history_v2::outbound::Submit::Queued(receiver) => receiver,
        crate::kit::history_v2::outbound::Submit::Direct(_) => {
            panic!("body must be queued, not sent directly")
        }
    };

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), server.recv())
            .await
            .is_err(),
        "K 未结算时 load/body 不得上线"
    );

    respond_wire(&server, id, json!({})).await;
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, "session/load", "load 必须排在 K 之后");
    assert_eq!(
        wire_key(&params["_meta"]["peri.history"]["requestKey"]),
        load_key.get()
    );
    assert!(load_key.get() > blocking_key, "K+1 的 key 必须晚于 K");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), server.recv())
            .await
            .is_err(),
        "load 未结算时 body 不得越队"
    );

    respond_wire(&server, id, json!({})).await;
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("body"));
    assert_eq!(
        wire_key(&params["requestKey"]),
        load_key.get() + 1,
        "body 的 key 紧接 load"
    );
    respond_wire(&server, id, json!({})).await;

    assert!(load_receiver.await.unwrap().is_ok());
    assert!(body_receiver.await.unwrap().is_ok());
    shutdown.cancel();
    consumer.abort();
    client.close();
}

/// 生产 `load_session` 走同一出站 FIFO：队列被占用时不得直发 `session/load`。
#[tokio::test]
#[serial]
async fn load_session_waits_behind_a_blocked_queue() {
    let _guard = atomics_guard();
    reset_request_key_for_test();
    let limits = HistoryLimits::default();
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = crate::acp_client::client::AcpTuiClient::new(transport);
    client.install_history_v2_for_test(true, true, limits.clone());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tx);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let consumer = spawn_history_v2_consumer(client.clone(), rx, shutdown.clone());

    let blocking = match crate::kit::history_v2::outbound::enqueue_window(|request_key| {
        Some(HistoryV2ConsumerRequest::Close {
            session_id: "s1".into(),
            request_key,
            window_id: "w0".into(),
            snapshot_token: "snap-0".into(),
            read_ticket: "ticket-0".into(),
        })
    }) {
        crate::kit::history_v2::outbound::WindowEnqueue::Sent(key) => key,
        other => panic!("blocking request must enqueue: {other:?}"),
    };
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("close"));

    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session("s1", "/tmp", None).await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), server.recv())
            .await
            .is_err(),
        "队列未结算时 load_session 不得直发"
    );

    respond_wire(&server, id, json!({})).await;
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, "session/load");
    let load_key = wire_key(&params["_meta"]["peri.history"]["requestKey"]);
    assert_eq!(load_key, blocking.get() + 1, "load 的 key 紧接队列前序");
    respond_wire(
        &server,
        id,
        initial_response_for("s1", "w1", "before-1", &load_key.to_string()),
    )
    .await;
    assert_eq!(load.await.unwrap().unwrap(), "s1");

    shutdown.cancel();
    consumer.abort();
    client.close();
}

/// load 永不返回时，串行 dispatcher 不能被它永久占住。
///
/// 超时按可恢复错误结算（服务端 load 不取消），后续 body 与窗口请求仍能上线。
#[tokio::test(start_paused = true)]
#[serial]
async fn load_timeout_releases_the_serial_dispatcher() {
    let _guard = atomics_guard();
    reset_request_key_for_test();
    let limits = HistoryLimits::default();
    let (transport, server) = peri_acp::transport::mpsc::mpsc_transport_pair();
    let (client, _, _) = crate::acp_client::client::AcpTuiClient::new(transport);
    client.install_history_v2_for_test(true, true, limits.clone());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *atoms::HISTORY_V2_TX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tx);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let consumer = spawn_history_v2_consumer(client.clone(), rx, shutdown.clone());

    let (load_key, load_submission) = crate::kit::history_v2::outbound::submit_load(|request_key| {
        json!({
            "sessionId": "s1",
            "_meta": {
                peri_acp_types::history_v2::HISTORY_META_KEY:
                    crate::kit::history_v2::initial::initial_request_value(&limits, request_key),
            },
        })
    })
    .expect("scheduler installed");
    let load_receiver = match load_submission {
        crate::kit::history_v2::outbound::Submit::Queued(receiver) => receiver,
        crate::kit::history_v2::outbound::Submit::Direct(_) => panic!("load must be queued"),
    };
    let (_id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, "session/load");
    assert_eq!(
        wire_key(&params["_meta"]["peri.history"]["requestKey"]),
        load_key.get()
    );

    // 服务端永不应答；paused 时钟越过 FETCH_TIMEOUT 后必须结算为超时错误。
    let error = load_receiver
        .await
        .expect("dispatcher must reply")
        .expect_err("a load that never returns must time out");
    assert_eq!(error.code, -32603);
    assert!(error.message.contains("timed out"), "{error:?}");

    // body 仍能被调度：dispatcher 已从超时的 load 中恢复。
    let binding = crate::kit::history_window::HistoryV2BodyBinding {
        session_id: "s1".into(),
        window_id: "w0".into(),
        snapshot_token: "snap-0".into(),
        read_ticket: "ticket-0".into(),
        source: peri_acp_types::history_v2::HistorySource::Own,
        entry_id: "s1:e2".into(),
        sub_item_id: "s1:e2:0".into(),
        body_ref: "body-1".into(),
        representation: peri_acp_types::history_v2::HistoryRepresentation::DisplayText,
        media_type: "text/plain; charset=utf-8".into(),
    };
    let body_receiver = match submit_body(&binding, 0, 128).expect("scheduler installed") {
        crate::kit::history_v2::outbound::Submit::Queued(receiver) => receiver,
        crate::kit::history_v2::outbound::Submit::Direct(_) => panic!("body must be queued"),
    };
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("body"));
    respond_wire(&server, id, json!({})).await;
    assert!(body_receiver.await.unwrap().is_ok());

    // 后续窗口请求同样不被阻塞。
    let _close_key = match crate::kit::history_v2::outbound::enqueue_window(|request_key| {
        Some(HistoryV2ConsumerRequest::Close {
            session_id: "s1".into(),
            request_key,
            window_id: "w0".into(),
            snapshot_token: "snap-0".into(),
            read_ticket: "ticket-0".into(),
        })
    }) {
        crate::kit::history_v2::outbound::WindowEnqueue::Sent(key) => key,
        other => panic!("close request must enqueue: {other:?}"),
    };
    let (id, method, params) = next_wire_request(&server).await;
    assert_eq!(method, peri_acp_types::history_v2::HISTORY_METHOD);
    assert_eq!(params["action"], json!("close"));
    respond_wire(&server, id, json!({})).await;

    shutdown.cancel();
    consumer.abort();
    client.close();
}
