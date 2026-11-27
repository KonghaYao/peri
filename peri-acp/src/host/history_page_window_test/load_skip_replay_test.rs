//! F04：声明 `peri.historyWindowV1` 的连接，`session/load` 不交付历史回放。
//!
//! 首屏由随后的 `history.open` 提供；跳过回放不能绕过所有权检查。非窗口连接
//! 保持现有回放行为（本文件用旧分页连接验证“不跳过”）。

use crate::host::requests::tests::history_page_wire_tests::PageFixture;
use crate::transport::AcpTransport;
use peri_acp_types::PeriCaps;
use serde_json::Value;
use serde_json::json;
use serial_test::serial;
use std::sync::Arc;

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
    let archived = fixture.archive(1).await;
    let expected = fixture.own_ids().await;
    fixture.sessions.lock().await.remove(&sid);
    let cfg = Arc::clone(&fixture.cfg);
    let (client, sessions, task) =
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            Arc::clone(&cfg),
            fixture.sessions,
            Some(
                crate::host::requests::tests::history_page_wire_tests::window_tests::window_caps(),
            ),
        )
        .await;

    client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .expect("window connection load must succeed");
    assert!(
        replay_texts(
            &crate::host::requests::tests::history_page_wire_tests::s9c_drain_notifications(
                &client
            )
            .await
        )
        .is_empty(),
        "声明 historyWindowV1 的连接，session/load 不得交付任何历史回放"
    );
    assert!(
        !sessions.lock().await[&sid].history_window.has_ticket(),
        "load 本身不建立窗口票据；首屏必须由客户端显式 open"
    );

    let opened = crate::host::requests::tests::history_page_wire_tests::window_tests::open(
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
    client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .expect("reopening an idle window must also stay deferred");
    {
        let states = sessions.lock().await;
        let state = &states[&sid];
        assert_eq!(
            state.context_ownership,
            crate::host::ContextOwnership::Deferred
        );
        assert!(state.history_payloads.is_empty());
    }
    assert!(
        crate::host::prompt::ensure_resident_context(
            &sessions,
            &sid,
            &cfg.controller,
            cfg.session_resources.as_ref(),
        )
        .await
        .expect("the first execution must load the active transcript")
        .is_some()
    );
    let states = sessions.lock().await;
    let state = &states[&sid];
    assert_eq!(
        state.context_ownership,
        crate::host::ContextOwnership::Resident
    );
    assert_eq!(
        state
            .history_payloads
            .iter()
            .map(|payload| payload.id())
            .collect::<Vec<_>>(),
        expected
            .into_iter()
            .filter(|id| !archived.contains(id))
            .collect::<Vec<_>>()
    );
    task.abort();
}

#[tokio::test]
#[serial]
async fn deferred_window_retries_failed_context_read_without_publishing_empty_history() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let id = fixture.own_ids().await[0].as_uuid().to_string();
    fixture.sessions.lock().await.remove(&sid);
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(tmp.path().join("threads.db")),
    )
    .await
    .unwrap();
    let original: String = sqlx::query_scalar("SELECT content FROM messages WHERE message_id = ?")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE messages SET content = '{}' WHERE message_id = ?")
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();

    let cfg = fixture.cfg;
    let (client, sessions, task) =
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            Arc::clone(&cfg),
            fixture.sessions,
            Some(PeriCaps {
                history_window_v1: true,
                ..Default::default()
            }),
        )
        .await;
    client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .unwrap();
    let load = || {
        crate::host::prompt::ensure_resident_context(
            &sessions,
            &sid,
            &cfg.controller,
            cfg.session_resources.as_ref(),
        )
    };
    assert_eq!(load().await.unwrap_err().code, -32603);
    {
        let states = sessions.lock().await;
        assert_eq!(
            states[&sid].context_ownership,
            crate::host::ContextOwnership::Deferred
        );
        assert!(states[&sid].history_payloads.is_empty());
    }
    sqlx::query("UPDATE messages SET content = ? WHERE message_id = ?")
        .bind(original)
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(load().await.unwrap().is_some());
    assert_eq!(sessions.lock().await[&sid].history_payloads.len(), 3);
    pool.close().await;
    task.abort();
}

/// Keep the storage future parked: cancel and close must complete without releasing it.
#[tokio::test]
#[serial]
async fn stop_and_close_interrupt_blocked_first_context_read() {
    use std::time::Duration;
    use tokio::time::timeout;
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid;
    fixture.sessions.lock().await.remove(&sid);
    let (client, sessions, task) =
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            fixture.cfg,
            fixture.sessions,
            Some(PeriCaps {
                history_window_v1: true,
                agent_event_done: true,
                ..Default::default()
            }),
        )
        .await;
    client
        .send_request("session/load", json!({"sessionId":sid}))
        .await
        .unwrap();
    let generation = sessions.lock().await[&sid].session_generation;
    let start_blocked_prompt = |request_id: &'static str| {
        let client = Arc::clone(&client);
        let sid = sid.clone();
        async move {
            let gate = crate::host::prompt_dispatch::admission_gate::install(&format!(
                "{sid}:context-read"
            ));
            let prompt = tokio::spawn(async move {
                client.send_request("session/prompt", json!({
                    "sessionId":sid, "requestId":request_id, "prompt":[{"type":"text","text":"resume"}],
                })).await
            });
            timeout(Duration::from_secs(5), gate.entered.notified())
                .await
                .expect("context read started");
            prompt
        }
    };
    let stopped = start_blocked_prompt("stop-first-read").await;
    client
        .send_notification("session/cancel", json!({"sessionId":sid}))
        .await
        .unwrap();
    assert_eq!(
        timeout(Duration::from_secs(3), stopped)
            .await
            .expect("stop must drop parked read")
            .unwrap()
            .unwrap()["stopReason"],
        "cancelled"
    );
    {
        let states = sessions.lock().await;
        assert_eq!(
            states[&sid].context_ownership,
            crate::host::ContextOwnership::Deferred
        );
        assert!(states[&sid].history_payloads.is_empty() && states[&sid].cancel_token.is_none());
    }

    let interrupted = start_blocked_prompt("close-first-read").await;
    timeout(
        Duration::from_secs(3),
        client.send_request("session/close", json!({"sessionId":sid})),
    )
    .await
    .expect("close must acquire the per-session lock before its five-second timeout")
    .unwrap();
    assert_eq!(interrupted.await.unwrap().unwrap_err().code, -32010);
    assert!(!sessions.lock().await.contains_key(&sid));
    let notifications =
        crate::host::requests::tests::history_page_wire_tests::s9c_drain_notifications(&client)
            .await;
    let terminals: Vec<_> = notifications
        .iter()
        .filter(|(method, _)| method == "peri/agent_event_done")
        .map(|(_, value)| (value["requestId"].as_str(), value["stopReason"].as_str()))
        .collect();
    assert_eq!(
        terminals,
        [
            (Some("stop-first-read"), Some("cancelled")),
            (Some("close-first-read"), Some("error"))
        ]
    );

    client
        .send_request("session/load", json!({"sessionId":sid}))
        .await
        .unwrap();
    let states = sessions.lock().await;
    assert_ne!(states[&sid].session_generation, generation);
    assert_eq!(
        states[&sid].context_ownership,
        crate::host::ContextOwnership::Deferred
    );
    assert!(states[&sid].history_payloads.is_empty());
    task.abort();
}

#[tokio::test]
#[serial]
async fn late_context_read_cannot_populate_a_replacement_window() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid;
    fixture.sessions.lock().await.remove(&sid);
    let cfg = fixture.cfg;
    let (client, sessions, task) =
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            Arc::clone(&cfg),
            fixture.sessions,
            Some(PeriCaps {
                history_window_v1: true,
                ..Default::default()
            }),
        )
        .await;
    let params = json!({"sessionId":sid});
    client
        .send_request("session/load", params.clone())
        .await
        .unwrap();
    let generation = sessions.lock().await[&sid].session_generation;
    let gate =
        crate::host::prompt_dispatch::admission_gate::install(&format!("{sid}:context-read"));
    let read = crate::host::prompt::ensure_resident_context(
        &sessions,
        &sid,
        &cfg.controller,
        cfg.session_resources.as_ref(),
    );
    let replace = async {
        gate.entered.notified().await;
        client
            .send_request("session/close", params.clone())
            .await
            .unwrap();
        client.send_request("session/load", params).await.unwrap();
        gate.release.notify_one();
    };
    let (stale, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(read, replace)
    })
    .await
    .expect("replacement and delayed read must finish");
    assert_eq!(stale.unwrap_err().code, -32010);
    let states = sessions.lock().await;
    assert_ne!(states[&sid].session_generation, generation);
    assert_eq!(
        states[&sid].context_ownership,
        crate::host::ContextOwnership::Deferred
    );
    assert!(states[&sid].history_payloads.is_empty());
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
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            cfg,
            fixture.sessions,
            Some(PeriCaps::default()),
        )
        .await;

    client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .expect("non-window connection load must succeed");
    assert_eq!(
        replay_texts(
            &crate::host::requests::tests::history_page_wire_tests::s9c_drain_notifications(
                &client
            )
            .await
        ),
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
    let (client, sessions, task) =
        crate::host::requests::tests::history_page_wire_tests::spawn_loop(
            cfg,
            fixture.sessions,
            Some(
                crate::host::requests::tests::history_page_wire_tests::window_tests::window_caps(),
            ),
        )
        .await;
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
        replay_texts(
            &crate::host::requests::tests::history_page_wire_tests::s9c_drain_notifications(
                &client
            )
            .await
        )
        .is_empty(),
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
