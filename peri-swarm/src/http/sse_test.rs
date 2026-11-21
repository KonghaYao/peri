use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use axum::body::to_bytes;
use axum::response::IntoResponse;
use serde_json::json;
use tokio::sync::broadcast;

use super::event_stream;
use crate::domain::{ItemKind, ItemSource, ItemView, StreamEvent};
use crate::service::EventSubscription;

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
