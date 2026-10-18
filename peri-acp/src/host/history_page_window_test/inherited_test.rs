//! S7.4 / A26：inherited 冻结前缀的 bounds、page/seek、分块与父变更隔离。

use super::*;

use peri_acp_types::history_window::HistoryEntryId;
use peri_acp_types::store::{serialize_persisted_payload, InheritedContext};

pub(super) async fn set_inherited(
    tmp: &tempfile::TempDir,
    sid: &str,
    payloads: &[PersistedPayload],
    ancestor: &str,
) {
    let context = InheritedContext {
        payloads: payloads.to_vec(),
        flags: HashMap::new(),
    }
    .to_json()
    .unwrap();
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(tmp.path().join("threads.db")),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE threads SET inherited_context = ?1, parent_thread_id = ?2 WHERE id = ?3")
        .bind(context)
        .bind(ancestor)
        .bind(sid)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

fn inherited_source(snapshot_id: &str, ancestor: &str) -> Value {
    json!({
        "inherited": {
            "frozenSnapshotId": snapshot_id,
            "ancestorThreadId": ancestor,
        }
    })
}

#[tokio::test]
#[serial]
async fn inherited_open_page_seek_and_chunks_use_the_frozen_snapshot() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let inherited = vec![
        PersistedPayload::Message(BaseMessage::human("祖先一：冻结正文")),
        PersistedPayload::Message(BaseMessage::ai("ancestor two")),
        PersistedPayload::Message(BaseMessage::human("祖先三：不可被父会话污染")),
        PersistedPayload::Message(BaseMessage::ai("ancestor four")),
    ];
    set_inherited(&tmp, &sid, &inherited, "parent-session").await;
    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let own = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap();
    assert_eq!(own["inheritedUnsupported"], false);
    let inherited_meta = &own["inherited"];
    assert_eq!(inherited_meta["entryCount"], inherited.len());
    assert_eq!(inherited_meta["ancestorThreadId"], "parent-session");
    let snapshot_id = inherited_meta["snapshotId"].as_str().unwrap().to_owned();
    let anchor_entry = HistoryEntryId::new(&sid, inherited[1].id()).encode();
    let source = inherited_source(&snapshot_id, "parent-session");

    let opened = open(
        &client,
        &sid,
        json!({
            "kind": "open",
            "anchor": { "kind": "entry", "entryId": anchor_entry, "source": source },
            "direction": "around",
            "beforeLimit": 2,
            "afterLimit": 1
        }),
    )
    .await
    .expect("open inherited around an entry");
    let items = opened["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(
        items
            .iter()
            .map(|item| item["entryId"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            HistoryEntryId::new(&sid, inherited[0].id()).encode(),
            HistoryEntryId::new(&sid, inherited[1].id()).encode(),
            HistoryEntryId::new(&sid, inherited[2].id()).encode(),
        ]
    );
    for item in items {
        assert!(
            item["position"].is_null(),
            "inherited entries have no own position"
        );
        assert_eq!(item["source"]["inherited"]["frozenSnapshotId"], snapshot_id);
    }
    assert_eq!(opened["committedTail"], Value::Null);
    assert!(opened["beforeCursor"].is_null());
    assert!(opened["afterCursor"].is_string());
    let later = page(
        &client,
        &sid,
        &opened["snapshotToken"],
        &opened["readTicket"],
        "afterCursor",
        &opened["afterCursor"],
        1,
    )
    .await
    .expect("inherited after page");
    assert_eq!(
        later["items"][0]["entryId"].as_str().unwrap(),
        HistoryEntryId::new(&sid, inherited[3].id()).encode()
    );

    let non_descriptor = client
        .send_request(
            "session/history/body",
            json!({
                "sessionId": sid,
                "snapshotToken": opened["snapshotToken"],
                "readTicket": opened["readTicket"],
                "entryId": HistoryEntryId::new(&sid, inherited[0].id()).encode(),
                "offset": 0,
                "maxBytes": 7
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(
        non_descriptor.code, -32016,
        "only exposed oversize descriptors are readable"
    );

    // A26：段释放后按同一 entryId 重新 seek，内容逐字节一致。
    let first_seek = client
        .send_request(
            "session/load",
            json!({
                "sessionId": sid,
                "history": {
                    "kind": "seek",
                    "snapshotToken": opened["snapshotToken"],
                    "readTicket": opened["readTicket"],
                    "anchor": { "entryId": anchor_entry, "source": source },
                    "beforeLimit": 1,
                    "afterLimit": 1
                }
            }),
        )
        .await
        .expect("inherited seek");
    let second_seek = client
        .send_request(
            "session/load",
            json!({
                "sessionId": sid,
                "history": {
                    "kind": "seek",
                    "snapshotToken": opened["snapshotToken"],
                    "readTicket": opened["readTicket"],
                    "anchor": { "entryId": anchor_entry, "source": source },
                    "beforeLimit": 1,
                    "afterLimit": 1
                }
            }),
        )
        .await
        .expect("inherited re-seek");
    assert_eq!(first_seek["items"], second_seek["items"]);

    // 父/自有侧变化后，同一 inherited entry 的内容仍不变。
    fixture
        .cfg
        .session_resources
        .append_history(
            &sid,
            &[PersistedPayload::Message(BaseMessage::human("own later"))],
        )
        .await
        .unwrap();
    fixture
        .cfg
        .session_resources
        .apply_compaction(
            &sid,
            &CompactionChange {
                flag_updates: Vec::new(),
                appended_messages: vec![BaseMessage::ai("own compact summary")],
            },
        )
        .await
        .unwrap();
    let after_change = client
        .send_request(
            "session/load",
            json!({
                "sessionId": sid,
                "history": {
                    "kind": "seek",
                    "snapshotToken": opened["snapshotToken"],
                    "readTicket": opened["readTicket"],
                    "anchor": { "entryId": anchor_entry, "source": source },
                    "beforeLimit": 1,
                    "afterLimit": 1
                }
            }),
        )
        .await
        .expect("inherited seek after own changes");
    let text_after = after_change["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["entryId"] == anchor_entry)
        .and_then(|item| item["subItems"][0]["update"]["content"]["text"].as_str())
        .expect("inherited anchor text");
    assert_eq!(text_after, "ancestor two");

    // inherited 超大条目 descriptor + 有界分块（UTF-8 跨块）。
    let oversized = open(
        &client,
        &sid,
        json!({
            "kind": "open",
            "anchor": { "kind": "entry", "entryId": anchor_entry, "source": source },
            "direction": "before",
            "beforeLimit": 1,
            "byteBudget": 1
        }),
    )
    .await
    .expect("inherited oversize open");
    let item = &oversized["items"][0];
    let body = item.get("body").expect("inherited oversize descriptor");
    assert_eq!(body["snapshotToken"], oversized["snapshotToken"]);
    let mut offset = 0u64;
    let mut collected = Vec::new();
    loop {
        let chunk = client
            .send_request(
                "session/history/body",
                json!({
                    "sessionId": sid,
                    "snapshotToken": oversized["snapshotToken"],
                    "readTicket": oversized["readTicket"],
                    "entryId": anchor_entry,
                    "offset": offset,
                    "maxBytes": 7
                }),
            )
            .await
            .expect("inherited body chunk");
        let bytes = chunk["bytes"].as_array().unwrap();
        collected.extend(bytes.iter().map(|byte| byte.as_u64().unwrap() as u8));
        match chunk["nextOffset"].as_u64() {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(
        collected,
        serialize_persisted_payload(&inherited[1])
            .unwrap()
            .into_bytes()
    );
    String::from_utf8(collected).expect("inherited chunk stream preserves UTF-8");

    // 新 load/rewind 的 generation/structure 失效语义同样作用于 inherited ticket。
    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().refresh_session_generation();
    }
    let stale = client
        .send_request(
            "session/load",
            json!({
                "sessionId": sid,
                "history": {
                    "kind": "seek",
                    "snapshotToken": oversized["snapshotToken"],
                    "readTicket": oversized["readTicket"],
                    "anchor": { "entryId": anchor_entry, "source": source },
                    "beforeLimit": 1,
                    "afterLimit": 1
                }
            }),
        )
        .await
        .unwrap_err();
    assert!(stale.code == -32012 || stale.code == -32014);
    task.abort();
}

#[tokio::test]
#[serial]
async fn inherited_absence_and_ancestor_mismatch_are_explicit() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let own = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap();
    assert!(own["inherited"].is_null());
    assert_eq!(own["inheritedUnsupported"], false);

    let source = inherited_source("inherited:missing", "no-such-ancestor");
    let anchor = HistoryEntryId::new(&sid, MessageId::new()).encode();
    let error = open(
        &client,
        &sid,
        json!({
            "kind": "open",
            "anchor": { "kind": "entry", "entryId": anchor, "source": source },
            "direction": "before",
            "limit": 1
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32016);
    task.abort();
}

#[tokio::test]
#[serial]
async fn inherited_ancestor_mismatch_is_rejected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let inherited = vec![PersistedPayload::Message(BaseMessage::human("ancestor"))];
    set_inherited(&tmp, &sid, &inherited, "parent-session").await;
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;
    let own = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap();
    let snapshot_id = own["inherited"]["snapshotId"].as_str().unwrap();
    let anchor = HistoryEntryId::new(&sid, inherited[0].id()).encode();
    let wrong_ancestor = inherited_source(snapshot_id, "wrong-parent");
    let error = open(
        &client,
        &sid,
        json!({
            "kind": "open",
            "anchor": { "kind": "entry", "entryId": anchor, "source": wrong_ancestor },
            "direction": "before",
            "limit": 1
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32015);
    task.abort();
}

#[tokio::test]
#[serial]
async fn rewind_invalidates_old_inherited_ticket_but_frozen_prefix_remains_readable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let inherited = vec![PersistedPayload::Message(BaseMessage::human(
        "frozen ancestor",
    ))];
    set_inherited(&tmp, &sid, &inherited, "parent-session").await;
    let own = fixture.own_ids().await;
    let target = own
        .last()
        .expect("fixture own history")
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
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap();
    let snapshot_id = opened["inherited"]["snapshotId"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = inherited_source(&snapshot_id, "parent-session");
    let anchor = HistoryEntryId::new(&sid, inherited[0].id()).encode();
    let inherited_open = open(
        &client,
        &sid,
        json!({
            "kind": "open",
            "anchor": { "kind": "entry", "entryId": anchor, "source": source },
            "direction": "around",
            "beforeLimit": 1,
            "afterLimit": 1
        }),
    )
    .await
    .unwrap();

    let preview = client
        .send_request(
            "session/rewind-preview",
            json!({ "sessionId": sid, "target_message_id": target }),
        )
        .await
        .unwrap();
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
        .expect("rewind");
    s9c_drain_notifications(&client).await;

    let stale = client
        .send_request(
            "session/load",
            json!({
                "sessionId": sid,
                "history": {
                    "kind": "seek",
                    "snapshotToken": inherited_open["snapshotToken"],
                    "readTicket": inherited_open["readTicket"],
                    "anchor": { "entryId": anchor, "source": source },
                    "beforeLimit": 1,
                    "afterLimit": 1
                }
            }),
        )
        .await
        .unwrap_err();
    assert!(stale.code == -32012 || stale.code == -32014);

    let reopened = open(
        &client,
        &sid,
        json!({
            "kind": "open",
            "anchor": { "kind": "entry", "entryId": anchor, "source": source },
            "direction": "around",
            "beforeLimit": 1,
            "afterLimit": 1
        }),
    )
    .await
    .expect("frozen inherited prefix remains readable after rewind");
    assert_eq!(reopened["items"].as_array().unwrap().len(), 1);
    task.abort();
}

#[tokio::test]
#[serial]
async fn close_invalidates_old_inherited_ticket() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let inherited = vec![PersistedPayload::Message(BaseMessage::human(
        "frozen ancestor",
    ))];
    set_inherited(&tmp, &sid, &inherited, "parent-session").await;
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;
    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap();
    let snapshot_id = opened["inherited"]["snapshotId"]
        .as_str()
        .unwrap()
        .to_owned();
    let source = inherited_source(&snapshot_id, "parent-session");
    let anchor = HistoryEntryId::new(&sid, inherited[0].id()).encode();
    let inherited_open = open(
        &client,
        &sid,
        json!({
            "kind": "open",
            "anchor": { "kind": "entry", "entryId": anchor, "source": source },
            "direction": "around",
            "beforeLimit": 1,
            "afterLimit": 1
        }),
    )
    .await
    .unwrap();
    client
        .send_request("session/close", json!({ "sessionId": sid }))
        .await
        .expect("close");
    s9c_drain_notifications(&client).await;
    let stale = client
        .send_request(
            "session/load",
            json!({
                "sessionId": sid,
                "history": {
                    "kind": "seek",
                    "snapshotToken": inherited_open["snapshotToken"],
                    "readTicket": inherited_open["readTicket"],
                    "anchor": { "entryId": anchor, "source": source },
                    "beforeLimit": 1,
                    "afterLimit": 1
                }
            }),
        )
        .await
        .unwrap_err();
    assert!(
        stale.code == -32010 || stale.code == -32012 || stale.code == -32602,
        "closed session must not serve the old inherited ticket: {stale:?}"
    );
    task.abort();
}
