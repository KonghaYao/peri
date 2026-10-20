//! Cancellation-safe settlement for interactions claimed by synchronous Drop.

use std::sync::{Arc, Weak};

use peri_acp::transport::{AcpTransport, mpsc::MpscClientTransport};
use tokio::sync::mpsc;

use super::client::AcpNotification;
use super::interaction_lifecycle::{
    ClaimCause, ClaimedInteraction, InteractionExpiryReason, InteractionUiOutcome,
    ReverseInteractionKind,
};
use super::interaction_response::{elicitation_cancel_response, permission_cancelled_response};

pub fn spawn_settlement_worker(
    transport: Weak<MpscClientTransport>,
    mut rx: mpsc::UnboundedReceiver<ClaimedInteraction>,
    notification_tx: Arc<std::sync::Mutex<Option<mpsc::WeakSender<AcpNotification>>>>,
) {
    tokio::spawn(async move {
        while let Some(claimed) = rx.recv().await {
            let outcome = if claimed.cause == ClaimCause::TransportTerminal {
                InteractionUiOutcome::Expired {
                    reason: InteractionExpiryReason::TransportTerminal,
                }
            } else if let Some(transport) = transport.upgrade() {
                let response = match claimed.owner.kind {
                    ReverseInteractionKind::Permission => permission_cancelled_response(),
                    ReverseInteractionKind::Elicitation => elicitation_cancel_response(),
                };
                match transport
                    .send_response(claimed.request_id, Ok(response))
                    .await
                {
                    Ok(()) => InteractionUiOutcome::Expired {
                        reason: expiry_for_cause(claimed.cause),
                    },
                    Err(error) => {
                        tracing::warn!(error = %error, "interaction Drop settlement failed");
                        InteractionUiOutcome::Expired {
                            reason: InteractionExpiryReason::ResponseTransportFailed,
                        }
                    }
                }
            } else {
                InteractionUiOutcome::Expired {
                    reason: InteractionExpiryReason::TransportTerminal,
                }
            };
            if let Some(tx) = notification_tx.lock().unwrap().as_ref()
                && let Some(tx) = tx.upgrade()
            {
                match tx.try_send(AcpNotification::InteractionTerminal {
                    owner: claimed.owner,
                    outcome,
                }) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        tracing::error!(
                            event = "acp_client_notification_backpressure",
                            "结算通知队列已满，关闭连接以避免无界增长"
                        );
                        if let Some(transport) = transport.upgrade() {
                            transport.close();
                        }
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {}
                }
            }
        }
    });
}

pub fn expiry_for_cause(cause: ClaimCause) -> InteractionExpiryReason {
    match cause {
        ClaimCause::BridgeReject => InteractionExpiryReason::BridgeRejected,
        ClaimCause::LifecycleDrain => InteractionExpiryReason::LifecycleDrain,
        ClaimCause::TurnTerminal => InteractionExpiryReason::TurnTerminal,
        ClaimCause::TransportTerminal => InteractionExpiryReason::TransportTerminal,
        ClaimCause::UserResponse => InteractionExpiryReason::ResponseTransportFailed,
    }
}

#[cfg(test)]
#[path = "interaction_settlement_test.rs"]
mod tests;
