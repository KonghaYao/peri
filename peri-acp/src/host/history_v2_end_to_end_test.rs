//! H8：History V2 wire 端到端（真实 `ServerLoop` + MPSC transport + 临时 SQLite）。
//!
//! 本文件只补**同一条真实请求路径上的组合**：`initialize` 协商 → `session/load`
//! initial → `_peri/history` page/seek/body → live `session/update` → `session/close`
//! → 重复 load。各分层细节仍由 `history_v2_load_test`、`history_v2_adapter_*`、
//! `history_service/*_test` 覆盖，这里不复制它们的断言矩阵。
//!
//! 用例名以 `history_v2_end_to_end` 开头，便于
//! `cargo test --lib -- history_v2_end_to_end` 过滤。

use super::*;
use crate::session::event_sink::TransportEventSink;
use peri_acp_types::event::{EventSink, ExecutorEvent};
use peri_acp_types::history_v2::{HistoryErrorData, HistoryErrorReason};
use peri_acp_types::messages::MessageId;

/// 超过页预算的 assistant 正文：open/initial 只返回 opaque descriptor，正文走 body。
const OPAQUE_TEXT_BYTES: usize = 70 * 1024;
/// initial 的目标页字节（协议下限），保证大条目被投影为 opaque 而不是内联。
const INITIAL_TARGET_BYTES: u64 = 64 * 1024;
/// initial 的请求 limit；实际页长仍由 64 KiB 预算决定。
const INITIAL_LIMIT: u32 = 2;

fn initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "_meta": {
                "peri.history": {
                    "versions": [2],
                    "requestedProfile": "windowed",
                },
            },
        },
    })
}

fn e2e_initial_meta(request_key: &str, limit: u32, target_bytes: u64) -> Value {
    json!({
        "_meta": {
            "peri.history": {
                "version": 2,
                "action": "initial",
                "requestKey": request_key,
                "limit": limit,
                "targetBytes": target_bytes,
            },
        },
    })
}

fn e2e_load_params(session_id: &str, meta: &Value) -> Value {
    let mut params = json!({ "sessionId": session_id });
    params
        .as_object_mut()
        .unwrap()
        .extend(meta.as_object().unwrap().clone());
    params
}

fn e2e_error_data(error: &crate::transport::types::AcpError) -> HistoryErrorData {
    serde_json::from_value(
        error
            .data
            .clone()
            .unwrap_or_else(|| panic!("领域错误必须带 data：{error:?}")),
    )
    .unwrap_or_else(|cause| panic!("错误 data 必须是 HistoryErrorData：{cause}; {error:?}"))
}

/// 起真实 `ServerLoop`，**不预置**连接 caps：协商必须经 wire `initialize` 完成。
///
/// 同时返回**服务端方向** transport：live 断言要用它注入 `TransportEventSink`，
/// 复用客户端方向会把通知发回客户端自己的入站通道。
async fn spawn_v2_e2e_loop(
    fixture: &PageFixture,
) -> (
    Arc<crate::transport::mpsc::MpscClientTransport>,
    Arc<dyn crate::transport::AcpTransport>,
    tokio::task::JoinHandle<()>,
) {
    let (client, server) = crate::transport::mpsc::mpsc_transport_pair();
    let server: Arc<dyn crate::transport::AcpTransport> = Arc::new(server);
    let (client, _sessions, task) = spawn_loop_with_transport(
        Arc::clone(&fixture.cfg),
        fixture.sessions.clone(),
        None,
        client,
        Arc::clone(&server),
    )
    .await;
    (client, server, task)
}

/// 真实 `initialize`：客户端声明 V2，服务端必须选择 windowed profile。
async fn negotiate_v2(client: &crate::transport::mpsc::MpscClientTransport) {
    let reply = client
        .send_request("initialize", initialize_params())
        .await
        .expect("本地 SQLite 已兑现 V2 读取端口，initialize 必须成功");
    let meta = &reply["agentCapabilities"]["_meta"]["peri.history"];
    assert_eq!(meta["version"], 2, "服务端必须选择 V2：{reply}");
    assert_eq!(
        meta["selectedProfile"], "windowed",
        "必须显式选择 windowed profile：{reply}"
    );
}

async fn append_opaque_assistant(fixture: &PageFixture, text: &str) -> MessageId {
    let payload = PersistedPayload::Message(BaseMessage::ai(text.to_owned()));
    let id = payload.id();
    fixture
        .cfg
        .session_resources
        .append_history(&fixture.sid, &[payload])
        .await
        .expect("append oversized assistant message");
    id
}

fn window_refs(page: &Value) -> (String, String, String) {
    (
        page["windowId"].as_str().expect("windowId").to_owned(),
        page["snapshotToken"]
            .as_str()
            .expect("snapshotToken")
            .to_owned(),
        page["readTicket"].as_str().expect("readTicket").to_owned(),
    )
}

async fn history_page_request(
    client: &crate::transport::mpsc::MpscClientTransport,
    session_id: &str,
    page: &Value,
    request_key: &str,
    direction: &str,
    cursor: Option<&str>,
    limit: u32,
) -> Result<Value, crate::transport::types::AcpError> {
    let (window_id, snapshot_token, read_ticket) = window_refs(page);
    let mut params = json!({
        "version": 2,
        "action": "page",
        "sessionId": session_id,
        "requestKey": request_key,
        "windowId": window_id,
        "snapshotToken": snapshot_token,
        "readTicket": read_ticket,
        "direction": direction,
        "limit": limit,
    });
    if let Some(cursor) = cursor {
        params["cursor"] = Value::String(cursor.to_owned());
    }
    client.send_request("_peri/history", params).await
}

async fn history_seek_request(
    client: &crate::transport::mpsc::MpscClientTransport,
    session_id: &str,
    page: &Value,
    request_key: &str,
    entry_id: &str,
    before: u32,
    after: u32,
) -> Result<Value, crate::transport::types::AcpError> {
    let (window_id, snapshot_token, read_ticket) = window_refs(page);
    client
        .send_request(
            "_peri/history",
            json!({
                "version": 2,
                "action": "seek",
                "sessionId": session_id,
                "requestKey": request_key,
                "windowId": window_id,
                "snapshotToken": snapshot_token,
                "readTicket": read_ticket,
                "anchor": {
                    "kind": "entry",
                    "entryId": entry_id,
                    "source": { "kind": "own" },
                },
                "before": before,
                "after": after,
            }),
        )
        .await
}

async fn history_body_request(
    client: &crate::transport::mpsc::MpscClientTransport,
    session_id: &str,
    page: &Value,
    request_key: &str,
    body_ref: &str,
    offset: u64,
    max_bytes: u64,
) -> Result<Value, crate::transport::types::AcpError> {
    let (window_id, snapshot_token, read_ticket) = window_refs(page);
    client
        .send_request(
            "_peri/history",
            json!({
                "version": 2,
                "action": "body",
                "sessionId": session_id,
                "requestKey": request_key,
                "windowId": window_id,
                "snapshotToken": snapshot_token,
                "readTicket": read_ticket,
                "bodyRef": body_ref,
                "offset": offset.to_string(),
                "maxBytes": max_bytes,
            }),
        )
        .await
}

fn decode_body_chunk(chunk: &Value) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(chunk["data"].as_str().expect("body data"))
        .expect("body data 必须是 standard base64")
}

/// V2 路径不得发送 V1 的 canonical 回放：load 响应之后短时间内不接受任何
/// `agent_message_chunk`。其他标准通知（配置等）忽略。
async fn assert_no_replay_updates(client: &crate::transport::mpsc::MpscClientTransport) {
    loop {
        let Ok(incoming) =
            tokio::time::timeout(std::time::Duration::from_millis(200), client.recv()).await
        else {
            return;
        };
        let Some(crate::transport::types::IncomingMessage::Notification { method, params }) =
            incoming
        else {
            continue;
        };
        assert!(
            !(method == "session/update"
                && params["update"]["sessionUpdate"] == "agent_message_chunk"),
            "V2 initial 已交付首屏，不得再回放 session/update：{params}"
        );
    }
}

async fn wait_live_resumed(live: &crate::host::live_delivery::LiveDelivery, session_id: &str) {
    for _ in 0..400 {
        if !live.is_paused(session_id) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("live delivery 未在 load 响应后恢复直发");
}

async fn next_transcript_update(client: &crate::transport::mpsc::MpscClientTransport) -> Value {
    loop {
        let incoming = tokio::time::timeout(std::time::Duration::from_secs(5), client.recv())
            .await
            .expect("live session/update timeout")
            .expect("client transport closed");
        if let crate::transport::types::IncomingMessage::Notification { method, params } = incoming
        {
            if method == "session/update"
                && params["update"]["sessionUpdate"] == "agent_message_chunk"
            {
                return params;
            }
        }
    }
}

fn e2e_transcript(text: &str) -> ExecutorEvent {
    ExecutorEvent::TextChunk {
        message_id: MessageId::new(),
        chunk: text.to_owned(),
        source_agent_id: None,
    }
}

/// 一条真实请求路径：协商 → initial → before/after → seek → body → live → close。
#[tokio::test]
#[serial]
async fn history_v2_end_to_end_initial_pages_seek_body_live_and_close() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let text = "x".repeat(OPAQUE_TEXT_BYTES);
    let opaque_id = append_opaque_assistant(&fixture, &text).await;
    let (client, server, task) = spawn_v2_e2e_loop(&fixture).await;

    // 1. 协商：客户端声明 → 服务端选择 V2。
    negotiate_v2(&client).await;

    // 2. 首屏：initial 携带 page、deliveryId 与 liveBoundary；V2 不发回放。
    let response = client
        .send_request(
            "session/load",
            e2e_load_params(
                &sid,
                &e2e_initial_meta("1", INITIAL_LIMIT, INITIAL_TARGET_BYTES),
            ),
        )
        .await
        .expect("V2 initial load 必须成功");
    let initial = &response["_meta"]["peri.history"];
    assert_eq!(initial["version"], 2);
    assert_eq!(initial["kind"], "initial");
    assert_eq!(initial["requestKey"], "1");
    assert_eq!(initial["executionReady"], true);
    assert!(
        response["_meta"].get("periLoadGeneration").is_none(),
        "V2 响应不得混入 V1 回放代次：{response}"
    );
    let stream_id = initial["liveBoundary"]["streamId"]
        .as_str()
        .expect("liveBoundary.streamId")
        .to_owned();
    let after_seq = initial["liveBoundary"]["afterSeq"]
        .as_str()
        .expect("liveBoundary.afterSeq（十进制字符串）")
        .to_owned();
    assert_eq!(after_seq, "0", "首次 initial 从 0 之后开始");
    assert!(
        initial["deliveryId"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "initial 必须交付 deliveryId：{initial}"
    );
    let page = initial["page"].clone();
    assert_eq!(page["version"], 2);
    assert_eq!(page["sessionId"], sid);
    let entries = page["items"].as_array().expect("page.items");
    assert_eq!(
        entries.len(),
        1,
        "70 KiB canonical 正文先耗尽 64 KiB 页预算，首屏只交付该 descriptor"
    );
    assert!(
        page["beforeCursor"].is_string(),
        "更早历史仍存在，必须给出 beforeCursor：{page}"
    );
    assert_no_replay_updates(&client).await;
    let initial_ids: Vec<String> = entries
        .iter()
        .map(|entry| entry["entryId"].as_str().unwrap().to_owned())
        .collect();

    // 3a. page before：沿 initial cursor 读取更早历史；opaque 条目可能落在首屏或该页。
    let before_cursor = page["beforeCursor"].as_str().unwrap().to_owned();
    let older = history_page_request(&client, &sid, &page, "2", "before", Some(&before_cursor), 2)
        .await
        .expect("before 页必须成功");
    let older_entries = older["items"].as_array().expect("before items");
    let older_ids: Vec<String> = older_entries
        .iter()
        .map(|entry| entry["entryId"].as_str().unwrap().to_owned())
        .collect();
    assert!(!older_ids.is_empty(), "更早方向仍有条目：{older}");
    assert!(
        older_ids.iter().all(|id| !initial_ids.contains(id)),
        "before 页不得重复首屏条目"
    );
    assert_eq!(
        older["windowId"], page["windowId"],
        "同一 initial 窗口必须继续复用"
    );

    let opaque_entry = older_entries
        .iter()
        .chain(entries.iter())
        .find(|entry| {
            entry["subItems"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["kind"] == "opaque"))
        })
        .expect("70 KiB 正文必须投影为 opaque 条目");
    assert!(
        opaque_entry["entryId"]
            .as_str()
            .is_some_and(|value| value.ends_with(&opaque_id.as_uuid().to_string())),
        "opaque 条目必须保留稳定消息身份：{opaque_entry}"
    );
    let opaque_item = opaque_entry["subItems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "opaque")
        .expect("opaque subItem");
    assert_eq!(opaque_item["body"]["representation"], "display-text");
    assert_eq!(
        opaque_item["body"]["mediaType"],
        "text/plain; charset=utf-8"
    );
    let descriptor_total = opaque_item["body"]["totalBytes"]
        .as_str()
        .expect("descriptor.totalBytes")
        .parse::<u64>()
        .expect("decimal totalBytes");
    assert!(
        descriptor_total > OPAQUE_TEXT_BYTES as u64,
        "page descriptor 的 totalBytes 是 canonical 预估"
    );
    let body_ref = opaque_item["body"]["bodyRef"]
        .as_str()
        .expect("bodyRef")
        .to_owned();

    // 3b. page after：从 before 页的 afterCursor 回到 fixed tail。
    let after_cursor = older["afterCursor"]
        .as_str()
        .expect("before 页必须给出 afterCursor")
        .to_owned();
    let newer = history_page_request(&client, &sid, &page, "3", "after", Some(&after_cursor), 5)
        .await
        .expect("after 页必须成功");
    let newer_ids: Vec<String> = newer["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["entryId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(newer_ids, initial_ids, "after 页必须回到同一 fixed tail");
    assert_eq!(newer["snapshotToken"], page["snapshotToken"]);

    // 3c. seek：按稳定条目身份定位，锚点恰好出现一次。
    let sought = history_seek_request(
        &client,
        &sid,
        &page,
        "4",
        opaque_entry["entryId"].as_str().unwrap(),
        1,
        0,
    )
    .await
    .expect("seek 必须成功");
    let hits = sought["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["entryId"] == opaque_entry["entryId"])
        .count();
    assert_eq!(hits, 1, "seek 锚点只出现一次：{sought}");

    // 3d. body：分块拼接 = 投影文本；body totalBytes 才是权威长度。
    let mut offset = 0_u64;
    let mut joined = Vec::new();
    let mut body_total = None;
    for attempt in 0..8 {
        let chunk = history_body_request(
            &client,
            &sid,
            &page,
            &format!("{}", 5 + attempt),
            &body_ref,
            offset,
            64 * 1024,
        )
        .await
        .expect("body chunk 必须成功");
        assert_eq!(chunk["kind"], "body");
        assert_eq!(chunk["representation"], "display-text");
        assert_eq!(chunk["encoding"], "base64");
        assert_eq!(chunk["bodyRef"], body_ref.as_str());
        let total = chunk["totalBytes"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert_eq!(
            *body_total.get_or_insert(total),
            total,
            "totalBytes 必须稳定"
        );
        joined.extend_from_slice(&decode_body_chunk(&chunk));
        if chunk["complete"] == Value::Bool(true) {
            assert!(
                chunk["nextOffset"].is_null(),
                "complete 时不得给 nextOffset"
            );
            break;
        }
        offset = chunk["nextOffset"]
            .as_str()
            .expect("未完成块必须给 nextOffset")
            .parse::<u64>()
            .expect("decimal nextOffset");
    }
    assert_eq!(
        body_total,
        Some(OPAQUE_TEXT_BYTES as u64),
        "body 的 totalBytes 是投影后的展示长度"
    );
    assert_eq!(
        String::from_utf8(joined).unwrap(),
        text,
        "分块拼回必须等于投影正文"
    );
    assert!(
        body_total.unwrap() < descriptor_total,
        "body 权威长度小于 page 的 canonical 预估"
    );

    // 4. live：`_meta["peri.delivery"]` 连续、deliveryId 唯一。
    let live = fixture
        .sessions
        .lock()
        .await
        .get(&sid)
        .and_then(|state| state.live_delivery.clone())
        .expect("V2 load 必须绑定实时注册表");
    wait_live_resumed(&live, &sid).await;
    let sink = TransportEventSink::new(server, fixture.cfg.session_manager.caps_registry())
        .with_live_delivery(Arc::clone(&live));
    let mut delivery_ids = Vec::new();
    for (index, expected_seq) in ["1", "2"].iter().enumerate() {
        sink.push_event(&sid, &e2e_transcript(&format!("live-{index}")), 0)
            .await
            .expect("push live transcript");
        let update = next_transcript_update(&client).await;
        let meta = &update["_meta"]["peri.delivery"];
        assert_eq!(
            meta["eventSeq"], *expected_seq,
            "eventSeq 必须从 afterSeq+1 连续"
        );
        assert_eq!(meta["streamId"], stream_id.as_str());
        let delivery_id = meta["deliveryId"].as_str().expect("deliveryId").to_owned();
        assert!(
            delivery_ids.iter().all(|seen| seen != &delivery_id),
            "deliveryId 必须唯一：{delivery_ids:?} vs {delivery_id}"
        );
        delivery_ids.push(delivery_id);
    }

    // 5. close：旧窗口 page/body 都返回 window_closed。
    client
        .send_request("session/close", json!({ "sessionId": sid }))
        .await
        .expect("session/close 必须成功");
    let page_error = history_page_request(
        &client,
        &sid,
        &page,
        "20",
        "before",
        Some(&before_cursor),
        2,
    )
    .await
    .expect_err("关闭后的窗口不得继续分页");
    assert_eq!(
        e2e_error_data(&page_error).reason,
        HistoryErrorReason::WindowClosed
    );
    let body_error = history_body_request(&client, &sid, &page, "21", &body_ref, 0, 1024)
        .await
        .expect_err("关闭后的窗口不得继续读正文");
    assert_eq!(
        e2e_error_data(&body_error).reason,
        HistoryErrorReason::WindowClosed
    );

    task.abort();
}

/// 6. 同一连接重复 `session/load`：每次替换窗口，不触发 `capacity_exceeded`。
#[tokio::test]
#[serial]
async fn history_v2_end_to_end_repeated_loads_replace_window_without_capacity_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let (client, _server, task) = spawn_v2_e2e_loop(&fixture).await;

    negotiate_v2(&client).await;

    let mut previous_window = String::new();
    let mut first_page = Value::Null;
    for key in 1..=3 {
        let response = client
            .send_request(
                "session/load",
                e2e_load_params(
                    &sid,
                    &e2e_initial_meta(&key.to_string(), 2, INITIAL_TARGET_BYTES),
                ),
            )
            .await
            .unwrap_or_else(|error| panic!("第 {key} 次 V2 load 必须成功：{error:?}"));
        let page = response["_meta"]["peri.history"]["page"].clone();
        let window_id = page["windowId"].as_str().expect("windowId").to_owned();
        assert_ne!(
            window_id, previous_window,
            "每次 initial 必须建立新窗口而不是累积旧窗口"
        );
        assert_ne!(window_id, "");
        previous_window = window_id;
        if key == 1 {
            first_page = page;
        }
    }

    // 旧窗口被新 initial 替换后按 closed 身份处理，而不是容量错误。
    let error = history_page_request(
        &client,
        &sid,
        &first_page,
        "10",
        "before",
        first_page["beforeCursor"].as_str(),
        2,
    )
    .await
    .expect_err("被替换的旧窗口不得继续分页");
    assert_eq!(
        e2e_error_data(&error).reason,
        HistoryErrorReason::WindowClosed,
        "旧窗口必须是 window_closed，不能是 capacity_exceeded：{error:?}"
    );

    task.abort();
}
