#[tokio::test]
#[serial]
async fn oversize_descriptor_serves_bounded_body_chunks() {
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
    crate::host::requests::tests::history_page_wire_tests::s9c_drain_notifications(&client).await;

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
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap();
    let generation = opened["sessionGeneration"].as_str().unwrap().to_owned();

    client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .expect("initial load on a live session must still succeed");
    crate::host::requests::tests::history_page_wire_tests::s9c_drain_notifications(&client).await;

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
    let (client, _sessions, task) =
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            cfg,
            fixture.sessions,
            None,
        )
        .await;

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
    let (client, _sessions, task) =
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            cfg,
            fixture.sessions,
            None,
        )
        .await;
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
    crate::host::requests::tests::history_page_wire_tests::s9c_drain_notifications(&client).await;
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
