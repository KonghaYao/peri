use super::*;
use peri_acp_types::history_window::HistoryEntryId;
use peri_acp_types::store::serialize_persisted_payload;
use peri_acp_types::system_reminder::*;

extern "C" {
    fn sqlite3_memory_highwater(reset: i32) -> i64;
    fn sqlite3_memory_used() -> i64;
}

fn sqlite_peak_reset() -> i64 {
    // SAFETY: SQLite 全局分配计数；分配测量用例串行执行（--test-threads=1）。
    let before = unsafe { sqlite3_memory_used() };
    unsafe {
        sqlite3_memory_highwater(1);
    }
    before
}

fn sqlite_peak_extra(before: i64) -> i64 {
    // SAFETY: 同上；只读取计数器。
    let peak = unsafe { sqlite3_memory_highwater(0) };
    (peak - before).max(0)
}

async fn visibility_scans(fixture: &PageFixture, sid: &str) -> Option<usize> {
    let states = fixture.sessions.lock().await;
    states
        .get(sid)
        .and_then(|state| state.history_window.visibility_full_scans())
}

fn hidden_reminder() -> PersistedPayload {
    PersistedPayload::SystemReminder {
        id: MessageId::new(),
        reminder: TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Security,
                source: ReminderSource("audience_test".into()),
                kind: "notice".into(),
                severity: ReminderSeverity::Warning,
                delivery: ReminderDelivery::Required,
                audiences: ReminderAudiences(vec![ReminderAudience::Model]),
                body: "hidden_model_body".repeat(100),
                summary: None,
                metadata: json!({}),
            })
            .unwrap(),
    }
}

#[tokio::test]
#[serial]
async fn tiny_budget_does_not_issue_hidden_body_descriptors_own_or_inherited() {
    for inherited in [false, true] {
        for payload in [
            hidden_reminder(),
            PersistedPayload::Message(BaseMessage::system("hidden_system_body".repeat(100))),
        ] {
            let tmp = tempfile::TempDir::new().unwrap();
            let fixture = PageFixture::new(&tmp).await;
            let sid = fixture.sid.clone();
            let source = if inherited {
                super::inherited_tests::set_inherited(
                    &tmp,
                    &sid,
                    std::slice::from_ref(&payload),
                    "parent",
                )
                .await;
                let bounds = fixture
                    .cfg
                    .session_resources
                    .history_window_bounds(&sid)
                    .await
                    .unwrap();
                json!({"inherited":{"frozenSnapshotId":bounds.inherited.snapshot().unwrap().snapshot_id,"ancestorThreadId":"parent"}})
            } else {
                fixture
                    .cfg
                    .session_resources
                    .append_history(&sid, std::slice::from_ref(&payload))
                    .await
                    .unwrap();
                json!("own")
            };
            let entry = HistoryEntryId::new(&sid, payload.id()).encode();
            let (client, _, task) = spawn_loop(
                Arc::clone(&fixture.cfg),
                fixture.sessions,
                Some(window_caps()),
            )
            .await;
            for budget in [512 * 1024, 1] {
                let opened = open(
                    &client,
                    &sid,
                    json!({
                        "kind":"open", "direction":"before", "limit":1, "byteBudget":budget,
                        "anchor":{"kind":"entry", "entryId":entry, "source":source}
                    }),
                )
                .await
                .unwrap();
                assert_eq!(opened["items"], json!([]));
                assert!(!opened.to_string().contains("hidden_model_body"));
                assert!(!opened.to_string().contains("hidden_system_body"));
                assert!(!opened.to_string().contains("totalBytes"));
                let denied = client.send_request("session/history/body", json!({
                    "sessionId":sid, "entryId":entry, "source":source,
                    "snapshotToken":opened["snapshotToken"], "readTicket":opened["readTicket"],
                    "offset":0, "maxBytes":1024
                })).await.unwrap_err();
                assert_eq!(denied.code, -32016);
                let wire = serde_json::to_value(&denied).unwrap();
                for field in ["bytes", "body", "readTicket", "totalBytes"] {
                    assert!(!wire.to_string().contains(field));
                }
                assert!(!wire.to_string().contains("hidden_model_body"));
                assert!(!wire.to_string().contains("hidden_system_body"));
            }
            task.abort();
        }
    }
}

#[tokio::test]
#[serial]
async fn body_rechecks_visibility_even_after_descriptor_was_issued() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let visible = BaseMessage::human("previously visible");
    let id = visible.id();
    fixture
        .cfg
        .session_resources
        .append_history(&sid, &[PersistedPayload::Message(visible)])
        .await
        .unwrap();
    let (client, _, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        Some(window_caps()),
    )
    .await;
    let opened = open(
        &client,
        &sid,
        json!({"kind":"open","direction":"before","limit":1,"byteBudget":1}),
    )
    .await
    .unwrap();
    assert!(opened["items"][0]["body"].is_object());
    assert_eq!(
        visibility_scans(&fixture, &sid).await,
        Some(1),
        "open 的首次完整解析只发生一次"
    );
    // 模拟旧票据登记后持久内容改变，证明 body 不仅依赖 descriptor 集合。
    let hidden = PersistedPayload::Message(BaseMessage::System {
        id,
        content: "hidden_changed_body".into(),
    });
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(tmp.path().join("threads.db")),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE messages SET content = ?1 WHERE message_id = ?2")
        .bind(serialize_persisted_payload(&hidden).unwrap())
        .bind(id.as_uuid().to_string())
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let denied = client.send_request("session/history/body", json!({
        "sessionId":sid,"entryId":opened["items"][0]["entryId"],
        "snapshotToken":opened["snapshotToken"],"readTicket":opened["readTicket"],"offset":0,"maxBytes":1024
    })).await.unwrap_err();
    assert_eq!(denied.code, -32016);
    let wire = serde_json::to_value(&denied).unwrap().to_string();
    assert!(!wire.contains("hidden_changed_body"));
    assert!(!wire.contains("bytes"));
    assert!(!wire.contains("totalBytes"));
    assert_eq!(
        visibility_scans(&fixture, &sid).await,
        Some(2),
        "前缀摘要变化必须触发一次新的完整解析"
    );
    task.abort();
}

#[tokio::test]
#[serial]
async fn visible_oversize_reminder_and_tool_body_remain_complete() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let PersistedPayload::SystemReminder { id, reminder } = hidden_reminder() else {
        unreachable!()
    };
    let mut reminder = reminder.as_reminder().clone();
    reminder.audiences = ReminderAudiences(vec![ReminderAudience::Tui]);
    let payloads = [
        PersistedPayload::SystemReminder {
            id,
            reminder: TrustedSystemReminderFactory::for_producer()
                .construct(reminder)
                .unwrap(),
        },
        PersistedPayload::Message(BaseMessage::tool_error(
            "large-result",
            format!("{}END_OF_DIFF", "结果".repeat(100_000)),
        )),
    ];
    fixture
        .cfg
        .session_resources
        .append_history(&sid, &payloads)
        .await
        .unwrap();
    let (client, _, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        fixture.sessions,
        Some(window_caps()),
    )
    .await;
    for payload in payloads {
        let entry = HistoryEntryId::new(&sid, payload.id()).encode();
        let opened = open(
            &client,
            &sid,
            json!({
                "kind":"open","direction":"before","limit":1,"byteBudget":1,
                "anchor":{"kind":"entry","entryId":entry,"source":"own"}
            }),
        )
        .await
        .unwrap();
        assert!(opened["items"][0]["body"].is_object());
        let mut collected = Vec::<u8>::new();
        loop {
            let chunk = client
                .send_request(
                    "session/history/body",
                    json!({
                        "sessionId":sid,"entryId":entry,"source":"own",
                        "snapshotToken":opened["snapshotToken"],"readTicket":opened["readTicket"],
                        "offset":collected.len(),"maxBytes":65536
                    }),
                )
                .await
                .unwrap();
            collected.extend(serde_json::from_value::<Vec<u8>>(chunk["bytes"].clone()).unwrap());
            if chunk["complete"] == true {
                break;
            }
            assert_eq!(chunk["nextOffset"], collected.len());
        }
        assert_eq!(
            collected,
            serialize_persisted_payload(&payload).unwrap().as_bytes()
        );
    }
    task.abort();
}

#[tokio::test]
#[serial]
async fn page_and_seek_do_not_issue_hidden_oversize_descriptors() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let hidden =
        PersistedPayload::Message(BaseMessage::system("hidden_large_system".repeat(40_000)));
    fixture
        .cfg
        .session_resources
        .append_history(
            &sid,
            &[
                hidden.clone(),
                PersistedPayload::Message(BaseMessage::human("visible tail")),
            ],
        )
        .await
        .unwrap();
    let (client, _, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        fixture.sessions,
        Some(window_caps()),
    )
    .await;
    let opened = open(
        &client,
        &sid,
        json!({"kind":"open","direction":"before","limit":1}),
    )
    .await
    .unwrap();
    let fetched = page(
        &client,
        &sid,
        &opened["snapshotToken"],
        &opened["readTicket"],
        "beforeCursor",
        &opened["beforeCursor"],
        1,
    )
    .await
    .unwrap();
    assert_eq!(fetched["items"], json!([]));
    assert!(fetched["beforeCursor"].is_string(), "隐藏条目仍推进游标");
    let sought = open(
        &client,
        &sid,
        json!({
            "kind":"seek","snapshotToken":opened["snapshotToken"],"readTicket":opened["readTicket"],
            "anchor":{"entryId":HistoryEntryId::new(&sid, hidden.id()).encode(),"source":"own"},
            "beforeLimit":1,"afterLimit":1
        }),
    )
    .await
    .unwrap();
    assert!(sought["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| { item["entryId"] != HistoryEntryId::new(&sid, hidden.id()).encode() }));
    assert!(!fetched.to_string().contains("totalBytes"));
    for response in [fetched, sought] {
        let wire = response.to_string();
        assert!(!wire.contains("hidden_large_system"));
    }
    task.abort();
}

/// 可见正文跨多个 64 KiB 块：open 的首次完整解析后，后续块请求只读前缀并命中缓存。
#[tokio::test]
#[serial]
async fn visibility_cache_survives_multiple_body_chunks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let payload =
        PersistedPayload::Message(BaseMessage::human("multi chunk visible ".repeat(20_000)));
    let expected = serialize_persisted_payload(&payload).unwrap();
    assert!(expected.len() > 4 * 64 * 1024);
    fixture
        .cfg
        .session_resources
        .append_history(&sid, std::slice::from_ref(&payload))
        .await
        .unwrap();
    let (client, _, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        Some(window_caps()),
    )
    .await;
    let opened = open(
        &client,
        &sid,
        json!({"kind":"open","direction":"before","limit":1,"byteBudget":1}),
    )
    .await
    .unwrap();
    let entry = opened["items"][0]["entryId"].clone();
    assert!(opened["items"][0]["body"].is_object());
    assert_eq!(visibility_scans(&fixture, &sid).await, Some(1));

    let mut collected = Vec::new();
    while collected.len() < expected.len() {
        let chunk = client
            .send_request(
                "session/history/body",
                json!({
                    "sessionId":sid,"entryId":entry,"source":"own",
                    "snapshotToken":opened["snapshotToken"],"readTicket":opened["readTicket"],
                    "offset":collected.len(),"maxBytes":65536
                }),
            )
            .await
            .unwrap();
        let bytes = serde_json::from_value::<Vec<u8>>(chunk["bytes"].clone()).unwrap();
        assert!(bytes.len() <= 65536);
        collected.extend_from_slice(&bytes);
        assert_eq!(
            visibility_scans(&fixture, &sid).await,
            Some(1),
            "每个分块请求都不得重扫全文"
        );
        if chunk["complete"] == true {
            break;
        }
    }
    assert_eq!(collected, expected.into_bytes());
    task.abort();
}

/// 16 MiB 正文、多次分块请求：完整解析只发生一次，每个请求的 SQLite 额外分配有界。
///
/// ```text
/// ./scripts/cargo-rmcp-patched.sh test --locked -p peri-acp --lib -- \
///     body_chunks_do_not_rescan_visibility_after_first_check --ignored --nocapture --test-threads=1
/// ```
#[tokio::test]
#[serial]
#[ignore = "16 MiB 夹具与分配测量；显式运行，见用例文档"]
async fn body_chunks_do_not_rescan_visibility_after_first_check() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let payload = PersistedPayload::Message(BaseMessage::human("Z".repeat(16 * 1024 * 1024)));
    let expected = serialize_persisted_payload(&payload).unwrap();
    fixture
        .cfg
        .session_resources
        .append_history(&sid, std::slice::from_ref(&payload))
        .await
        .unwrap();
    let (client, _, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        Some(window_caps()),
    )
    .await;
    let opened = open(
        &client,
        &sid,
        json!({"kind":"open","direction":"before","limit":1,"byteBudget":1}),
    )
    .await
    .unwrap();
    let entry = opened["items"][0]["entryId"].clone();
    assert_eq!(visibility_scans(&fixture, &sid).await, Some(1));

    for offset in [0usize, 64 * 1024, 128 * 1024] {
        let before = sqlite_peak_reset();
        let chunk = client
            .send_request(
                "session/history/body",
                json!({
                    "sessionId":sid,"entryId":entry,"source":"own",
                    "snapshotToken":opened["snapshotToken"],"readTicket":opened["readTicket"],
                    "offset":offset,"maxBytes":65536
                }),
            )
            .await
            .unwrap();
        let extra = sqlite_peak_extra(before);
        let bytes = serde_json::from_value::<Vec<u8>>(chunk["bytes"].clone()).unwrap();
        assert_eq!(bytes, expected.as_bytes()[offset..offset + 65536]);
        println!(
            "body_chunk offset={offset} sqlite_extra={} MiB",
            extra as f64 / (1024.0 * 1024.0)
        );
        assert!(
            extra < 8 * 1024 * 1024,
            "body chunk must stay bounded, got {extra} bytes"
        );
        assert_eq!(
            visibility_scans(&fixture, &sid).await,
            Some(1),
            "分块请求不得重新完整解析"
        );
    }
    task.abort();
}

/// 重新 open 会替换 ticket 及其可见性缓存；旧结论不能跨票据存活。
#[tokio::test]
#[serial]
async fn ticket_rotation_clears_visibility_cache() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let payload = PersistedPayload::Message(BaseMessage::human("rotate ".repeat(20_000)));
    fixture
        .cfg
        .session_resources
        .append_history(&sid, std::slice::from_ref(&payload))
        .await
        .unwrap();
    let (client, _, task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        Arc::clone(&fixture.sessions),
        Some(window_caps()),
    )
    .await;
    let first = open(
        &client,
        &sid,
        json!({"kind":"open","direction":"before","limit":1,"byteBudget":1}),
    )
    .await
    .unwrap();
    assert_eq!(visibility_scans(&fixture, &sid).await, Some(1));
    let entry = first["items"][0]["entryId"].clone();
    client
        .send_request(
            "session/history/body",
            json!({
                "sessionId":sid,"entryId":entry,"source":"own",
                "snapshotToken":first["snapshotToken"],"readTicket":first["readTicket"],
                "offset":0,"maxBytes":65536
            }),
        )
        .await
        .unwrap();
    assert_eq!(visibility_scans(&fixture, &sid).await, Some(1));

    let second = open(
        &client,
        &sid,
        json!({"kind":"open","direction":"before","limit":1,"byteBudget":1}),
    )
    .await
    .unwrap();
    assert_ne!(second["readTicket"], first["readTicket"]);
    assert_eq!(
        visibility_scans(&fixture, &sid).await,
        Some(1),
        "新票据从零开始计数，不复用旧票据结论"
    );
    task.abort();
}
