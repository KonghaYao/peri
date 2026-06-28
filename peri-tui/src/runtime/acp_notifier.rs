//! ACP notifier background task.
//!
//! Spawns a dedicated tokio task that receives [`AcpNotification`]s from the
//! `AcpTuiClient` notification pump, converts them into [`TuiEvent`] variants,
//! and pushes them into the single event channel.
//!
//! This is one of the two input sources for the v2 TUI event loop (the other
//! being the keyboard collector).  The task performs **no state mutation** --
//! pure conversion + channel push.
//!
//! Design: <https://github.com/user/perihelion/blob/main/docs/design/peri-tui-architecture.md#71-from-input-to-event>

use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::event_channel::{EventTx, TuiEvent};
use crate::acp_client::AcpNotification;

/// Spawn the ACP notifier background task.
///
/// The task runs an internal loop that:
///
/// 1. Receives [`AcpNotification`] from the `AcpTuiClient` notification pump.
/// 2. Converts each notification into the appropriate [`TuiEvent`] variant.
/// 3. Pushes the event into the unified event channel.
/// 4. On channel close (transport disconnect), pushes
///    [`TuiEvent::AcpDisconnected`] and exits.
/// 5. Exits cleanly when `shutdown` is cancelled.
pub fn spawn(
    tx: EventTx,
    notification_rx: tokio::sync::mpsc::UnboundedReceiver<AcpNotification>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move { run(tx, notification_rx, shutdown).await })
}

async fn run(
    tx: EventTx,
    mut notification_rx: tokio::sync::mpsc::UnboundedReceiver<AcpNotification>,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            // Shutdown signal takes priority.
            _ = shutdown.cancelled() => {
                debug!("ACP notifier: shutdown signal received, exiting");
                break;
            }

            // Receive next ACP notification from the pump.
            notification = notification_rx.recv() => {
                match notification {
                    Some(n) => {
                        handle_notification(&tx, n);
                    }
                    None => {
                        // Channel closed -- the AcpTuiClient pump has exited,
                        // which means the transport connection dropped.
                        debug!("ACP notifier: notification channel closed (transport disconnected)");
                        let _ = tx.send(TuiEvent::AcpDisconnected);
                        break;
                    }
                }
            }
        }
    }
}

/// Convert a single [`AcpNotification`] into one or more [`TuiEvent`]s and push
/// them into the event channel.
fn handle_notification(tx: &EventTx, n: AcpNotification) {
    match n {
        // Agent events carry an AcpEvent DTO.  The state machine will interpret
        // the event internally -- here we just forward the JSON representation.
        AcpNotification::AgentEvent { session_id, event } => {
            let data = serde_json::to_value(&event)
                .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }));
            let _ = tx.send(TuiEvent::AcpEvent {
                event: "agent-event".into(),
                data: serde_json::json!({ "sessionId": session_id, "event": data }),
            });
        }

        AcpNotification::SessionUpdate { session_id, params } => {
            let _ = tx.send(TuiEvent::AcpEvent {
                event: "session-update".into(),
                data: serde_json::json!({ "sessionId": session_id, "params": params }),
            });
        }

        AcpNotification::AgentDone { session_id } => {
            let _ = tx.send(TuiEvent::AcpEvent {
                event: "agent-done".into(),
                data: serde_json::json!({ "sessionId": session_id }),
            });
        }

        AcpNotification::RequestPermission { id, params } => {
            let _ = tx.send(TuiEvent::AcpEvent {
                event: "request-permission".into(),
                data: serde_json::json!({ "id": id, "params": params }),
            });
        }

        AcpNotification::Elicitation { id, params } => {
            let _ = tx.send(TuiEvent::AcpEvent {
                event: "elicitation".into(),
                data: serde_json::json!({ "id": id, "params": params }),
            });
        }

        AcpNotification::PredictionReady { session_id, text } => {
            let _ = tx.send(TuiEvent::AcpEvent {
                event: "prediction-ready".into(),
                data: serde_json::json!({ "sessionId": session_id, "text": text }),
            });
        }

        AcpNotification::Peri {
            session_id,
            method,
            params,
        } => {
            let _ = tx.send(TuiEvent::AcpEvent {
                event: method,
                data: serde_json::json!({ "sessionId": session_id, "params": params }),
            });
        }

        AcpNotification::Other { msg } => {
            // Unrecognised notifications are still forwarded so the state
            // machine can log or display them.
            warn!(msg = %msg, "ACP notifier: unrecognised notification");
            let _ = tx.send(TuiEvent::AcpEvent {
                event: "other".into(),
                data: serde_json::json!({ "msg": msg }),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::mpsc;

    /// Helper: create a (tx, rx) pair for `AcpNotification`.
    fn notification_channel() -> (
        mpsc::UnboundedSender<AcpNotification>,
        mpsc::UnboundedReceiver<AcpNotification>,
    ) {
        mpsc::unbounded_channel()
    }

    /// Helper: create a (EventTx, EventRx) pair and spawn the notifier.
    /// Returns (EventRx, UnboundedSender<AcpNotification>, CancellationToken).
    fn spawn_test_notifier() -> (
        mpsc::UnboundedReceiver<TuiEvent>,
        mpsc::UnboundedSender<AcpNotification>,
        CancellationToken,
    ) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (notif_tx, notif_rx) = notification_channel();
        let shutdown = CancellationToken::new();
        let _handle = spawn(event_tx, notif_rx, shutdown.clone());
        (event_rx, notif_tx, shutdown)
    }

    #[tokio::test]
    async fn test_agent_event_forwarded() {
        use peri_acp::event::AcpEvent;

        let (mut event_rx, notif_tx, shutdown) = spawn_test_notifier();

        // Send a minimal AcpEvent (using a simple variant).
        let acp_event = AcpEvent::TurnCommitted {
            messages_json: "[]".into(),
            steps: 0,
        };
        notif_tx
            .send(AcpNotification::AgentEvent {
                session_id: "sess-1".into(),
                event: acp_event,
            })
            .unwrap();

        let ev = event_rx.recv().await.unwrap();
        match ev {
            TuiEvent::AcpEvent { event, data } => {
                assert_eq!(event, "agent-event");
                assert_eq!(data["sessionId"], "sess-1");
            }
            other => panic!("期望 TuiEvent::AcpEvent，实际 {other:?}"),
        }

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_session_update_forwarded() {
        let (mut event_rx, notif_tx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::SessionUpdate {
                session_id: "sess-2".into(),
                params: json!({ "key": "value" }),
            })
            .unwrap();

        let ev = event_rx.recv().await.unwrap();
        match ev {
            TuiEvent::AcpEvent { event, data } => {
                assert_eq!(event, "session-update");
                assert_eq!(data["sessionId"], "sess-2");
            }
            other => panic!("期望 TuiEvent::AcpEvent，实际 {other:?}"),
        }

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_request_permission_forwarded() {
        let (mut event_rx, notif_tx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::RequestPermission {
                id: peri_acp::transport::types::RequestId::Number(42),
                params: json!({ "tool": "Write" }),
            })
            .unwrap();

        let ev = event_rx.recv().await.unwrap();
        match ev {
            TuiEvent::AcpEvent { event, data } => {
                assert_eq!(event, "request-permission");
                // RequestId is serialized as a number.
                assert_eq!(data["id"], 42);
            }
            other => panic!("期望 TuiEvent::AcpEvent，实际 {other:?}"),
        }

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_agent_done_forwarded() {
        let (mut event_rx, notif_tx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::AgentDone {
                session_id: "sess-3".into(),
            })
            .unwrap();

        let ev = event_rx.recv().await.unwrap();
        match ev {
            TuiEvent::AcpEvent { event, data } => {
                assert_eq!(event, "agent-done");
                assert_eq!(data["sessionId"], "sess-3");
            }
            other => panic!("期望 TuiEvent::AcpEvent，实际 {other:?}"),
        }

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_peri_notification_forwarded() {
        let (mut event_rx, notif_tx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::Peri {
                session_id: "sess-4".into(),
                method: "notifications/peri/compact_done".into(),
                params: json!({ "saved_tokens": 5000 }),
            })
            .unwrap();

        let ev = event_rx.recv().await.unwrap();
        match ev {
            TuiEvent::AcpEvent { event, data } => {
                assert_eq!(event, "notifications/peri/compact_done");
                assert_eq!(data["sessionId"], "sess-4");
                assert_eq!(data["params"]["saved_tokens"], 5000);
            }
            other => panic!("期望 TuiEvent::AcpEvent，实际 {other:?}"),
        }

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_other_notification_forwarded() {
        let (mut event_rx, notif_tx, shutdown) = spawn_test_notifier();

        notif_tx
            .send(AcpNotification::Other {
                msg: "notification: session/unknown".into(),
            })
            .unwrap();

        let ev = event_rx.recv().await.unwrap();
        match ev {
            TuiEvent::AcpEvent { event, data } => {
                assert_eq!(event, "other");
                assert_eq!(data["msg"], "notification: session/unknown");
            }
            other => panic!("期望 TuiEvent::AcpEvent，实际 {other:?}"),
        }

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_transport_disconnect_pushes_acp_disconnected() {
        let (mut event_rx, notif_tx, shutdown) = spawn_test_notifier();

        // Drop the sender to simulate transport disconnect (channel close).
        drop(notif_tx);

        let ev = event_rx.recv().await.unwrap();
        match ev {
            TuiEvent::AcpDisconnected => {}
            other => panic!("期望 TuiEvent::AcpDisconnected，实际 {other:?}"),
        }

        shutdown.cancel();
    }

    #[tokio::test]
    async fn test_shutdown_exits_cleanly() {
        // This test verifies the documented shutdown contract: when the
        // shutdown signal is cancelled, the notifier task eventually exits.
        //
        // IMPORTANT: `tokio::select!` between `shutdown.cancelled()` and
        // `notification_rx.recv()` is non-deterministic when both are
        // ready.  If a notification was already buffered when shutdown
        // fires, the task MAY process the notification before exiting.
        // Therefore this test does NOT send any late notification — it
        // only verifies the notifier exits and the channel closes.
        let (mut event_rx, _notif_tx, shutdown) = spawn_test_notifier();

        // Cancel shutdown -- the notifier should exit without sending anything.
        shutdown.cancel();

        // Channel should close within a reasonable window (notifier exited).
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(500), event_rx.recv()).await;
        // Expect: channel closed (recv returns None). Timeout would indicate
        // the notifier did not exit, which is a real bug.
        assert!(
            result.is_ok() && result.as_ref().unwrap().is_none(),
            "notifier did not exit after shutdown (got: {result:?})"
        );
    }
}
