//! F04：声明 `peri.historyWindowV1` 的连接，`session/load` 不交付历史回放。
//!
//! 首屏由随后的 `history.open` 提供；跳过回放不能绕过所有权检查。非窗口连接
//! 保持现有回放行为（本文件用旧分页连接验证“不跳过”）。

use super::*;

/// 历史回放通知里的正文（只统计 user/agent message chunk）。
fn replay_texts(notifications: &[(String, Value)]) -> Vec<String> {
    notifications
        .iter()
        .filter(|(method, params)| {
            method == "session/update"
                && matches!(
                    params["update"]["sessionUpdate"].as_str(),
                    Some("user_message_chunk") | Some("agent_message_chunk")
                )
        })
        .map(|(_, params)| {
            params["update"]["content"]["text"]
                .as_str()
                .expect("replay text chunk carries content.text")
                .to_owned()
        })
        .collect()
}

/// 窗口连接：load 不产生任何 replay payload，首屏由显式 open 提供。
#[tokio::test]
#[serial]
async fn window_connection_load_skips_replay_and_open_provides_the_first_screen() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;

    client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .expect("window connection load must succeed");
    assert!(
        replay_texts(&s9c_drain_notifications(&client).await).is_empty(),
        "声明 historyWindowV1 的连接，session/load 不得交付任何历史回放"
    );
    assert!(
        !sessions.lock().await[&sid].history_window.has_ticket(),
        "load 本身不建立窗口票据；首屏必须由客户端显式 open"
    );

    let opened = open(
        &client,
        &sid,
        json!({ "kind": "open", "direction": "before", "limit": 2 }),
    )
    .await
    .expect("window open after load must succeed");
    assert_eq!(
        opened["items"]
            .as_array()
            .expect("open carries items")
            .len(),
        2,
        "首屏内容来自 open，而不是 load 的回放"
    );
    task.abort();
}

/// 非窗口连接：保持现有全量回放（标准连接：canonical 完整历史）。
#[tokio::test]
#[serial]
async fn non_window_connection_load_keeps_full_replay() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) =
        spawn_loop(cfg, fixture.sessions, Some(PeriCaps::default())).await;

    client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .expect("non-window connection load must succeed");
    assert_eq!(
        replay_texts(&s9c_drain_notifications(&client).await),
        vec!["第一轮用户问题", "第一轮回答", "第二轮用户问题"],
        "未声明 historyWindowV1 的连接必须保持现有全量回放"
    );
    task.abort();
}

/// 窗口连接遇到 `HandedOff`：仍按所有权拒绝，不能因为 skip_replay 而放过。
#[tokio::test]
#[serial]
async fn window_connection_load_still_rejects_handed_off_context() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) = spawn_loop(cfg, fixture.sessions, Some(window_caps())).await;
    {
        let mut states = sessions.lock().await;
        let state = states.get_mut(&sid).expect("fixture session must be live");
        state.context_ownership = crate::host::ContextOwnership::HandedOff;
        state.history_payloads.clear();
    }

    let error = client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .expect_err("HandedOff must reject the load even for a window connection");
    assert_eq!(error.code, -32010, "HandedOff：错误码");
    assert_eq!(
        error.message, "Session context is owned by an in-flight prompt",
        "HandedOff：措辞"
    );
    assert!(
        replay_texts(&s9c_drain_notifications(&client).await).is_empty(),
        "拒绝不得发出任何回放通知"
    );
    let states = sessions.lock().await;
    let state = states
        .get(&sid)
        .expect("rejection must not remove the session");
    assert_eq!(
        state.context_ownership,
        crate::host::ContextOwnership::HandedOff,
        "拒绝不得改写所有权状态"
    );
    assert!(
        state.history_payloads.is_empty(),
        "拒绝不得把存储快照回填进热态"
    );
    drop(states);
    task.abort();
}
