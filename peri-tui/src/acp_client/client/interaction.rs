//! Owner-qualified interaction responses, bridge publication, and owned settlement.

use peri_acp::transport::{AcpTransport, types::AcpError};
use serde_json::Value;
use tracing::warn;

use super::super::interaction_lifecycle::{
    ClaimCause, ClaimedInteraction, InteractionExpiryReason, InteractionOwner,
    InteractionUiOutcome, ReverseInteractionKind,
};
use super::super::interaction_response::{
    elicitation_cancel_response, permission_cancelled_response,
};
use super::super::interaction_settlement::expiry_for_cause;
use super::{AcpNotification, AcpTuiClient, ClientProjectionMode};

impl AcpTuiClient {
    /// Claim and settle an interaction. A stale owner is a successful no-op.
    pub async fn respond_interaction(
        &self,
        owner: &InteractionOwner,
        response: Value,
        result: String,
    ) -> Result<bool, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        let Some(claimed) = self.lifecycle.claim(owner, ClaimCause::UserResponse) else {
            return Ok(false);
        };
        let wire_result = self
            .transport
            .send_response(claimed.request_id, Ok(response))
            .await;
        let outcome = match &wire_result {
            Ok(()) => InteractionUiOutcome::Resolved { result },
            Err(_) => InteractionUiOutcome::Expired {
                reason: InteractionExpiryReason::ResponseTransportFailed,
            },
        };
        self.emit_terminal(claimed.owner, outcome);
        wire_result.map(|_| true)
    }

    /// Ordered bridge publication seam. The gate remains held until all atom /
    /// popup / panel / durable-block publication in `publish` has completed.
    pub async fn publish_if_owned(&self, owner: &InteractionOwner, publish: impl FnOnce()) -> bool {
        let _operation = self.lifecycle.operation_gate().lock().await;
        let projection_matches = self.projection_mode == ClientProjectionMode::Interactive
            && crate::kit::atoms::ACTIVE_SESSION_ID.state().read().as_str() == owner.session_id;
        if self.lifecycle.is_pending_owner(owner) && projection_matches {
            publish();
            return true;
        }
        if let Some(claimed) = self.lifecycle.claim(owner, ClaimCause::BridgeReject) {
            self.settle_claims_owned(vec![claimed]).await;
        }
        false
    }

    pub async fn reject_interaction(&self, owner: &InteractionOwner) {
        let _operation = self.lifecycle.operation_gate().lock().await;
        if let Some(claimed) = self.lifecycle.claim(owner, ClaimCause::BridgeReject) {
            self.settle_claims_owned(vec![claimed]).await;
        }
    }

    fn emit_terminal(&self, owner: InteractionOwner, outcome: InteractionUiOutcome) {
        let weak = self.notification_weak.lock().unwrap().clone();
        if let Some(weak) = weak
            && let Some(tx) = weak.upgrade()
        {
            let _ = tx.send(AcpNotification::InteractionTerminal { owner, outcome });
        }
    }

    pub(super) async fn settle_claims_owned(&self, claims: Vec<ClaimedInteraction>) {
        let mut batch = self.lifecycle.arm_claimed_batch(claims);
        while let Some(lease) = batch.next_claim() {
            let claim = lease.claim();
            let response = match claim.owner.kind {
                ReverseInteractionKind::Permission => permission_cancelled_response(),
                ReverseInteractionKind::Elicitation => elicitation_cancel_response(),
            };
            let outcome = match self
                .transport
                .send_response(claim.request_id.clone(), Ok(response))
                .await
            {
                Ok(()) => InteractionUiOutcome::Expired {
                    reason: expiry_for_cause(claim.cause),
                },
                Err(error) => {
                    warn!(error = %error, "failed to settle owned interaction");
                    InteractionUiOutcome::Expired {
                        reason: InteractionExpiryReason::ResponseTransportFailed,
                    }
                }
            };
            let claim = lease.complete();
            self.emit_terminal(claim.owner, outcome);
        }
    }
}
