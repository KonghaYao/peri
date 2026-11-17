//! `peri.historyWindowV1` 的 wire 级行为：协商、open/page/seek、失效、fixed tail、
//! oversize descriptor 与正文分块。
//!
//! 走真实 `ServerLoop` + MPSC transport + 真实 SQLite 门面（父模块夹具），
//! 覆盖能力门、sessions 短锁二次校验与响应形状。

use crate::host::requests::tests::history_page_wire_tests::PageFixture;
use crate::transport::types::AcpError;
use crate::transport::AcpTransport;
use peri_acp_types::messages::BaseMessage;

use peri_acp_types::store::PersistedPayload;
use peri_acp_types::PeriCaps;
use serde_json::json;
use serde_json::Value;
use serial_test::serial;
use std::sync::Arc;

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
    let (client, _sessions, task) =
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            cfg,
            fixture.sessions,
            Some(window_caps()),
        )
        .await;

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
    let (client, _sessions, task) =
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            cfg,
            fixture.sessions,
            Some(window_caps()),
        )
        .await;

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
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            cfg,
            fixture.sessions,
            Some(PeriCaps::default()),
        )
        .await;

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
        crate::host::requests::tests::history_page_wire_tests::s9c_drain_notifications(&client)
            .await
            .is_empty(),
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
    let (client, _sessions, task) =
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            cfg,
            fixture.sessions,
            Some(window_caps()),
        )
        .await;

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
    let (client, sessions, task) =
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            cfg,
            fixture.sessions,
            Some(window_caps()),
        )
        .await;

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
    let (client, _sessions, task) =
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            cfg,
            fixture.sessions,
            Some(window_caps()),
        )
        .await;

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

include!("history_page_window_test_part2_test.rs");
