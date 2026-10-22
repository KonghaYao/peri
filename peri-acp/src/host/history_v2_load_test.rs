//! H4b：`session/load` 接入 History V2 `initial`（设计 §4.4、§6.1–6.5、§12.1）。
//!
//! 走真实 `ServerLoop` + MPSC transport：连接级协商、生命周期装配、initial 服务调用与
//! 响应挂载都在派发链上，只测服务层会漏掉「准入发生在装配之前」。
//!
//! 用例全部以 `session_load_v2_` 开头，便于 `cargo test -- session_load` 覆盖。

use super::*;
use crate::session::event_sink::TransportEventSink;
use peri_acp_types::event::{EventSink, ExecutorEvent};
use peri_acp_types::history_v2::{
    ExecutionEffect, HistoryEffectState, HistoryErrorData, HistoryErrorReason, HistoryLimits,
    HistoryServerCapability, HISTORY_CHANGED_NOTIFICATION, HISTORY_METHOD,
};
use peri_acp_types::messages::MessageId;

fn history_v2_caps() -> PeriCaps {
    PeriCaps {
        history_v2: Some(HistoryServerCapability {
            version: 2,
            selected_profile: "windowed".to_owned(),
            method: HISTORY_METHOD.to_owned(),
            notification: HISTORY_CHANGED_NOTIFICATION.to_owned(),
            limits: HistoryLimits::default(),
        }),
        ..Default::default()
    }
}

fn initial_meta(request_key: &str, target_bytes: u64) -> Value {
    initial_meta_with_limit(request_key, 50, target_bytes)
}

fn initial_meta_with_limit(request_key: &str, limit: u32, target_bytes: u64) -> Value {
    json!({
        "_meta": {
            "peri.history": {
                "version": 2,
                "action": "initial",
                "requestKey": request_key,
                "limit": limit,
                "targetBytes": target_bytes,
            }
        }
    })
}

fn load_params(sid: &str, meta: Value) -> Value {
    let mut params = json!({ "sessionId": sid });
    params
        .as_object_mut()
        .unwrap()
        .extend(meta.as_object().unwrap().clone());
    params
}

fn error_data(error: &crate::transport::types::AcpError) -> HistoryErrorData {
    serde_json::from_value(
        error
            .data
            .clone()
            .unwrap_or_else(|| panic!("错误必须带 data：{error:?}")),
    )
    .unwrap_or_else(|cause| panic!("错误 data 必须是 HistoryErrorData：{cause}; {error:?}"))
}

async fn v2_fixture(
    tmp: &tempfile::TempDir,
) -> (
    PageFixture,
    Arc<AcpServerConfig>,
    Arc<crate::transport::mpsc::MpscClientTransport>,
    tokio::task::JoinHandle<()>,
) {
    let fixture = PageFixture::new(tmp).await;
    let cfg = Arc::clone(&fixture.cfg);
    let (client, _sessions, task) = spawn_loop(
        cfg.clone(),
        fixture.sessions.clone(),
        Some(history_v2_caps()),
    )
    .await;
    (fixture, cfg, client, task)
}

/// V2 load + initial 成功：标准字段保留，`_meta["peri.history"]` 给出首屏、delivery 与
/// 执行事实；同一窗口可继续走 `_peri/history page`。
#[tokio::test]
#[serial]
async fn session_load_v2_initial_returns_page_and_window_continues() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (fixture, _cfg, client, task) = v2_fixture(&tmp).await;
    let sid = fixture.sid.clone();

    let response = client
        .send_request(
            "session/load",
            load_params(&sid, initial_meta_with_limit("1", 2, 524_288)),
        )
        .await
        .expect("V2 initial load 必须成功");

    assert!(response["modes"].is_object(), "标准 modes 字段必须保留");
    assert!(
        response["configOptions"].is_array(),
        "标准 configOptions 字段必须保留"
    );
    assert!(
        response["_meta"].get("periLoadGeneration").is_none(),
        "V2 响应不得混入 V1 回放代次"
    );

    let initial = &response["_meta"]["peri.history"];
    assert_eq!(initial["version"], 2);
    assert_eq!(initial["kind"], "initial");
    assert_eq!(initial["requestKey"], "1");
    assert_eq!(initial["executionReady"], true);
    assert!(
        initial["runtimeGeneration"]
            .as_str()
            .is_some_and(|value| value.starts_with("runtime-")),
        "runtimeGeneration 必须来自当前生命周期：{initial}"
    );
    assert!(
        initial["deliveryId"]
            .as_str()
            .is_some_and(|value| value.starts_with("dl_")),
        "deliveryId 必须由服务端生成：{initial}"
    );
    assert!(
        initial["liveBoundary"]["streamId"]
            .as_str()
            .is_some_and(|value| value.starts_with("live:")),
        "streamId 必须标识本连接交付流：{initial}"
    );
    assert_eq!(initial["liveBoundary"]["afterSeq"], "0");
    assert!(
        fixture
            .sessions
            .lock()
            .await
            .get(&sid)
            .and_then(|state| state.live_delivery.clone())
            .is_some(),
        "V2 initial 成功装配后必须把本连接的实时注册表绑定到会话"
    );
    // 首屏由 initial 承载：V2 load 不得再发送回放 transcript 更新（配置通知不受影响）。
    let replayed = s9c_drain_notifications(&client).await;
    let transcript_updates = replayed
        .iter()
        .filter(|(method, params)| {
            method == "session/update"
                && matches!(
                    params["update"]["sessionUpdate"].as_str(),
                    Some(
                        "agent_message_chunk"
                            | "agent_thought_chunk"
                            | "user_message_chunk"
                            | "tool_call"
                            | "tool_call_update"
                    )
                )
        })
        .count();
    assert_eq!(
        transcript_updates, 0,
        "V2 load 不得发送回放 transcript session/update：{replayed:?}"
    );

    let page = &initial["page"];
    assert_eq!(page["kind"], "window");
    assert_eq!(page["sessionId"], sid.as_str());
    assert_eq!(
        page["items"].as_array().map(|items| items.len()),
        Some(2),
        "limit=2 的首屏只交付最近两条，剩余一条由后续 page 补齐：{page}"
    );
    assert!(page["windowId"].is_string());
    assert!(page["snapshotToken"].is_string());
    assert!(page["readTicket"].is_string());
    assert!(
        page["beforeCursor"].is_string(),
        "首屏还有更早历史时必须给出 beforeCursor（page 的 home 边界）：{page}"
    );

    let next = client
        .send_request(
            "_peri/history",
            json!({
                "version": 2,
                "action": "page",
                "sessionId": sid,
                "requestKey": "2",
                "windowId": page["windowId"],
                "snapshotToken": page["snapshotToken"],
                "readTicket": page["readTicket"],
                "direction": "before",
                "limit": 50,
            }),
        )
        .await
        .expect("initial 返回的窗口票据必须可直接继续分页");
    assert_eq!(next["kind"], "window");
    assert_eq!(next["windowId"], page["windowId"]);
    assert_eq!(next["snapshotToken"], page["snapshotToken"]);
    assert_eq!(
        next["items"].as_array().map(|items| items.len()),
        Some(1),
        "同一窗口的下一页补齐剩余一条：{next}"
    );

    task.abort();
}

/// 相同 requestKey 重放：initial 结果字节等价，deliveryId 不换。
#[tokio::test]
#[serial]
async fn session_load_v2_same_request_key_replays_initial_bytes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (fixture, _cfg, client, task) = v2_fixture(&tmp).await;
    let sid = fixture.sid.clone();
    let params = load_params(&sid, initial_meta("7", 524_288));

    let first = client
        .send_request("session/load", params.clone())
        .await
        .expect("首次 initial 成功");
    let second = client
        .send_request("session/load", params)
        .await
        .expect("同 requestKey 重放必须成功");

    let first = &first["_meta"]["peri.history"];
    let second = &second["_meta"]["peri.history"];
    assert_eq!(
        serde_json::to_vec(first).unwrap(),
        serde_json::to_vec(second).unwrap(),
        "同一 requestKey 的 initial 必须字节等价"
    );
    assert_eq!(first["deliveryId"], second["deliveryId"]);

    task.abort();
}

/// 窗口关闭后再用同一 requestKey 重试：显式 `delivery_expired`，不返回旧执行事实。
#[tokio::test]
#[serial]
async fn session_load_v2_closed_window_replay_is_delivery_expired() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (fixture, _cfg, client, task) = v2_fixture(&tmp).await;
    let sid = fixture.sid.clone();
    let params = load_params(&sid, initial_meta("1", 524_288));

    let response = client
        .send_request("session/load", params.clone())
        .await
        .expect("首次 initial 成功");
    let page = &response["_meta"]["peri.history"]["page"];

    client
        .send_request(
            "_peri/history",
            json!({
                "version": 2,
                "action": "close",
                "sessionId": sid,
                "requestKey": "2",
                "windowId": page["windowId"],
                "snapshotToken": page["snapshotToken"],
                "readTicket": page["readTicket"],
            }),
        )
        .await
        .expect("关闭窗口必须成功");

    let error = client
        .send_request("session/load", params)
        .await
        .expect_err("窗口关闭后的同键重试必须显式失败");
    assert_eq!(error.code, -32000);
    let data = error_data(&error);
    assert_eq!(data.reason, HistoryErrorReason::DeliveryExpired);
    assert_eq!(data.effect.history, HistoryEffectState::NotPublished);
    assert_eq!(data.effect.execution, ExecutionEffect::Ready);
    assert!(data.effect.runtime_generation.is_some());

    task.abort();
}

/// initial 失败（同键不同参数 → request_conflict）：load 返回可理解错误与 effect，
/// 会话仍驻留且不启动执行。
#[tokio::test]
#[serial]
async fn session_load_v2_initial_failure_returns_effect_without_execution() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (fixture, _cfg, client, task) = v2_fixture(&tmp).await;
    let sid = fixture.sid.clone();
    let generation_before = {
        let sessions = fixture.sessions.lock().await;
        sessions[&sid].session_generation
    };

    client
        .send_request(
            "session/load",
            load_params(&sid, initial_meta("3", 524_288)),
        )
        .await
        .expect("首次 initial 成功");

    let error = client
        .send_request("session/load", load_params(&sid, initial_meta("3", 65_536)))
        .await
        .expect_err("同键不同预算必须显式失败");
    assert_eq!(error.code, -32000);
    let data = error_data(&error);
    assert_eq!(data.reason, HistoryErrorReason::RequestConflict);
    assert_eq!(data.effect.history, HistoryEffectState::NotPublished);
    assert_eq!(data.effect.execution, ExecutionEffect::Ready);
    assert!(data.effect.runtime_generation.is_some());

    let sessions = fixture.sessions.lock().await;
    let state = &sessions[&sid];
    assert_eq!(
        state.session_generation, generation_before,
        "失败的 initial 不得启动或重建执行"
    );
    assert_eq!(
        state.context_ownership,
        crate::host::ContextOwnership::Resident
    );
    assert!(!state.closing);
    drop(sessions);

    task.abort();
}

/// 旧的 legacy-only load 不出现任何新 key；V2 连接缺 meta 则显式拒绝。
#[tokio::test]
#[serial]
async fn session_load_v2_legacy_response_is_unchanged_and_missing_meta_is_rejected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();

    let (legacy_client, _sessions, legacy_task) =
        spawn_loop(Arc::clone(&fixture.cfg), fixture.sessions.clone(), None).await;
    let legacy = legacy_client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .expect("legacy load 必须保持成功");
    assert!(
        legacy["_meta"].get("peri.history").is_none(),
        "legacy-only 响应不得出现 V2 key：{legacy}"
    );
    assert!(legacy["modes"].is_object());
    assert!(legacy["configOptions"].is_array());
    legacy_task.abort();

    let (v2_client, _sessions, v2_task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        fixture.sessions.clone(),
        Some(history_v2_caps()),
    )
    .await;
    let error = v2_client
        .send_request("session/load", json!({ "sessionId": sid }))
        .await
        .expect_err("V2 连接的 load 缺 initial 必须显式拒绝");
    assert_eq!(error.code, -32602);
    assert_eq!(
        error.data.as_ref().and_then(|data| data["reason"].as_str()),
        Some("history_initial_required")
    );
    v2_task.abort();
}

/// 混入旧顶层私有字段 → -32602；未协商 V2 却带 initial → protocol_not_negotiated。
#[tokio::test]
#[serial]
async fn session_load_v2_mixed_fields_and_unnegotiated_initial_are_rejected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = fixture.sid.clone();

    let (v2_client, _sessions, v2_task) = spawn_loop(
        Arc::clone(&fixture.cfg),
        fixture.sessions.clone(),
        Some(history_v2_caps()),
    )
    .await;
    for (field, value) in [
        ("cursor", json!("opaque")),
        ("limit", json!(10)),
        ("loadGeneration", json!("gen")),
        ("history", json!({})),
    ] {
        let mut params = load_params(&sid, initial_meta("1", 524_288));
        params[field] = value;
        let error = v2_client
            .send_request("session/load", params)
            .await
            .unwrap_err();
        assert_eq!(error.code, -32602, "{field} 必须按格式错误拒绝");
        assert_eq!(
            error.data.as_ref().and_then(|data| data["reason"].as_str()),
            Some("history_initial_mixed_fields"),
            "{field} 必须显式说明混合字段：{error:?}"
        );
    }
    v2_task.abort();

    let (legacy_client, _sessions, legacy_task) =
        spawn_loop(Arc::clone(&fixture.cfg), fixture.sessions.clone(), None).await;
    let error = legacy_client
        .send_request(
            "session/load",
            load_params(&sid, initial_meta("1", 524_288)),
        )
        .await
        .expect_err("未协商 V2 的 initial 不得被静默忽略");
    assert_eq!(error.code, -32000);
    let data = error_data(&error);
    assert_eq!(data.reason, HistoryErrorReason::ProtocolNotNegotiated);
    assert_eq!(data.effect.execution, ExecutionEffect::NotReady);
    legacy_task.abort();
}

/// 同一连接连续多次 `session/load`：每次 initial 替换同会话旧窗口，不触发容量上限。
#[tokio::test]
#[serial]
async fn session_load_v2_repeated_loads_do_not_exhaust_window_capacity() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (fixture, _cfg, client, task) = v2_fixture(&tmp).await;
    let sid = fixture.sid.clone();

    let mut previous_window = String::new();
    for key in 1..=10 {
        let response = client
            .send_request(
                "session/load",
                load_params(&sid, initial_meta(&key.to_string(), 524_288)),
            )
            .await
            .unwrap_or_else(|error| panic!("第 {key} 次 V2 load 失败：{error:?}"));
        let page = &response["_meta"]["peri.history"]["page"];
        let window_id = page["windowId"]
            .as_str()
            .expect("每次 load 都必须交付窗口")
            .to_owned();
        assert_ne!(
            window_id, previous_window,
            "每次 initial 必须建立新窗口而不是复用/累积旧窗口"
        );
        previous_window = window_id;
    }

    task.abort();
}

/// `session/close` 成功后，本连接上该会话的旧窗口 page/body 都返回 `window_closed`。
#[tokio::test]
#[serial]
async fn session_close_invalidates_v2_windows() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (fixture, _cfg, client, task) = v2_fixture(&tmp).await;
    let sid = fixture.sid.clone();

    let response = client
        .send_request(
            "session/load",
            load_params(&sid, initial_meta("1", 524_288)),
        )
        .await
        .expect("V2 load");
    let live = fixture
        .sessions
        .lock()
        .await
        .get(&sid)
        .and_then(|state| state.live_delivery.clone())
        .expect("V2 load 必须绑定实时注册表");
    let page = &response["_meta"]["peri.history"]["page"];
    client
        .send_request("session/close", json!({"sessionId": sid}))
        .await
        .expect("session/close");
    assert!(
        !live.has_stream(&sid),
        "session/close 成功后必须移除本连接的实时流"
    );

    let page_error = client
        .send_request(
            "_peri/history",
            json!({
                "version": 2,
                "action": "page",
                "sessionId": sid,
                "requestKey": "2",
                "windowId": page["windowId"],
                "snapshotToken": page["snapshotToken"],
                "readTicket": page["readTicket"],
                "direction": "before",
                "limit": 50,
            }),
        )
        .await
        .expect_err("关闭后的窗口不得继续分页");
    assert_eq!(
        error_data(&page_error).reason,
        HistoryErrorReason::WindowClosed
    );

    let body_error = client
        .send_request(
            "_peri/history",
            json!({
                "version": 2,
                "action": "body",
                "sessionId": sid,
                "requestKey": "3",
                "windowId": page["windowId"],
                "snapshotToken": page["snapshotToken"],
                "readTicket": page["readTicket"],
                "bodyRef": "00000000-0000-0000-0000-000000000000",
                "offset": "0",
                "maxBytes": 1024,
            }),
        )
        .await
        .expect_err("关闭后的窗口不得继续读正文");
    assert_eq!(
        error_data(&body_error).reason,
        HistoryErrorReason::WindowClosed
    );

    task.abort();
}

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
