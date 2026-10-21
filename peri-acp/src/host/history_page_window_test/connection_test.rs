//! V1 连接绑定、提交通知与 body 快照边界（从父测试模块拆出）。

use super::*;

/// 记录型 transport：只收集 host 通知。
struct RecordingTransport {
    notifications: std::sync::Mutex<Vec<(String, Value)>>,
}

#[async_trait::async_trait]
impl crate::transport::AcpTransport for RecordingTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        Ok(json!({}))
    }
    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.notifications
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        Ok(())
    }
    async fn recv(&self) -> Option<crate::transport::types::IncomingMessage> {
        None
    }
    async fn send_response(
        &self,
        _id: crate::transport::types::RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

struct RecordingSink {
    recording: std::sync::Arc<RecordingTransport>,
}

#[async_trait::async_trait]
impl crate::host::history_window::WindowNotificationSink for RecordingSink {
    async fn send(&self, _session_id: &str, payload: Value) -> Result<(), AcpError> {
        self.recording
            .send_notification("peri/history_committed", payload)
            .await
    }
}

#[tokio::test]
#[serial]
async fn history_commit_notification_is_gated_by_an_open_window_and_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let recording = std::sync::Arc::new(RecordingTransport {
        notifications: std::sync::Mutex::new(Vec::new()),
    });

    // 没有活动窗口：不发送私有通知。
    crate::host::history_window::notify_commit_if_open(&fixture.sessions, &sid).await;
    assert!(recording.notifications.lock().unwrap().is_empty());

    let request = peri_acp_types::history_window::HistoryWindowRequest::decode(&json!({
        "kind": "open", "direction": "before", "limit": 1
    }))
    .unwrap();
    let connection = crate::host::history_window::WindowConnection::new(
        "connection-a",
        std::sync::Arc::new(RecordingSink {
            recording: recording.clone(),
        }),
    );
    crate::host::history_window::handle(
        &fixture.cfg,
        &fixture.sessions,
        &window_caps(),
        &sid,
        request,
        &connection,
    )
    .await
    .expect("window open");

    let checkpoint = peri_acp_types::history_window::HistoryCheckpoint {
        checkpoint_id: format!("{sid}:summary"),
        summary_entry_id: None,
        position: 4,
    };
    let tail = peri_acp_types::history_window::HistoryCommittedTail::new(
        &sid,
        peri_acp_types::messages::MessageId::new(),
        4,
    );
    {
        let mut states = fixture.sessions.lock().await;
        let state = states.get_mut(&sid).unwrap();
        crate::host::history_window::publish_commit(state, Some(&checkpoint), Some(&tail));
    }

    crate::host::history_window::notify_commit_if_open(&fixture.sessions, &sid).await;
    crate::host::history_window::notify_commit_if_open(&fixture.sessions, &sid).await;
    let notifications = recording.notifications.lock().unwrap();
    assert_eq!(notifications.len(), 1, "same checkpoint must notify once");
    assert_eq!(notifications[0].0, "peri/history_committed");
    assert_eq!(notifications[0].1["checkpointId"], checkpoint.checkpoint_id);
    assert!(notifications[0].1["committedTail"].is_string());
}

#[tokio::test]
#[serial]
async fn open_after_durable_full_reads_checkpoint_from_storage() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let ids = fixture.own_ids().await;
    let summary = BaseMessage::ai("durable compact summary");
    let summary_id = summary.id();
    fixture
        .cfg
        .session_resources
        .apply_compaction(
            &sid,
            &CompactionChange {
                flag_updates: ids
                    .iter()
                    .take(2)
                    .map(|id| {
                        (
                            *id,
                            MessageFlags {
                                excluded: true,
                                projection: Some(
                                    peri_acp_types::projection::MessageProjectionDirective {
                                        policy_version: 1,
                                        entries: Vec::new(),
                                        superseded_by: Some(summary_id),
                                        ..Default::default()
                                    },
                                ),
                                ..Default::default()
                            },
                        )
                    })
                    .collect(),
                appended_messages: vec![summary],
            },
        )
        .await
        .expect("durable full compaction");

    let expected = peri_acp_types::history_window::HistoryEntryId::new(&sid, summary_id).encode();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;
    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .expect("open after compact");
    assert_eq!(
        opened["checkpointId"].as_str(),
        Some(expected.as_str()),
        "checkpointId must be rebuilt from durable superseded_by rows"
    );
    assert!(opened["committedTail"].is_string());
    task.abort();
}

#[tokio::test]
#[serial]
async fn tickets_and_commit_notifications_are_bound_to_the_opening_connection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client_a, _sessions_a, task_a) = spawn_loop(
        Arc::clone(&cfg),
        fixture.sessions.clone(),
        Some(window_caps()),
    )
    .await;
    let (client_b, _sessions_b, task_b) = spawn_loop(
        Arc::clone(&cfg),
        fixture.sessions.clone(),
        Some(window_caps()),
    )
    .await;

    let opened = open(
        &client_a,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .expect("connection A opens the window");

    let foreign = page(
        &client_b,
        &sid,
        &opened["snapshotToken"],
        &opened["readTicket"],
        "beforeCursor",
        &opened["beforeCursor"],
        1,
    )
    .await
    .unwrap_err();
    assert_eq!(foreign.code, -32014);

    let checkpoint = peri_acp_types::history_window::HistoryCheckpoint {
        checkpoint_id: format!("{sid}:summary"),
        summary_entry_id: None,
        position: 4,
    };
    let tail = peri_acp_types::history_window::HistoryCommittedTail::new(
        &sid,
        peri_acp_types::messages::MessageId::new(),
        4,
    );
    {
        let mut states = fixture.sessions.lock().await;
        let state = states.get_mut(&sid).unwrap();
        crate::host::history_window::publish_commit(state, Some(&checkpoint), Some(&tail));
    }
    crate::host::history_window::notify_commit_if_open(&fixture.sessions, &sid).await;

    let notification = tokio::time::timeout(std::time::Duration::from_secs(2), client_a.recv())
        .await
        .expect("A must receive the commit notification")
        .expect("A transport remains open");
    match notification {
        crate::transport::types::IncomingMessage::Notification { method, params } => {
            assert_eq!(method, "peri/history_committed");
            assert_eq!(params["sessionId"], sid);
            assert_eq!(params["checkpointId"], checkpoint.checkpoint_id);
        }
        other => panic!("expected commit notification, got {other:?}"),
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), client_b.recv())
            .await
            .is_err(),
        "B must not receive the A-bound commit notification"
    );

    task_a.abort();
    task_b.abort();
}

#[tokio::test]
#[serial]
async fn body_rejects_non_oversize_and_post_snapshot_entries() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let normal = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap();
    let normal_entry = normal["items"][0]["entryId"].as_str().unwrap();
    let rejected = client
        .send_request(
            "session/history/body",
            json!({
                "sessionId": sid,
                "snapshotToken": normal["snapshotToken"],
                "readTicket": normal["readTicket"],
                "entryId": normal_entry,
                "offset": 0,
                "maxBytes": 7
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(rejected.code, -32016);

    let oversized = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1, "byteBudget": 1 }),
    )
    .await
    .unwrap();
    let descriptor_entry = oversized["items"][0]["entryId"]
        .as_str()
        .unwrap()
        .to_owned();
    let later = PersistedPayload::Message(BaseMessage::human("append after body ticket"));
    fixture
        .cfg
        .session_resources
        .append_history(&sid, std::slice::from_ref(&later))
        .await
        .unwrap();
    let later_entry =
        peri_acp_types::history_window::HistoryEntryId::new(&sid, later.id()).encode();
    let post_snapshot = client
        .send_request(
            "session/history/body",
            json!({
                "sessionId": sid,
                "snapshotToken": oversized["snapshotToken"],
                "readTicket": oversized["readTicket"],
                "entryId": later_entry,
                "offset": 0,
                "maxBytes": 7
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(
        post_snapshot.code, -32016,
        "an entry appended after the snapshot must not be readable by the old ticket"
    );

    for invalid in [
        json!({
            "sessionId": sid, "snapshotToken": oversized["snapshotToken"],
            "readTicket": oversized["readTicket"], "entryId": descriptor_entry,
            "offset": "0", "maxBytes": 7
        }),
        json!({
            "sessionId": sid, "snapshotToken": oversized["snapshotToken"],
            "readTicket": oversized["readTicket"], "entryId": descriptor_entry,
            "offset": 0, "maxBytes": "7"
        }),
    ] {
        let error = client
            .send_request("session/history/body", invalid)
            .await
            .unwrap_err();
        assert_eq!(error.code, -32602);
    }

    let first = client
        .send_request(
            "session/history/body",
            json!({
                "sessionId": sid,
                "snapshotToken": oversized["snapshotToken"],
                "readTicket": oversized["readTicket"],
                "entryId": descriptor_entry,
                "offset": 0,
                "maxBytes": 7
            }),
        )
        .await
        .expect("registered oversize descriptor remains readable");
    assert!(!first["bytes"].as_array().unwrap().is_empty());
    task.abort();
}

#[tokio::test]
#[serial]
async fn failed_open_keeps_the_old_ticket_usable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    // 旧窗口使用小预算，拿到一个 descriptor 以便同时验证 body。
    let old = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1, "byteBudget": 1 }),
    )
    .await
    .unwrap();
    let old_entry = old["items"][0]["entryId"].as_str().unwrap().to_owned();

    // 失败的 open：非法预算在准备/发布前被拒绝。
    let failed = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "byteBudget": 0 }),
    )
    .await
    .unwrap_err();
    assert_eq!(failed.code, -32602);

    // 旧 ticket/snapshot 仍然完全可用：page、seek、body。
    let old_page = page(
        &client,
        &sid,
        &old["snapshotToken"],
        &old["readTicket"],
        "beforeCursor",
        &old["beforeCursor"],
        1,
    )
    .await
    .expect("failed open must not revoke the old ticket");
    assert!(old_page.get("items").is_some());

    let sought = client
        .send_request(
            "session/load",
            json!({
                "sessionId": sid,
                "history": {
                    "kind": "seek",
                    "snapshotToken": old["snapshotToken"],
                    "readTicket": old["readTicket"],
                    "anchor": { "entryId": old_entry, "source": "own" },
                    "beforeLimit": 1,
                    "afterLimit": 1
                }
            }),
        )
        .await
        .expect("old ticket seek after failed open");
    assert!(sought.get("items").is_some());

    let old_body = client
        .send_request(
            "session/history/body",
            json!({
                "sessionId": sid,
                "snapshotToken": old["snapshotToken"],
                "readTicket": old["readTicket"],
                "entryId": old_entry,
                "offset": 0,
                "maxBytes": 7
            }),
        )
        .await
        .expect("old descriptor after failed open");
    assert!(!old_body["bytes"].as_array().unwrap().is_empty());

    // 成功的 open 才替换旧 ticket。
    open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .expect("successful open");
    let stale = page(
        &client,
        &sid,
        &old["snapshotToken"],
        &old["readTicket"],
        "beforeCursor",
        &old["beforeCursor"],
        1,
    )
    .await
    .unwrap_err();
    assert_eq!(stale.code, -32014);
    task.abort();
}

#[tokio::test]
#[serial]
async fn resume_wire_uses_the_shared_preparation_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;
    client
        .send_request("session/resume", json!({ "sessionId": sid }))
        .await
        .expect("resume through the shared path");
    task.abort();
}

#[tokio::test]
#[serial]
async fn fork_wire_uses_the_shared_preparation_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "unused-test-key",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&config).unwrap();
    let cfg = Arc::new(make_server_config(config, provider, &tmp).await);
    let sessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let (client, _sessions, task) =
        spawn_loop(Arc::clone(&cfg), sessions, Some(window_caps())).await;
    let created = client
        .send_request("session/new", json!({ "cwd": tmp.path() }))
        .await
        .expect("create a fork source with frozen data");
    let source = created["sessionId"].as_str().expect("source id").to_owned();
    let forked = client
        .send_request("session/fork", json!({ "sessionId": source }))
        .await
        .expect("fork through the shared path");
    let target = forked["sessionId"].as_str().expect("fork target id");
    assert_ne!(target, source);
    task.abort();
}
