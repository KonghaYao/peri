use super::*;
use crate::acp_client::AcpTuiClient;
use crate::kit::acp_notifier::spawn_kit_notifier;
use crate::kit::tui_render_unit::TuiRenderUnit;
use peri_acp::event::AcpEvent;
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair, types::IncomingMessage};
use serde_json::json;
use std::time::Duration;

async fn cancel_streaming_output(chunk_kinds: &[&str]) {
    let _restore = ReplayAtomsGuard::new();
    let (transport, server) = mpsc_transport_pair();
    let (client, notifications, notification_rx) = AcpTuiClient::new(transport);
    client.force_stable_for_test("s1", false);
    client.spawn_pump(notifications);
    let shutdown = CancellationToken::new();
    let (bridge_tx, mut bridge_rx) = mpsc::unbounded_channel();
    let notifier = spawn_kit_notifier(notification_rx, bridge_tx, shutdown.clone());
    let mut state = scheduler_state();

    server
        .send_notification(
            "peri/agent_event",
            json!({"sessionId":"s1", "event": AcpEvent::ExecutionStarted {
                generation:"generation-1".into(), request_id:"streaming-1".into(),
            }}),
        )
        .await
        .unwrap();
    let started = tokio::time::timeout(Duration::from_secs(2), bridge_rx.recv())
        .await
        .unwrap()
        .unwrap();
    acp_events::dispatch_for_bridge(&mut state, &started.event);
    assert_eq!(state.current_request_id.as_deref(), Some("streaming-1"));
    assert!(atoms::ACP_STATE.state().read().is_loading);

    for chunk_kind in chunk_kinds {
        server
            .send_notification(
                "session/update",
                json!({"sessionId":"s1", "update":{
                    "sessionUpdate":chunk_kind,
                    "content":{"type":"text", "text":"partial streamed output"},
                }}),
            )
            .await
            .unwrap();
        let chunk = tokio::time::timeout(Duration::from_secs(2), bridge_rx.recv())
            .await
            .unwrap()
            .unwrap();
        acp_events::dispatch_for_bridge(&mut state, &chunk.event);
    }
    assert!(!state.current_turn.is_empty());

    client.cancel().await.unwrap();
    assert!(
        matches!(server.recv().await, Some(IncomingMessage::Notification {
        method, params,
    }) if method == "session/cancel" && params["sessionId"] == "s1")
    );
    assert!(
        atoms::ACP_STATE.state().read().is_loading,
        "notification delivery is not a terminal result"
    );
    server
        .send_notification(
            "peri/agent_event_done",
            json!({"sessionId":"s1", "requestId":"streaming-1", "stopReason":"cancelled"}),
        )
        .await
        .unwrap();
    let terminal = tokio::time::timeout(Duration::from_millis(200), bridge_rx.recv()).await;
    shutdown.cancel();
    notifier.await.unwrap();
    client.close();
    let terminal = terminal
        .expect("cancelled streaming request must forward its terminal notification")
        .unwrap();
    assert!(
        matches!(&terminal.event, AcpEventData::TurnInterrupted { request_id, .. }
        if request_id.as_deref() == Some("streaming-1"))
    );
    acp_events::dispatch_for_bridge(&mut state, &terminal.event);
    assert!(!atoms::ACP_STATE.state().read().is_loading);
    assert!(state.current_turn.is_empty());
    for chunk_kind in chunk_kinds {
        assert!(
            state.committed.iter().any(|unit| {
                let TuiRenderUnit::TuiAssistantBubble(bubble) = unit else {
                    return false;
                };
                if *chunk_kind == "agent_message_chunk" {
                    bubble.text == "partial streamed output" && bubble.started_at.is_none()
                } else {
                    bubble.reasoning.as_ref().is_some_and(|reasoning| {
                        reasoning.text == "partial streamed output" && !reasoning.is_running
                    })
                }
            }),
            "cancelled output must remain visible with its streaming animation stopped"
        );
    }
}

#[tokio::test]
#[serial]
async fn cancel_streaming_thinking_forwards_terminal_and_clears_loading() {
    cancel_streaming_output(&["agent_thought_chunk"]).await;
}

#[tokio::test]
#[serial]
async fn cancel_streaming_text_forwards_terminal_and_clears_loading() {
    cancel_streaming_output(&["agent_message_chunk"]).await;
}

#[tokio::test]
#[serial]
async fn cancel_streaming_thinking_then_text_preserves_both_and_clears_loading() {
    cancel_streaming_output(&["agent_thought_chunk", "agent_message_chunk"]).await;
}
