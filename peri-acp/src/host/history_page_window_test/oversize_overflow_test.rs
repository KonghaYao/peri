//! oversize 记忆溢出后的 body 行为：身份集合不再是权威，必须按稳定 `entryId`
//! 回存储重取校验；已暴露 descriptor 仍可读，未暴露条目继续拒绝。

use super::*;

#[tokio::test]
#[serial]
async fn overflowed_memory_revalidates_by_stable_entry_identity() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();

    // 使用超过默认页字节预算（512 KiB）的正文，保证溢出后的稳定身份重取
    // 仍按同一口径把它判定为 descriptor。
    let big = PersistedPayload::Message(BaseMessage::human("x".repeat(600 * 1024)));
    fixture
        .cfg
        .session_resources
        .append_history(&sid, std::slice::from_ref(&big))
        .await
        .unwrap();
    let normal_id = fixture.own_ids().await[0];

    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .expect("open latest with a canonical oversize entry");
    let descriptor = &opened["items"][0];
    assert_eq!(
        descriptor["oversize"].as_bool(),
        Some(true),
        "the large latest entry must be a descriptor: {opened}"
    );
    let descriptor_entry = descriptor["entryId"].as_str().unwrap().to_owned();

    // 模拟身份集合/预算溢出：集合不再是权威。
    {
        let mut states = sessions.lock().await;
        states
            .get_mut(&sid)
            .unwrap()
            .history_window
            .force_oversize_overflow();
    }

    // 已暴露 descriptor：按稳定 entryId 重取校验后仍可读。
    let chunk = client
        .send_request(
            "session/history/body",
            json!({
                "sessionId": sid,
                "snapshotToken": opened["snapshotToken"],
                "readTicket": opened["readTicket"],
                "entryId": descriptor_entry,
                "offset": 0,
                "maxBytes": 7
            }),
        )
        .await
        .expect("overflowed ticket must still serve an exposed descriptor");
    assert!(!chunk["bytes"].as_array().unwrap().is_empty());

    // 未暴露的普通条目：重取按稳定身份判定它不是 descriptor，继续拒绝。
    let normal_entry =
        peri_acp_types::history_window::HistoryEntryId::new(&sid, normal_id).encode();
    let rejected = client
        .send_request(
            "session/history/body",
            json!({
                "sessionId": sid,
                "snapshotToken": opened["snapshotToken"],
                "readTicket": opened["readTicket"],
                "entryId": normal_entry,
                "offset": 0,
                "maxBytes": 7
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(
        rejected.code, -32016,
        "an unexposed entry must not become readable after overflow"
    );
    task.abort();
}
