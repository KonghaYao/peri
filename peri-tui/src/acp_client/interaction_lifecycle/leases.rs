//! RAII leases that settle owned work when a caller future is dropped.

use std::collections::VecDeque;

use tokio::sync::mpsc;

use super::{ClaimCause, ClaimedInteraction, InteractionLifecycle, PromptMarker};

impl InteractionLifecycle {
    pub fn install_drop_settlement_sender(&self, tx: mpsc::UnboundedSender<ClaimedInteraction>) {
        *self.drop_settlement_tx.lock().unwrap() = Some(tx);
    }

    fn enqueue_drop_claims(&self, claims: Vec<ClaimedInteraction>) {
        let tx = self.drop_settlement_tx.lock().unwrap().clone();
        if let Some(tx) = tx {
            for claim in claims {
                let _ = tx.send(claim);
            }
        }
    }

    /// Arm an already-owned batch before the first transport await. If the
    /// caller future is dropped, both the in-flight claim and every unstarted
    /// claim are handed to the cancellation-safe settlement worker.
    pub fn arm_claimed_batch(&self, claims: Vec<ClaimedInteraction>) -> ClaimedBatchLease {
        ClaimedBatchLease {
            lifecycle: self.clone(),
            claims: claims.into(),
        }
    }
}

pub struct PromptLease {
    pub(super) lifecycle: InteractionLifecycle,
    pub(super) marker: PromptMarker,
    pub(super) armed: bool,
}

pub struct TransitionLease {
    pub(super) lifecycle: InteractionLifecycle,
    pub(super) generation: u64,
    pub(super) armed: bool,
}

pub struct ClaimedBatchLease {
    lifecycle: InteractionLifecycle,
    claims: VecDeque<ClaimedInteraction>,
}

pub struct ClaimedSettlementLease {
    lifecycle: InteractionLifecycle,
    claim: Option<ClaimedInteraction>,
}

impl ClaimedBatchLease {
    pub fn next_claim(&mut self) -> Option<ClaimedSettlementLease> {
        self.claims.pop_front().map(|claim| ClaimedSettlementLease {
            lifecycle: self.lifecycle.clone(),
            claim: Some(claim),
        })
    }
}

impl Drop for ClaimedBatchLease {
    fn drop(&mut self) {
        self.lifecycle
            .enqueue_drop_claims(self.claims.drain(..).collect());
    }
}

impl ClaimedSettlementLease {
    pub fn claim(&self) -> &ClaimedInteraction {
        self.claim
            .as_ref()
            .expect("claimed settlement lease is armed")
    }

    pub fn complete(mut self) -> ClaimedInteraction {
        self.claim
            .take()
            .expect("claimed settlement lease is armed")
    }
}

impl Drop for ClaimedSettlementLease {
    fn drop(&mut self) {
        if let Some(claim) = self.claim.take() {
            self.lifecycle.enqueue_drop_claims(vec![claim]);
        }
    }
}

impl TransitionLease {
    pub fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for TransitionLease {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let claims = self.lifecycle.fail_transition(self.generation);
        self.lifecycle.enqueue_drop_claims(claims);
        self.armed = false;
    }
}

impl PromptLease {
    pub fn marker(&self) -> &PromptMarker {
        &self.marker
    }

    pub fn finish(mut self) -> Vec<ClaimedInteraction> {
        let claims = self
            .lifecycle
            .close_prompt_exact(&self.marker, ClaimCause::TurnTerminal);
        self.armed = false;
        claims
    }
}

impl Drop for PromptLease {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let claims = self
            .lifecycle
            .close_prompt_exact(&self.marker, ClaimCause::TurnTerminal);
        self.lifecycle.enqueue_drop_claims(claims);
        self.armed = false;
    }
}
