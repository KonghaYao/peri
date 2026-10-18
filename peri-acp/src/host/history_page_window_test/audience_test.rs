use super::*;
use peri_acp_types::history_window::HistoryEntryId;
use peri_acp_types::store::serialize_persisted_payload;
use peri_acp_types::system_reminder::*;

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
        fixture.sessions,
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
