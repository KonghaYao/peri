//! 冷恢复、实时投递与 resume 的接线用例。
//!
//! 从 `history_v2_load_test.rs` 拆出：这些用例共享父模块的装载夹具，额外依赖事件
//! 写入器与实时投递注册表。

use crate::session::event_sink::TransportEventSink;
use peri_acp_types::event::{EventSink, ExecutorEvent};
use peri_acp_types::messages::MessageId;

use super::*;

/// 冷恢复路径同样在发布 `SessionState` 之后绑定实时注册表。
#[tokio::test]
#[serial]
async fn session_load_v2_cold_restore_binds_live_delivery() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (fixture, _cfg, client, task) = v2_fixture(&tmp).await;
    let sid = fixture.sid.clone();
    fixture.sessions.lock().await.remove(&sid);

    let response = client
        .send_request(
            "session/load",
            load_params(&sid, initial_meta("1", 524_288)),
        )
        .await
        .expect("冷恢复 V2 load");
    assert!(response["_meta"]["peri.history"]["liveBoundary"]["streamId"].is_string());
    assert!(
        fixture
            .sessions
            .lock()
            .await
            .get(&sid)
            .and_then(|state| state.live_delivery.clone())
            .is_some(),
        "冷恢复发布的新 SessionState 必须绑定本连接的实时注册表"
    );

    task.abort();
}

/// 冷恢复已发布会话后 initial 交付失败：保留会话供同 requestKey 重试，
/// 不得按「恢复失败」回滚拆掉已装配的 SessionState。
#[tokio::test]
#[serial]
async fn session_load_v2_cold_restore_initial_failure_keeps_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (fixture, cfg, client, task) = v2_fixture(&tmp).await;
    let sid = fixture.sid.clone();

    client
        .send_request(
            "session/load",
            load_params(&sid, initial_meta("3", 524_288)),
        )
        .await
        .expect("首次 initial 成功并登记连接级账本");
    fixture.sessions.lock().await.remove(&sid);

    // 同键不同预算：冷恢复发布后 initial 拒绝，恢复本身已经完成。
    let error = client
        .send_request("session/load", load_params(&sid, initial_meta("3", 65_536)))
        .await
        .expect_err("同键不同参数必须显式失败");
    assert_eq!(
        error_data(&error).reason,
        HistoryErrorReason::RequestConflict
    );
    assert!(
        fixture.sessions.lock().await.contains_key(&sid),
        "initial 交付失败不得回滚已发布的冷恢复会话"
    );
    assert!(
        cfg.session_manager
            .get_session(&sid)
            .is_some_and(|session| session
                .task_events_started
                .load(std::sync::atomic::Ordering::Acquire)),
        "保留的会话仍须绑定 TaskManager，否则重试前的 MCP 工具全部 unavailable"
    );

    task.abort();
}

/// V2 resume 不建立新流：另一连接恢复同一会话时，必须撤销它不拥有的实时绑定；
/// 拥有该流的连接 resume 时保留绑定。
#[tokio::test]
#[serial]
async fn session_resume_v2_detaches_live_delivery_owned_by_another_connection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (fixture, cfg, client_a, task_a) = v2_fixture(&tmp).await;
    let sid = fixture.sid.clone();
    let sessions = fixture.sessions.clone();

    let response = client_a
        .send_request(
            "session/load",
            load_params(&sid, initial_meta("1", 524_288)),
        )
        .await
        .expect("A 连接建立 V2 会话");
    assert!(response["_meta"]["peri.history"]["liveBoundary"]["streamId"].is_string());

    // 拥有者 resume：绑定保留。
    client_a
        .send_request("session/resume", json!({"sessionId": sid}))
        .await
        .expect("A 连接恢复同一会话");
    assert!(
        sessions
            .lock()
            .await
            .get(&sid)
            .and_then(|state| state.live_delivery.clone())
            .is_some(),
        "流的拥有连接 resume 不得撤销自己的绑定"
    );

    // 另一 V2 连接 resume：不得继承 A 的投递身份。
    let (client_b, _observed, task_b) =
        super::spawn_loop(cfg, sessions.clone(), Some(history_v2_caps())).await;
    client_b
        .send_request("session/resume", json!({"sessionId": sid}))
        .await
        .expect("B 连接恢复同一会话");
    assert!(
        sessions
            .lock()
            .await
            .get(&sid)
            .and_then(|state| state.live_delivery.clone())
            .is_none(),
        "非拥有连接的 resume 必须清除上一条连接留下的实时绑定"
    );

    task_b.abort();
    task_a.abort();
}

/// 等待 load 响应之后的 flush 完成：服务端在 `diagnostics.send` 返回后才恢复直发。
async fn wait_live_resumed(live: &crate::host::live_delivery::LiveDelivery, session_id: &str) {
    for _ in 0..400 {
        if !live.is_paused(session_id) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("live delivery did not resume after load response");
}

async fn next_session_update(client: &crate::transport::mpsc::MpscClientTransport) -> Value {
    loop {
        let incoming = tokio::time::timeout(std::time::Duration::from_secs(5), client.recv())
            .await
            .expect("session/update timeout")
            .expect("client transport closed");
        if let crate::transport::types::IncomingMessage::Notification { method, params } = incoming
        {
            // load 期间还会发送配置等标准通知；只取 transcript 内容的实时更新。
            if method == "session/update"
                && params["update"]["sessionUpdate"] == "agent_message_chunk"
            {
                return params;
            }
        }
    }
}

fn transcript(text: &str) -> ExecutorEvent {
    ExecutorEvent::TextChunk {
        message_id: MessageId::new(),
        chunk: text.to_owned(),
        source_agent_id: None,
    }
}

/// F2a：活动流直发 `eventSeq = afterSeq + 1`；重复 initial 换流并以旧边界续号。
#[tokio::test]
#[serial]
async fn session_load_v2_live_updates_follow_boundary_and_reload_replaces_stream() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();
    let (client, server) = crate::transport::mpsc::mpsc_transport_pair();
    let server: Arc<dyn crate::transport::AcpTransport> = Arc::new(server);
    let sink_transport = Arc::clone(&server);
    let (client, _sessions, task) = spawn_loop_with_transport(
        Arc::clone(&fixture.cfg),
        fixture.sessions.clone(),
        Some(history_v2_caps()),
        client,
        server,
    )
    .await;

    let first = client
        .send_request(
            "session/load",
            load_params(&sid, initial_meta("1", 524_288)),
        )
        .await
        .expect("首次 V2 load");
    let first_boundary = first["_meta"]["peri.history"]["liveBoundary"].clone();
    let first_stream = first_boundary["streamId"]
        .as_str()
        .expect("streamId")
        .to_owned();
    let live = fixture
        .sessions
        .lock()
        .await
        .get(&sid)
        .and_then(|state| state.live_delivery.clone())
        .expect("V2 load 必须绑定实时注册表");
    wait_live_resumed(&live, &sid).await;

    let sink = TransportEventSink::new(sink_transport, fixture.cfg.session_manager.caps_registry())
        .with_live_delivery(Arc::clone(&live));
    for text in ["one", "two"] {
        sink.push_event(&sid, &transcript(text), 0)
            .await
            .expect("push live update");
    }
    for expected in ["1", "2"] {
        let update = next_session_update(&client).await;
        let meta = &update["_meta"]["peri.delivery"];
        assert_eq!(meta["eventSeq"], expected);
        assert_eq!(meta["streamId"], first_stream.as_str());
        assert_eq!(
            meta["deliveryId"],
            format!("{first_stream}:{expected}").as_str()
        );
    }

    let second = client
        .send_request(
            "session/load",
            load_params(&sid, initial_meta("2", 524_288)),
        )
        .await
        .expect("重复 V2 load");
    let second_boundary = &second["_meta"]["peri.history"]["liveBoundary"];
    assert_eq!(
        second_boundary["afterSeq"], "2",
        "新首屏边界沿用已发出的最后序号：{second_boundary}"
    );
    assert_ne!(
        second_boundary["streamId"],
        first_stream.as_str(),
        "重复 initial 必须换流，旧 seq 不再使用"
    );
    assert!(
        !live.has_stream_with_id(&sid, &first_stream),
        "旧 streamId 在替换后不得仍然有效"
    );
    wait_live_resumed(&live, &sid).await;

    sink.push_event(&sid, &transcript("three"), 0)
        .await
        .expect("push after reload");
    let update = next_session_update(&client).await;
    let meta = &update["_meta"]["peri.delivery"];
    assert_eq!(meta["eventSeq"], "3");
    assert_eq!(meta["streamId"], second_boundary["streamId"]);

    task.abort();
}
