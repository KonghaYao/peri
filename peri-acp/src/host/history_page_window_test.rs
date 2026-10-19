//! `peri.historyWindowV1` 的 wire 级行为：协商、open/page/seek、失效、fixed tail、
//! oversize descriptor 与正文分块。
//!
//! 走真实 `ServerLoop` + MPSC transport + 真实 SQLite 门面（父模块夹具），
//! 覆盖能力门、sessions 短锁二次校验与响应形状。

use super::*;

fn window_caps() -> PeriCaps {
    PeriCaps {
        history_window_v1: true,
        history_pagination: true,
        replay: true,
        ..Default::default()
    }
}

async fn open(
    client: &crate::transport::mpsc::MpscClientTransport,
    sid: &str,
    history: Value,
) -> Result<Value, AcpError> {
    client
        .send_request(
            "session/load",
            json!({ "sessionId": sid, "history": history }),
        )
        .await
}

async fn page(
    client: &crate::transport::mpsc::MpscClientTransport,
    sid: &str,
    snapshot: &Value,
    ticket: &Value,
    cursor_key: &str,
    cursor: &Value,
    limit: u64,
) -> Result<Value, AcpError> {
    client
        .send_request(
            "session/load",
            json!({
                "sessionId": sid,
                "history": {
                    "kind": "page",
                    "snapshotToken": snapshot,
                    "readTicket": ticket,
                    cursor_key: cursor,
                    "limit": limit,
                }
            }),
        )
        .await
}

#[tokio::test]
#[serial]
async fn open_latest_pages_backwards_and_reports_metadata() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 2 }),
    )
    .await
    .expect("negotiated open must succeed");
    for key in [
        "sessionId",
        "sessionGeneration",
        "structureVersion",
        "snapshotToken",
        "committedTail",
        "readTicket",
        "beforeCursor",
        "afterCursor",
        "items",
    ] {
        assert!(
            opened.get(key).is_some(),
            "open response missing {key}: {opened}"
        );
    }
    assert_eq!(opened["items"].as_array().unwrap().len(), 2);
    assert!(opened["readTicket"].as_str().unwrap().len() > 8);
    assert!(opened["beforeCursor"].is_string());
    assert!(opened["afterCursor"].is_null());
    assert_eq!(
        opened["items"][0]["source"].as_str(),
        Some("own"),
        "V1 items must carry an explicit source"
    );

    let fetched = page(
        &client,
        &sid,
        &opened["snapshotToken"],
        &opened["readTicket"],
        "beforeCursor",
        &opened["beforeCursor"],
        2,
    )
    .await
    .expect("before page must succeed");
    assert_eq!(fetched["items"].as_array().unwrap().len(), 1);
    assert!(fetched["beforeCursor"].is_null());
    // 更早页与 durable tail 之间仍有行：必须暴露更晚方向的内侧游标，
    // 否则尾段被淘汰后无法向下补回（页到达 tail 时 afterCursor 才为 null）。
    assert!(
        fetched["afterCursor"].is_string(),
        "a before page must expose the newer direction: {fetched}"
    );
    assert_eq!(fetched["readTicket"], opened["readTicket"]);
    assert_eq!(fetched["committedTail"], opened["committedTail"]);

    let returned = page(
        &client,
        &sid,
        &opened["snapshotToken"],
        &opened["readTicket"],
        "afterCursor",
        &fetched["afterCursor"],
        2,
    )
    .await
    .expect("the later cursor from a before page must be usable");
    let returned_ids: Vec<&str> = returned["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["entryId"].as_str())
        .collect();
    assert_eq!(returned_ids.len(), 2);
    assert!(
        returned["afterCursor"].is_null(),
        "the returned page reaches the durable tail: {returned}"
    );

    task.abort();
}

#[tokio::test]
#[serial]
async fn open_entry_direction_after_returns_the_anchor() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let own = fixture.own_ids().await;
    assert!(own.len() >= 3, "fixture provides three own entries");
    let anchor = own[1];
    let anchor_entry = peri_acp_types::history_window::HistoryEntryId::new(&sid, anchor).encode();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let opened = open(
        &client,
        &sid,
        json!({
            "kind": "open",
            "direction": "after",
            "anchor": { "kind": "entry", "entryId": anchor_entry, "source": "own" },
            "limit": 2
        }),
    )
    .await
    .expect("open(entry, after) must succeed");
    let entry_ids: Vec<&str> = opened["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["entryId"].as_str())
        .collect();
    assert_eq!(
        entry_ids.first().copied(),
        Some(anchor_entry.as_str()),
        "after 半段必须包含锚点自身：{opened}"
    );
    assert_eq!(entry_ids.len(), 2, "锚点只出现一次并推进一页");
    assert!(
        opened["beforeCursor"].is_string(),
        "锚点之前仍有行时必须暴露更早方向：{opened}"
    );
    assert!(
        opened["afterCursor"].is_null(),
        "页已到达 durable tail：{opened}"
    );

    task.abort();
}

#[tokio::test]
#[serial]
async fn unnegotiated_window_request_is_rejected_without_initial_load() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) =
        spawn_loop(cfg, fixture.sessions, Some(PeriCaps::default())).await;

    let error = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "around" }),
    )
    .await
    .expect_err("history window must be fail-closed before negotiation");
    assert_eq!(error.code, -32601);
    assert_eq!(
        error.data.as_ref().unwrap()["reason"],
        "history_window_not_negotiated"
    );
    assert!(
        s9c_drain_notifications(&client).await.is_empty(),
        "rejection must not replay an initial load"
    );
    task.abort();
}

#[tokio::test]
#[serial]
async fn schema_and_budget_bounds_are_rejected_before_storage() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    // history 与旧顶层字段互斥。
    let mixed = client
        .send_request(
            "session/load",
            json!({
                "sessionId": sid,
                "cursor": "old-cursor",
                "history": { "kind": "open", "direction": "around" }
            }),
        )
        .await
        .expect_err("old cursor and history are mutually exclusive");
    assert_eq!(mixed.code, -32602);
    assert_eq!(
        mixed.data.as_ref().unwrap()["reason"],
        "invalid_history_request"
    );

    for history in [
        json!({ "kind": "unknown" }),
        json!({ "kind": "open", "direction": "around", "extra": 1 }),
        json!({ "kind": "open", "direction": "around", "limit": 0 }),
        json!({ "kind": "open", "direction": "around", "byteBudget": 0 }),
    ] {
        let error = open(&client, &sid, history.clone()).await.unwrap_err();
        assert_eq!(error.code, -32602, "history={history} must be rejected");
        assert_eq!(
            error.data.as_ref().unwrap()["reason"],
            "invalid_history_request"
        );
    }

    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap();
    let both = client
        .send_request(
            "session/load",
            json!({
                "sessionId": sid,
                "history": {
                    "kind": "page",
                    "snapshotToken": opened["snapshotToken"],
                    "readTicket": opened["readTicket"],
                    "beforeCursor": opened["beforeCursor"],
                    "afterCursor": "x",
                    "limit": 1
                }
            }),
        )
        .await
        .expect_err("both cursors must be rejected");
    assert_eq!(both.code, -32602);
    assert_eq!(
        both.data.as_ref().unwrap()["reason"],
        "invalid_history_request"
    );

    let malformed = page(
        &client,
        &sid,
        &opened["snapshotToken"],
        &opened["readTicket"],
        "beforeCursor",
        &json!("not-a-history-window-cursor"),
        1,
    )
    .await
    .expect_err("malformed cursors must be rejected");
    assert_eq!(malformed.code, -32602);
    assert_eq!(
        malformed.data.as_ref().unwrap()["reason"],
        "invalid_history_request"
    );
    task.abort();
}

#[tokio::test]
#[serial]
async fn second_check_rejects_stale_generation_structure_ticket_and_snapshot() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 2 }),
    )
    .await
    .unwrap();
    let cursor = opened["beforeCursor"].clone();

    // structureVersion 变化：窗口失效（-32012）。
    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().structure_version += 1;
    }
    let stale_structure = page(
        &client,
        &sid,
        &opened["snapshotToken"],
        &opened["readTicket"],
        "beforeCursor",
        &cursor,
        2,
    )
    .await
    .unwrap_err();
    assert_eq!(stale_structure.code, -32012);

    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().structure_version -= 1;
    }
    let reopened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 2 }),
    )
    .await
    .unwrap();
    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().history_window.revoke_ticket();
    }
    let revoked = page(
        &client,
        &sid,
        &reopened["snapshotToken"],
        &reopened["readTicket"],
        "beforeCursor",
        &reopened["beforeCursor"],
        2,
    )
    .await
    .unwrap_err();
    assert_eq!(revoked.code, -32014);

    let reopened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 2 }),
    )
    .await
    .unwrap();
    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().session_generation += 1;
    }
    let stale_generation = page(
        &client,
        &sid,
        &reopened["snapshotToken"],
        &reopened["readTicket"],
        "beforeCursor",
        &reopened["beforeCursor"],
        2,
    )
    .await
    .unwrap_err();
    assert_eq!(stale_generation.code, -32012);

    let reopened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 2 }),
    )
    .await
    .unwrap();
    let mismatched = page(
        &client,
        &sid,
        &json!("wrong-snapshot"),
        &reopened["readTicket"],
        "beforeCursor",
        &reopened["beforeCursor"],
        2,
    )
    .await
    .unwrap_err();
    assert_eq!(mismatched.code, -32015);
    task.abort();
}

#[tokio::test]
#[serial]
async fn fixed_tail_keeps_later_appends_out_of_an_open_snapshot() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let own = fixture.own_ids().await;
    let anchor = own.first().expect("fixture has own history");
    let anchor_entry = peri_acp_types::history_window::HistoryEntryId::new(&sid, *anchor).encode();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap();
    let sought = client
        .send_request(
            "session/load",
            json!({
                "sessionId": sid,
                "history": {
                    "kind": "seek",
                    "snapshotToken": opened["snapshotToken"],
                    "readTicket": opened["readTicket"],
                    "anchor": { "entryId": anchor_entry, "source": "own" },
                    "beforeLimit": 1,
                    "afterLimit": 1
                }
            }),
        )
        .await
        .expect("seek must succeed");
    let after_cursor = sought["afterCursor"]
        .as_str()
        .expect("seek must expose a later cursor")
        .to_owned();
    let tail = sought["committedTail"].clone();

    let later = PersistedPayload::Message(BaseMessage::human("append after open"));
    fixture
        .cfg
        .session_resources
        .append_history(&sid, std::slice::from_ref(&later))
        .await
        .unwrap();

    let page = page(
        &client,
        &sid,
        &opened["snapshotToken"],
        &opened["readTicket"],
        "afterCursor",
        &json!(after_cursor),
        2,
    )
    .await
    .expect("fixed-tail page must succeed after append");
    let forbidden = peri_acp_types::history_window::HistoryEntryId::new(&sid, later.id()).encode();
    let entry_ids: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["entryId"].as_str())
        .collect();
    assert!(
        !entry_ids.contains(&forbidden.as_str()),
        "appended row must stay outside the fixed snapshot: {page}"
    );
    assert_eq!(page["committedTail"], tail);
    task.abort();
}

#[tokio::test]
#[serial]
async fn oversize_descriptor_serves_bounded_body_chunks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1, "byteBudget": 1 }),
    )
    .await
    .unwrap();
    // 只读正文同样不依赖执行上下文驻留。
    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().context_ownership = crate::host::ContextOwnership::HandedOff;
    }
    let item = &opened["items"][0];
    let body = item
        .get("body")
        .expect("byteBudget=1 must produce a descriptor");
    let entry_id = item["entryId"].as_str().unwrap();
    let total = body["totalBytes"].as_u64().unwrap();
    assert!(total > 1);

    let mut offset = 0u64;
    let mut collected = Vec::new();
    for _ in 0..64 {
        let chunk = client
            .send_request(
                "session/history/body",
                json!({
                    "sessionId": sid,
                    "snapshotToken": opened["snapshotToken"],
                    "readTicket": opened["readTicket"],
                    "entryId": entry_id,
                    "offset": offset,
                    "maxBytes": 7
                }),
            )
            .await
            .expect("bounded body chunk must succeed");
        let bytes = chunk["bytes"].as_array().unwrap();
        assert!(bytes.len() <= 7);
        collected.extend(bytes.iter().map(|byte| byte.as_u64().unwrap() as u8));
        if chunk["complete"].as_bool().unwrap() {
            assert!(chunk["nextOffset"].is_null());
            break;
        }
        let next = chunk["nextOffset"].as_u64().unwrap();
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(
        collected.len() as u64,
        total,
        "chunks must reconstruct the stored byte length"
    );
    String::from_utf8(collected)
        .expect("chunking must preserve UTF-8 boundaries in the byte stream");

    // ReloadRequired 只要求重新准备执行上下文，不把同一 descriptor 的正文读取关掉。
    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().context_ownership =
            crate::host::ContextOwnership::ReloadRequired;
    }
    let again = client
        .send_request(
            "session/history/body",
            json!({
                "sessionId": sid,
                "snapshotToken": opened["snapshotToken"],
                "readTicket": opened["readTicket"],
                "entryId": entry_id,
                "offset": 0,
                "maxBytes": 7
            }),
        )
        .await
        .expect("ReloadRequired must allow read-only body");
    assert!(!again["bytes"].as_array().unwrap().is_empty());

    let invalid = client
        .send_request(
            "session/history/body",
            json!({
                "sessionId": sid,
                "snapshotToken": opened["snapshotToken"],
                "readTicket": opened["readTicket"],
                "entryId": entry_id,
                "offset": 0,
                "maxBytes": 0
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(invalid.code, -32602);
    task.abort();
}

#[tokio::test]
#[serial]
async fn rewind_revokes_the_window_and_advances_structure_once() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let own = fixture.own_ids().await;
    let target = own
        .last()
        .expect("fixture has own history")
        .as_uuid()
        .to_string();
    fixture.cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..window_caps()
        },
    );
    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap();
    let preview = client
        .send_request(
            "session/rewind-preview",
            json!({ "sessionId": sid, "target_message_id": target }),
        )
        .await
        .expect("rewind preview");
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_owned();
    client
        .send_request(
            "session/rewind",
            json!({
                "sessionId": sid,
                "target_message_id": target,
                "preview_fingerprint": fingerprint,
            }),
        )
        .await
        .expect("rewind must succeed");
    s9c_drain_notifications(&client).await;

    let stale = page(
        &client,
        &sid,
        &opened["snapshotToken"],
        &opened["readTicket"],
        "beforeCursor",
        &opened["beforeCursor"],
        1,
    )
    .await
    .unwrap_err();
    assert!(
        stale.code == -32012 || stale.code == -32014,
        "rewind must invalidate the window, got {stale:?}"
    );

    {
        let states = sessions.lock().await;
        assert_eq!(
            states.get(&sid).unwrap().structure_version,
            1,
            "one durable rewind advances structureVersion exactly once"
        );
    }
    let reopened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "around", "limit": 1 }),
    )
    .await
    .expect("a fresh open must recover after rewind");
    assert_eq!(reopened["structureVersion"].as_u64(), Some(1));
    task.abort();
}

#[tokio::test]
#[serial]
async fn warm_load_keeps_generation_and_old_window() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap();
    let generation = opened["sessionGeneration"].as_str().unwrap().to_owned();

    client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .expect("initial load on a live session must still succeed");
    s9c_drain_notifications(&client).await;

    {
        let states = sessions.lock().await;
        let state = states.get(&sid).unwrap();
        assert_eq!(
            state.session_generation.to_string(),
            generation,
            "warm load 只推进本连接 loadGeneration，不替换共享 sessionGeneration"
        );
    }
    let still_valid = page(
        &client,
        &sid,
        &opened["snapshotToken"],
        &opened["readTicket"],
        "beforeCursor",
        &opened["beforeCursor"],
        1,
    )
    .await
    .expect("warm load must not invalidate another connection's window");
    assert!(still_valid.get("items").is_some());
    task.abort();
}

#[tokio::test]
#[serial]
async fn empty_snapshot_serializes_committed_tail_as_explicit_null() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let ids = fixture.own_ids().await;
    fixture
        .cfg
        .session_resources
        .remove_history_entries(&sid, &ids)
        .await
        .expect("fixture history removal");
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "around", "limit": 2 }),
    )
    .await
    .expect("empty snapshot open must succeed");
    assert!(
        opened.get("committedTail").is_some_and(Value::is_null),
        "committedTail must be present and null for an empty snapshot: {opened}"
    );
    assert!(opened["items"].as_array().unwrap().is_empty());
    assert!(opened["beforeCursor"].is_null());
    assert!(opened["afterCursor"].is_null());
    task.abort();
}

#[tokio::test]
#[serial]
async fn initialize_request_controls_history_window_v1() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    // 连接侧不预置任何协商值：门必须由真实 initialize 请求建立。
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, None).await;

    client
        .send_request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": { "_meta": { "peri.replay": true } },
            }),
        )
        .await
        .expect("standard initialize");
    let closed = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "around" }),
    )
    .await
    .unwrap_err();
    assert_eq!(closed.code, -32601);
    assert_eq!(
        closed.data.as_ref().unwrap()["reason"],
        "history_window_not_negotiated"
    );

    task.abort();

    // 新连接显式声明 V1：同一条真实 initialize → commit_initialize 链路放行。
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, None).await;
    client
        .send_request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "_meta": { "peri.historyWindowV1": true },
                },
            }),
        )
        .await
        .expect("V1 initialize");
    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "around", "limit": 1 }),
    )
    .await
    .expect("explicit V1 negotiation must open the window");
    assert!(opened["snapshotToken"].is_string());
    task.abort();
}

/// 记录型 transport：只收集 host 通知。
#[tokio::test]
#[serial]
async fn rejected_initial_load_and_warm_attach_keep_generation_and_ticket() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap();
    let generation = opened["sessionGeneration"].as_str().unwrap().to_owned();
    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().context_ownership = crate::host::ContextOwnership::HandedOff;
    }
    let rejected = client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .unwrap_err();
    assert_eq!(rejected.code, -32010);
    {
        let states = sessions.lock().await;
        let state = states.get(&sid).unwrap();
        assert_eq!(
            state.session_generation.to_string(),
            generation,
            "a rejected load must not advance sessionGeneration"
        );
        assert_eq!(
            state.context_ownership,
            crate::host::ContextOwnership::HandedOff
        );
    }

    // 旧 ticket 仍然有效：恢复 ownership 后继续用同一 ticket 翻页。
    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().context_ownership = crate::host::ContextOwnership::Resident;
    }
    let still_valid = page(
        &client,
        &sid,
        &opened["snapshotToken"],
        &opened["readTicket"],
        "beforeCursor",
        &opened["beforeCursor"],
        1,
    )
    .await
    .expect("rejected load must not revoke the old ticket");
    assert!(still_valid.get("items").is_some());

    // warm load 只推进本连接自己的 loadGeneration：共享 session generation 不变，
    // 另一连接已发布的固定读取视图继续有效。
    client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .expect("successful initial load");
    s9c_drain_notifications(&client).await;
    {
        let states = sessions.lock().await;
        assert_eq!(
            states.get(&sid).unwrap().session_generation.to_string(),
            generation,
            "a warm load must not advance the shared sessionGeneration"
        );
    }
    let still_valid = page(
        &client,
        &sid,
        &opened["snapshotToken"],
        &opened["readTicket"],
        "beforeCursor",
        &opened["beforeCursor"],
        1,
    )
    .await
    .expect("a warm load must not revoke another connection's ticket");
    assert!(still_valid.get("items").is_some());
    task.abort();
}

#[path = "history_page_window_test/connection_test.rs"]
mod connection_tests;

#[path = "history_page_window_test/inherited_test.rs"]
mod inherited_tests;

#[path = "history_page_window_test/audience_test.rs"]
mod audience_tests;

#[path = "history_page_window_test/read_gate_test.rs"]
mod read_gate_tests;

#[path = "history_page_window_test/oversize_overflow_test.rs"]
mod oversize_overflow_tests;

/// F04：窗口连接的 `session/load` 跳过历史回放，首屏由 open 提供。
#[path = "history_page_window_test/load_skip_replay_test.rs"]
mod load_skip_replay_tests;
