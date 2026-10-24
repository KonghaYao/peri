//! F2a：History V2 实时投递的 sink 行为（暂停缓冲、恢复续号、非 transcript 透传）。

use super::*;

/// 暂停期（load 装配中）的 transcript 更新只进缓冲；恢复后 load 响应已发，
/// 补发事件从 afterSeq + 1 开始；之后恢复直发。
#[tokio::test]
async fn history_v2_live_buffers_transcript_updates_while_paused() {
    let transport = Arc::new(MockTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert("s1".to_string(), PeriCaps::default());
    let live = Arc::new(LiveDelivery::new());
    let boundary = live.begin("s1");
    let sink =
        TransportEventSink::new(transport.clone(), caps).with_live_delivery(Arc::clone(&live));

    for text in ["load window", "after response"] {
        sink.push_event(
            "s1",
            &ExecutorEvent::TextChunk {
                message_id: MessageId::new(),
                chunk: text.to_string(),
                source_agent_id: None,
            },
            0,
        )
        .await
        .expect("push_event");
        assert!(
            transport.notifications.lock().unwrap().is_empty(),
            "暂停期不得发出 session/update"
        );
    }

    let pending = live.resume_and_take("s1");
    assert_eq!(pending.len(), 2, "两条缓冲必须在恢复时按序交付");
    for (index, payload) in pending.iter().enumerate() {
        assert_eq!(
            payload["_meta"]["peri.delivery"]["streamId"],
            boundary.stream_id.as_str()
        );
        assert_eq!(
            payload["_meta"]["peri.delivery"]["eventSeq"],
            (index + 1).to_string(),
            "恢复后首条从 afterSeq + 1 开始"
        );
    }

    sink.push_event(
        "s1",
        &ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "live".to_string(),
            source_agent_id: Some("child-agent".to_string()),
        },
        0,
    )
    .await
    .expect("push_event");

    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(notifications.len(), 1, "恢复后直发且只发新事件");
    let meta = &notifications[0].1["_meta"];
    assert_eq!(meta["peri.delivery"]["eventSeq"], "3");
    assert_eq!(
        meta["peri.delivery"]["deliveryId"],
        format!("{}:3", boundary.stream_id)
    );
    assert_eq!(
        meta["peri"]["sourceAgentId"], "child-agent",
        "投递元数据必须与既有 _meta 键合并，不能覆盖来源身份"
    );
}

/// 非 transcript 通知（用量、配置、命令等）不进入实时序列，wire 形状保持原样。
#[tokio::test]
async fn history_v2_live_ignores_non_transcript_updates() {
    let transport = Arc::new(MockTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert(
        "s1".to_string(),
        PeriCaps {
            token_stats: true,
            ..PeriCaps::default()
        },
    );
    let live = Arc::new(LiveDelivery::new());
    live.begin("s1");
    live.resume_and_take("s1");
    let sink = TransportEventSink::new(transport.clone(), caps).with_live_delivery(live);

    sink.push_event(
        "s1",
        &ExecutorEvent::LlmCallEnd {
            step: 0,
            model: "aux-model".into(),
            output: String::new(),
            usage: Some(peri_acp_types::model::TokenUsage::new(100, 1)),
            stop_reason: None,
            request_id: Some("aux-request".into()),
            source_agent_id: None,
        },
        200_000,
    )
    .await
    .expect("push_event");

    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(
        notifications[0].1["update"]["sessionUpdate"],
        "usage_update"
    );
    assert!(
        notifications[0].1.get("_meta").is_none(),
        "非 transcript 更新不得携带投递元数据：{}",
        notifications[0].1
    );
}

/// 没有实时注册表（V1/非 V2 会话）时，transcript 更新不带任何投递键。
#[tokio::test]
async fn history_v2_absent_live_keeps_transcript_wire_unchanged() {
    let transport = Arc::new(MockTransport::default());
    let caps: Arc<DashMap<String, PeriCaps>> = Arc::new(DashMap::new());
    caps.insert("s1".to_string(), PeriCaps::default());
    let sink = TransportEventSink::new(transport.clone(), caps);

    sink.push_event(
        "s1",
        &ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "legacy".to_string(),
            source_agent_id: None,
        },
        0,
    )
    .await
    .expect("push_event");

    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(notifications.len(), 1);
    assert!(
        notifications[0].1.get("_meta").is_none(),
        "非 V2 会话不得出现投递元数据：{}",
        notifications[0].1
    );
}
