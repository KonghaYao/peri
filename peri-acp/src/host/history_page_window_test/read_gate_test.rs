//! 只读 gate：执行上下文不驻留不构成读取拒绝，写入安全边界继续拒绝。
//!
//! `HandedOff`/`ReloadRequired` 只描述执行上下文是否在当前实例，窗口读取的事实源是
//! Store。只读放行不等于解除写入约束：`closing`、rewind 在途与会话不存在仍在进入
//! 存储前拒绝。

use super::*;

#[tokio::test]
#[serial]
async fn handed_off_and_reload_required_allow_read_only_window() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let own = fixture.own_ids().await;
    let anchor_entry = peri_acp_types::history_window::HistoryEntryId::new(&sid, own[1]).encode();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    // 执行上下文移交给在途 prompt 不等于历史不可读：Store 是固定事实，窗口只做读取。
    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().context_ownership = crate::host::ContextOwnership::HandedOff;
    }
    let handed_off = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 2 }),
    )
    .await
    .expect("HandedOff must allow read-only open");
    assert_eq!(handed_off["items"].as_array().unwrap().len(), 2);
    let fetched = page(
        &client,
        &sid,
        &handed_off["snapshotToken"],
        &handed_off["readTicket"],
        "beforeCursor",
        &handed_off["beforeCursor"],
        1,
    )
    .await
    .expect("HandedOff must allow read-only page");
    assert!(!fetched["items"].as_array().unwrap().is_empty());
    let sought = client
        .send_request(
            "session/load",
            json!({
                "sessionId": sid,
                "history": {
                    "kind": "seek",
                    "snapshotToken": handed_off["snapshotToken"],
                    "readTicket": handed_off["readTicket"],
                    "anchor": { "entryId": anchor_entry, "source": "own" },
                    "beforeLimit": 1,
                    "afterLimit": 1
                }
            }),
        )
        .await
        .expect("HandedOff must allow read-only seek");
    assert!(sought.get("items").is_some());

    // ReloadRequired 同理：需要重新 load 的是执行上下文，不是只读窗口。
    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().context_ownership =
            crate::host::ContextOwnership::ReloadRequired;
    }
    let reload = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 2 }),
    )
    .await
    .expect("ReloadRequired must allow read-only open");
    assert!(!reload["items"].as_array().unwrap().is_empty());
    page(
        &client,
        &sid,
        &reload["snapshotToken"],
        &reload["readTicket"],
        "beforeCursor",
        &reload["beforeCursor"],
        1,
    )
    .await
    .expect("ReloadRequired must allow read-only page");
    client
        .send_request(
            "session/load",
            json!({
                "sessionId": sid,
                "history": {
                    "kind": "seek",
                    "snapshotToken": reload["snapshotToken"],
                    "readTicket": reload["readTicket"],
                    "anchor": { "entryId": anchor_entry, "source": "own" },
                    "beforeLimit": 1,
                    "afterLimit": 1
                }
            }),
        )
        .await
        .expect("ReloadRequired must allow read-only seek");

    // 只读放行不解除写入安全边界：closing / rewind / 会话消失仍在进入存储前拒绝。
    {
        let mut states = sessions.lock().await;
        states.get_mut(&sid).unwrap().closing = true;
    }
    let closing = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap_err();
    assert_eq!(closing.code, -32010, "closing 必须拒绝只读: {closing}");
    {
        let mut states = sessions.lock().await;
        let state = states.get_mut(&sid).unwrap();
        state.closing = false;
        state.history_window.begin_rewind();
    }
    let rewinding = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap_err();
    assert_eq!(
        rewinding.code, -32010,
        "rewind 在途必须拒绝只读: {rewinding}"
    );
    sessions.lock().await.remove(&sid);
    let missing = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 1 }),
    )
    .await
    .unwrap_err();
    assert_eq!(missing.code, -32602, "会话不存在必须拒绝只读: {missing}");
    task.abort();
}
