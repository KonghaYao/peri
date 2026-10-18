//! 历史页消费者用例（REQ-C01-2/-5、A11）。
//!
//! 覆盖：并发合并判据、RPC 参数与响应解码、空页续读、失效错误码（-32011/-32012）
//! 标记、游标链中途换快照身份时丢弃、以及在发 RPC **之前**就丢掉过期浏览身份的页。

#[cfg(test)]
use super::*;
use peri_acp::transport::{
    AcpTransport,
    mpsc::{MpscClientTransport, MpscServerTransport, mpsc_transport_pair},
    types::{AcpError, IncomingMessage},
};
use serde_json::{Value, json};
use serial_test::serial;
use std::time::Duration;

fn make_client() -> (AcpTuiClient, MpscServerTransport) {
    let (client_transport, server_transport): (MpscClientTransport, MpscServerTransport) =
        mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) = AcpTuiClient::new(client_transport);
    (client, server_transport)
}

fn request(cursor: &str) -> HistoryPageRequest {
    HistoryPageRequest {
        session_id: "s1".to_string(),
        snapshot_token: "tok".to_string(),
        cursor: cursor.to_string(),
        limit: 50,
    }
}

fn page_body(next_cursor: Option<&str>, items: Value) -> Value {
    json!({
        "sessionId": "s1",
        "snapshotToken": "tok",
        "nextCursor": next_cursor,
        "items": items,
    })
}

fn item(entry_id: &str, position: u64) -> Value {
    json!({
        "entryId": entry_id,
        "position": position,
        "subItems": [
            { "position": 0, "update": { "sessionUpdate": "agent_message_chunk", "messageId": "m" } },
        ],
    })
}

/// 收下一条 `session/load` 请求，按 `result` 应答，返回请求参数。
async fn serve_one(server: &MpscServerTransport, result: Result<Value, AcpError>) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(2), server.recv())
        .await
        .expect("session/load should become in-flight")
        .expect("server transport open");
    match message {
        IncomingMessage::Request { id, method, params } => {
            assert_eq!(method, "session/load");
            server.send_response(id, result).await.expect("respond");
            params
        }
        other => panic!("unexpected transport message: {other:?}"),
    }
}

#[test]
fn test_is_duplicate_of_merges_only_same_cursor_and_limit() {
    let in_flight = request("c1").identity();
    assert!(is_duplicate_of(&request("c1"), &in_flight));

    let mut other_cursor = request("c2");
    other_cursor.limit = in_flight.limit;
    assert!(!is_duplicate_of(&other_cursor, &in_flight));

    let mut other_limit = request("c1");
    other_limit.limit = in_flight.limit + 1;
    assert!(!is_duplicate_of(&other_limit, &in_flight));

    let mut other_session = request("c1");
    other_session.session_id = "s2".to_string();
    assert!(!is_duplicate_of(&other_session, &in_flight));

    let mut other_snapshot = request("c1");
    other_snapshot.snapshot_token = "tok-2".to_string();
    assert!(!is_duplicate_of(&other_snapshot, &in_flight));
}

#[tokio::test]
async fn test_fetch_page_sends_cursor_and_decodes_items() {
    let (client, server) = make_client();
    let handle = tokio::spawn(async move {
        serve_one(
            &server,
            Ok(page_body(
                Some("c2"),
                json!([item("s1:a", 10), item("s1:b", 20)]),
            )),
        )
        .await
    });

    let fetched = fetch_page(&client, &request("c1")).await.expect("page");
    let params = handle.await.expect("server task");
    let response = fetched.response;
    let origin = fetched.origin.expect("origin");

    assert_eq!(params.get("sessionId").and_then(Value::as_str), Some("s1"));
    assert_eq!(params.get("cursor").and_then(Value::as_str), Some("c1"));
    assert_eq!(params.get("limit").and_then(Value::as_u64), Some(50));
    assert_eq!(origin.cursor, "c1");
    assert_eq!(origin.limit, 50);
    assert_eq!(origin.session_id, "s1");
    assert_eq!(origin.snapshot_token, "tok");
    // 历史页不携带 setup 参数（服务端明确拒绝 cwd / mcpServers）。
    assert!(params.get("cwd").is_none());
    assert!(params.get("mcpServers").is_none());

    assert_eq!(response.session_id, "s1");
    assert_eq!(response.snapshot_token, "tok");
    assert_eq!(response.next_cursor.as_deref(), Some("c2"));
    assert_eq!(response.items.len(), 2);
    assert_eq!(response.items[0].entry_id, "s1:a");
    assert_eq!(response.items[1].position, 20);
}

/// 空页 + 前进的游标是合法结果：继续往前读，直到拿到有内容的页。
#[tokio::test]
async fn test_fetch_page_skips_empty_pages_until_content() {
    let (client, server) = make_client();
    let handle = tokio::spawn(async move {
        let first = serve_one(&server, Ok(page_body(Some("c2"), json!([])))).await;
        let second = serve_one(
            &server,
            Ok(page_body(Some("c3"), json!([item("s1:old", 5)]))),
        )
        .await;
        (first, second)
    });

    let fetched = fetch_page(&client, &request("c1")).await.expect("page");
    let (first, second) = handle.await.expect("server task");
    let response = fetched.response;

    assert_eq!(first.get("cursor").and_then(Value::as_str), Some("c1"));
    assert_eq!(
        second.get("cursor").and_then(Value::as_str),
        Some("c2"),
        "空页必须用服务端给的 nextCursor 继续往前读"
    );
    assert_eq!(
        fetched.origin.map(|origin| origin.cursor).as_deref(),
        Some("c2"),
        "来源必须是产出 items 的那次请求，而不是被跳过的空页 cursor"
    );
    assert_eq!(response.items.len(), 1);
    assert_eq!(response.items[0].entry_id, "s1:old");
    assert_eq!(response.next_cursor.as_deref(), Some("c3"));
}

#[tokio::test]
async fn test_fetch_page_flags_range_invalidating_errors() {
    let (client, server) = make_client();
    let handle = tokio::spawn(async move {
        serve_one(&server, Err(AcpError::new(-32012, "cursor range expired"))).await
    });

    let error = fetch_page(&client, &request("c1"))
        .await
        .expect_err("range expired must fail");
    let _ = handle.await.expect("server task");

    assert!(error.invalidates, "-32012 必须标记为「浏览身份失效」");
    assert!(error.message.contains("-32012"));
}

#[tokio::test]
async fn test_fetch_page_flags_cross_session_errors() {
    let (client, server) = make_client();
    let handle = tokio::spawn(async move {
        serve_one(
            &server,
            Err(AcpError::new(-32011, "cursor belongs elsewhere")),
        )
        .await
    });

    let error = fetch_page(&client, &request("c1"))
        .await
        .expect_err("cross session must fail");
    let _ = handle.await.expect("server task");

    assert!(error.invalidates, "-32011 必须标记为「浏览身份失效」");
}

/// 游标链中途换快照身份：这不是同一次浏览，丢弃并停止翻页。
#[tokio::test]
async fn test_fetch_page_drops_page_with_changed_snapshot_token() {
    let (client, server) = make_client();
    let handle = tokio::spawn(async move {
        serve_one(
            &server,
            Ok(json!({
                "sessionId": "s1",
                "snapshotToken": "tok-2",
                "nextCursor": null,
                "items": [item("s1:a", 10)],
            })),
        )
        .await
    });

    let error = fetch_page(&client, &request("c1"))
        .await
        .expect_err("changed snapshot token must drop the page");
    let _ = handle.await.expect("server task");

    assert!(error.invalidates);
    assert!(error.message.contains("snapshot token"));
}

/// 服务端静默（连接未断、永不响应）：页 RPC 必须在有限预算内失败，且失败按普通
/// 失败结算——不是范围失效，游标保留供退避后重试（REQ-C01-2）。
///
/// 没有这条预算时，`in_flight` 认领会被永久占用、该会话翻页彻底停滞：transport
/// 明确「连接沉默没有内建超时」（`peri-acp/src/transport/mod.rs`）。
#[tokio::test(start_paused = true)]
async fn test_fetch_page_times_out_on_silent_server() {
    let (client, server) = make_client();
    // 外层护栏只比内层预算宽 2s：内层预算被移除时（回归）本用例明确失败，
    // 而不是让测试挂死到 harness 超时。
    let outcome = tokio::time::timeout(
        PAGE_FETCH_TIMEOUT + Duration::from_secs(2),
        fetch_page(&client, &request("c1")),
    )
    .await
    .expect("取页必须有有限预算：静默服务端不得让 fetch_page 无限等待");
    let error = outcome.expect_err("静默服务端不得让取页成功");
    assert!(!error.invalidates, "静默不是范围失效：游标必须保留可重试");
    assert!(
        error.message.contains("timed out"),
        "超时原因必须写进错误：{}",
        error.message
    );
    // 连接保持存活：关闭会以 -32603 结算，覆盖不到「连接沉默」这条路径。
    let _ = server;
}

/// 超时的端到端后果：在途认领释放、游标保留、原因可展示——翻页不会永久停摆。
#[tokio::test(start_paused = true)]
#[serial]
async fn test_handle_request_releases_in_flight_on_timeout() {
    crate::kit::atoms::init_atoms();
    history_paging::reset_browsing_state();
    history_paging::note_initial_cursor("s1", Some("tok".to_string()), Some("c1".to_string()));
    let current = request("c1");
    let identity = current.identity();
    atoms::HISTORY_PAGING.state().write().in_flight = Some(identity.clone());

    let (client, server) = make_client();
    // 同上层护栏：没有有限预算时本用例失败而不是挂死。
    tokio::time::timeout(
        PAGE_FETCH_TIMEOUT + Duration::from_secs(2),
        handle_request(&client, &current, identity),
    )
    .await
    .expect("handle_request 必须在有限预算内失败");

    let state = atoms::HISTORY_PAGING.state().read().clone();
    assert!(
        state.in_flight.is_none(),
        "超时必须释放在途认领，否则该会话翻页永久停滞：{state:?}"
    );
    assert_eq!(
        state.next_cursor.as_deref(),
        Some("c1"),
        "超时保留当前游标（服务端沉默不代表范围失效）"
    );
    assert!(
        state
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("timed out")),
        "超时原因留给重试展示：{:?}",
        state.last_error
    );
    let _ = server;
}

/// 切会话 / 重新 load 之后到达的请求：在发 RPC **之前**就判过期，不入画面也不发请求
/// （REQ-C01-5、A11）。
#[tokio::test]
#[serial]
async fn test_handle_request_drops_stale_browsing_before_rpc() {
    crate::kit::atoms::init_atoms();
    history_paging::reset_browsing_state();
    history_paging::note_initial_cursor("s1", Some("tok".to_string()), Some("c1".to_string()));

    let (client, server) = make_client();
    // 请求携带的是旧快照身份（上一会话的页）。
    let stale = HistoryPageRequest {
        session_id: "s1".to_string(),
        snapshot_token: "tok-stale".to_string(),
        cursor: "c1".to_string(),
        limit: 50,
    };
    let identity = stale.identity();
    handle_request(&client, &stale, identity).await;

    assert!(
        tokio::time::timeout(Duration::from_millis(200), server.recv())
            .await
            .is_err(),
        "过期浏览身份不得发出 session/load 请求"
    );
}

/// 正常路径：页响应经 `LOCAL_EVENT_TX` 注入桥接任务，并推进游标。
#[tokio::test]
#[serial]
async fn test_handle_request_injects_page_and_advances_cursor() {
    crate::kit::atoms::init_atoms();
    history_paging::reset_browsing_state();
    let (local_tx, mut local_rx) = tokio::sync::mpsc::unbounded_channel();
    let owns_local_tx = atoms::LOCAL_EVENT_TX.set(local_tx).is_ok();

    history_paging::note_initial_cursor("s1", Some("tok".to_string()), Some("c1".to_string()));
    let current = request("c1");
    // 触发侧与消费侧**各自独立**构造同一页的身份（生产里就是这个时序：触发侧认领
    // 在途请求，消费者只拿到页参数）。两者必须相等，否则结算会永远失败、游标不再推进。
    atoms::HISTORY_PAGING.state().write().in_flight = Some(request("c1").identity());
    let identity = current.identity();

    let (client, server) = make_client();
    let handle = tokio::spawn(async move {
        serve_one(
            &server,
            Ok(page_body(Some("c2"), json!([item("s1:old", 10)]))),
        )
        .await
    });
    handle_request(&client, &current, identity).await;
    let _ = handle.await.expect("server task");

    if owns_local_tx {
        let event = local_rx.try_recv().expect("page injected into the bridge");
        match &event.event {
            AcpEventData::HistoryPageLoaded {
                origin, refetch, ..
            } => {
                assert_eq!(
                    origin.as_ref().map(|origin| origin.cursor.as_str()),
                    Some("c1"),
                    "来源 cursor 必须随响应事件传递"
                );
                assert!(refetch.is_none(), "普通页不是补读响应");
            }
            other => panic!("unexpected local event: {other:?}"),
        }
        assert_eq!(event.active_session_id, "s1");
    }
    assert_eq!(
        atoms::HISTORY_PAGING.state().read().next_cursor.as_deref(),
        Some("c2"),
        "成功后游标推进到下一页"
    );
    assert!(atoms::HISTORY_PAGING.state().read().in_flight.is_none());
}

/// 失效错误码：停止对本次浏览继续翻页（游标与快照身份一并作废）。
#[tokio::test]
#[serial]
async fn test_handle_request_invalidates_browsing_on_range_error() {
    crate::kit::atoms::init_atoms();
    history_paging::reset_browsing_state();
    history_paging::note_initial_cursor("s1", Some("tok".to_string()), Some("c1".to_string()));
    let current = request("c1");
    let identity = current.identity();
    atoms::HISTORY_PAGING.state().write().in_flight = Some(identity.clone());

    let (client, server) = make_client();
    let handle = tokio::spawn(async move {
        serve_one(&server, Err(AcpError::new(-32012, "cursor range expired"))).await
    });
    handle_request(&client, &current, identity).await;
    let _ = handle.await.expect("server task");

    let state = atoms::HISTORY_PAGING.state().read().clone();
    assert!(state.next_cursor.is_none(), "失效后不得再翻页");
    assert!(state.snapshot_token.is_none());
    assert!(
        state
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("-32012")),
        "失败原因留给重试展示"
    );
}
