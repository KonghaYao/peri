use std::collections::BTreeMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use axum::body::to_bytes;
use axum::response::IntoResponse;
use futures::StreamExt;
use serde_json::json;
use tokio::sync::broadcast;

use super::{create_response, event_stream};
use crate::domain::{
    AgentView, EnvironmentRef, ItemKind, ItemSource, ItemView, OfficialEvent, OfficialTurn,
    SessionState, SessionStatus, SessionView, StreamEvent, TurnStatus, TurnView,
};
use crate::service::{CreateSessionResult, EventSubscription};

fn item(seq: u64) -> StreamEvent {
    StreamEvent::Item(ItemView {
        id: "itim_test".to_owned(),
        seq,
        item_id: "itim_test".to_owned(),
        kind: ItemKind::Message,
        source: ItemSource::root(),
        payload: json!({ "text": format!("item {seq}") }),
        created_at: "2026-01-01T00:00:00.000000Z".to_owned(),
        truncated: false,
    })
}

fn session_view(id: &str) -> SessionView {
    SessionView {
        id: id.to_owned(),
        object: "agent.session".to_owned(),
        logical_id: id.to_owned(),
        root_session_id: "0192f0c5-0000-7000-8000-000000000001".to_owned(),
        cwd: "/tmp".to_owned(),
        state: SessionState::Idle,
        status: SessionStatus::Idle,
        turn: TurnView::default(),
        last_item_seq: 0,
        created_at: 100,
        last_active_at: 101,
        agent: AgentView {
            model: Some("model-a".to_owned()),
            instructions: None,
        },
        environment: EnvironmentRef::none(),
        sandbox: None,
        required_actions: Vec::new(),
        vault_ids: Vec::new(),
        usage: None,
        error: None,
        metadata: BTreeMap::new(),
        prompt_cache_options: None,
    }
}

fn parse_frames(text: &str) -> Vec<(String, String)> {
    text.split("\n\n")
        .filter_map(|block| {
            let mut event = None;
            let mut data = None;
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("event: ") {
                    event = Some(value.to_owned());
                } else if let Some(value) = line.strip_prefix("data: ") {
                    data = Some(value.to_owned());
                }
            }
            Some((event?, data?))
        })
        .collect()
}

async fn read_frames(body: axum::body::Body, count: usize) -> Vec<(String, String)> {
    let mut stream = body.into_data_stream();
    let mut text = String::new();
    while parse_frames(&text).len() < count {
        let chunk = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("the initial frames must arrive")
            .expect("a body chunk")
            .expect("body");
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    parse_frames(&text)
}

/// The create stream's first official frame is `agent.session.created`; the
/// custom `session` frame follows it, and the first turn's frames name the same
/// session.
#[tokio::test]
async fn create_stream_starts_with_the_official_session_frame() {
    let (sender, receiver) = broadcast::channel(8);
    let subscription = EventSubscription::new(Vec::new(), receiver, Arc::new(AtomicU64::new(0)));
    let result = CreateSessionResult {
        session: session_view("agent-01"),
        created: true,
        pending_initial: None,
    };
    let response = create_response(&result, subscription, Duration::from_secs(60));
    sender
        .send(StreamEvent::Official(OfficialEvent::turn_created(
            "agent-01",
            "turn_1",
            OfficialTurn::new("agent-01", "turn_1", TurnStatus::InProgress, 100, None),
        )))
        .expect("live turn");

    let frames = read_frames(response.into_body(), 3).await;
    assert_eq!(frames[0].0, "agent.session.created", "{frames:?}");
    let created: serde_json::Value = serde_json::from_str(&frames[0].1).expect("json");
    assert_eq!(created["type"], json!("agent.session.created"));
    assert_eq!(created["event_id"], json!("evt_agent-01_session_created"));
    assert_eq!(created["session"]["id"], json!("agent-01"));
    assert_eq!(created["session"]["status"], json!("idle"));
    assert_eq!(frames[1].0, "session", "{frames:?}");
    assert_eq!(frames[2].0, "agent.session.turn.created", "{frames:?}");
    let turn: serde_json::Value = serde_json::from_str(&frames[2].1).expect("json");
    assert_eq!(
        turn["session_id"], created["session"]["id"],
        "the first turn names the created session"
    );
}

/// An idempotent attach reuses the session; it must not claim the official
/// `agent.session.created` frame.
#[tokio::test]
async fn attach_stream_does_not_emit_session_created() {
    let (sender, receiver) = broadcast::channel(8);
    let subscription = EventSubscription::new(Vec::new(), receiver, Arc::new(AtomicU64::new(0)));
    let result = CreateSessionResult {
        session: session_view("agent-01"),
        created: false,
        pending_initial: None,
    };
    let response = create_response(&result, subscription, Duration::from_secs(60));
    sender
        .send(StreamEvent::Official(OfficialEvent::turn_created(
            "agent-01",
            "turn_1",
            OfficialTurn::new("agent-01", "turn_1", TurnStatus::InProgress, 100, None),
        )))
        .expect("live turn");

    let frames = read_frames(response.into_body(), 2).await;
    assert_eq!(frames[0].0, "session", "{frames:?}");
    let session: serde_json::Value = serde_json::from_str(&frames[0].1).expect("json");
    assert_eq!(session["created"], json!(false));
    assert_eq!(frames[1].0, "agent.session.turn.created", "{frames:?}");
    assert!(
        !frames
            .iter()
            .any(|(event, _)| event == "agent.session.created"),
        "attach must not fabricate a created frame: {frames:?}"
    );
}

/// A subscription that fell behind lost events, possibly a turn terminal.
/// Reporting one `gap` and closing the stream is the only way an official SDK
/// can notice the interruption instead of waiting for the next heartbeat
/// forever.
#[tokio::test]
async fn a_lagged_subscription_reports_one_gap_and_ends() {
    let (sender, receiver) = broadcast::channel(1);
    for seq in 1..=3 {
        sender.send(item(seq)).expect("fill the channel");
    }
    let subscription = EventSubscription::new(Vec::new(), receiver, Arc::new(AtomicU64::new(0)));
    let response = axum::response::Sse::new(event_stream(
        subscription,
        Duration::from_secs(60),
        Vec::new(),
    ))
    .into_response();
    let body = tokio::time::timeout(
        Duration::from_secs(5),
        to_bytes(response.into_body(), usize::MAX),
    )
    .await
    .expect("the stream must end after the gap, not hang")
    .expect("body");
    let text = String::from_utf8_lossy(&body);

    assert_eq!(
        text.matches("event: ").count(),
        1,
        "exactly the gap frame is delivered: {text}"
    );
    assert!(text.contains("event: gap"), "{text}");
    assert!(
        text.contains("\"fromSeq\":1,\"toSeq\":2"),
        "the gap names the dropped sequence range: {text}"
    );
}
