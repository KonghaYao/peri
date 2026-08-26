//! TUI-private semantic ownership for reverse interactions.
//!
//! Wire request ids identify JSON-RPC frames. [`InteractionOwner`] identifies
//! the one UI operation that may settle a reverse request.  Session boundaries,
//! prompt terminals, bridge publication and user responses all compete for that
//! same owner in this module.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use peri_acp::transport::types::RequestId;
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use super::client::AcpNotification;

pub const NEW_SESSION_EVENT_BUFFER_CAPACITY: usize = 64;

static NEXT_CLIENT_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReverseInteractionKind {
    Permission,
    Elicitation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionKind {
    New,
    Load,
    DeleteCurrent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRoute {
    Bootstrapping {
        generation: u64,
    },
    Stable {
        session_id: String,
        generation: u64,
        accepting_reverse: bool,
        prompt_epoch: u64,
    },
    Transitioning {
        kind: TransitionKind,
        from: Option<String>,
        target: Option<String>,
        generation: u64,
    },
    NoSession {
        generation: u64,
    },
}

impl SessionRoute {
    fn generation(&self) -> u64 {
        match self {
            Self::Bootstrapping { generation }
            | Self::Stable { generation, .. }
            | Self::Transitioning { generation, .. }
            | Self::NoSession { generation } => *generation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InteractionOwner {
    pub client_instance_id: u64,
    pub token: u64,
    pub session_id: String,
    pub generation: u64,
    pub prompt_epoch: u64,
    pub kind: ReverseInteractionKind,
}

impl Default for InteractionOwner {
    fn default() -> Self {
        Self {
            client_instance_id: 0,
            token: 0,
            session_id: "test-session".into(),
            generation: 0,
            prompt_epoch: 0,
            kind: ReverseInteractionKind::Permission,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMarker {
    pub client_instance_id: u64,
    pub session_id: String,
    pub generation: u64,
    pub prompt_epoch: u64,
    pub request_id: Option<String>,
}

#[derive(Debug)]
struct PendingInteractionEntry {
    owner: InteractionOwner,
    request_id: RequestId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimCause {
    UserResponse,
    BridgeReject,
    LifecycleDrain,
    TurnTerminal,
    TransportTerminal,
}

#[derive(Debug)]
pub struct ClaimedInteraction {
    pub owner: InteractionOwner,
    pub request_id: RequestId,
    pub cause: ClaimCause,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionExpiryReason {
    BridgeRejected,
    LifecycleDrain,
    TurnTerminal,
    TransportTerminal,
    ResponseTransportFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionUiOutcome {
    Resolved { result: String },
    Expired { reason: InteractionExpiryReason },
}

#[derive(Debug)]
pub struct RegisteredReverseRequest {
    pub owner: InteractionOwner,
    pub request_id_json: String,
    pub params: Value,
}

#[derive(Debug)]
pub enum RegisterDecision {
    Forward(RegisteredReverseRequest),
    Settle {
        kind: ReverseInteractionKind,
        id: RequestId,
    },
}

#[derive(Debug)]
pub enum OrdinaryDecision {
    Forward(Box<AcpNotification>),
    Buffered,
    Drop,
}

#[derive(Debug)]
struct BufferedOrdinaryNotification {
    session_id: String,
    notification: AcpNotification,
}

#[derive(Debug)]
struct InteractionLifecycleState {
    client_instance_id: u64,
    route: SessionRoute,
    next_token: u64,
    deleted_session_ids: HashSet<String>,
    pending: BTreeMap<u64, PendingInteractionEntry>,
    active_prompt: Option<PromptMarker>,
    new_buffer: VecDeque<BufferedOrdinaryNotification>,
    warned_buffer_full: bool,
}

#[derive(Clone)]
pub struct InteractionLifecycle {
    operation_gate: Arc<AsyncMutex<()>>,
    state: Arc<Mutex<InteractionLifecycleState>>,
    drop_settlement_tx: Arc<Mutex<Option<mpsc::UnboundedSender<ClaimedInteraction>>>>,
}

impl std::fmt::Debug for InteractionLifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InteractionLifecycle")
            .finish_non_exhaustive()
    }
}

impl InteractionLifecycle {
    pub fn new() -> Self {
        let client_instance_id = NEXT_CLIENT_INSTANCE_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .unwrap_or(0);
        Self {
            operation_gate: Arc::new(AsyncMutex::new(())),
            state: Arc::new(Mutex::new(InteractionLifecycleState {
                client_instance_id,
                route: SessionRoute::Bootstrapping { generation: 0 },
                next_token: 1,
                deleted_session_ids: HashSet::new(),
                pending: BTreeMap::new(),
                active_prompt: None,
                new_buffer: VecDeque::new(),
                warned_buffer_full: false,
            })),
            drop_settlement_tx: Arc::new(Mutex::new(None)),
        }
    }

    pub fn operation_gate(&self) -> &AsyncMutex<()> {
        &self.operation_gate
    }

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

    pub fn has_session(&self) -> bool {
        matches!(
            self.state.lock().unwrap().route,
            SessionRoute::Stable { .. }
        )
    }

    pub fn current_session_id(&self) -> Option<String> {
        match &self.state.lock().unwrap().route {
            SessionRoute::Stable { session_id, .. } => Some(session_id.clone()),
            SessionRoute::Transitioning { target, .. } => target.clone(),
            SessionRoute::Bootstrapping { .. } | SessionRoute::NoSession { .. } => None,
        }
    }

    pub fn stable_identity(&self) -> Option<(String, u64)> {
        match &self.state.lock().unwrap().route {
            SessionRoute::Stable {
                session_id,
                generation,
                ..
            } => Some((session_id.clone(), *generation)),
            _ => None,
        }
    }

    pub fn begin_transition(
        &self,
        kind: TransitionKind,
        target: Option<String>,
    ) -> Result<TransitionStart, &'static str> {
        let mut state = self.state.lock().unwrap();
        let generation = state
            .route
            .generation()
            .checked_add(1)
            .ok_or("session generation exhausted")?;
        let from = match &state.route {
            SessionRoute::Stable { session_id, .. } => Some(session_id.clone()),
            SessionRoute::Transitioning { target, .. } => target.clone(),
            _ => None,
        };
        state.route = SessionRoute::Transitioning {
            kind,
            from: from.clone(),
            target,
            generation,
        };
        state.active_prompt = None;
        state.new_buffer.clear();
        state.warned_buffer_full = false;
        let claims = drain_pending(&mut state, ClaimCause::LifecycleDrain, |_| true);
        Ok(TransitionStart {
            generation,
            from,
            claims,
        })
    }

    pub fn arm_transition(&self, generation: u64) -> TransitionLease {
        TransitionLease {
            lifecycle: self.clone(),
            generation,
            armed: true,
        }
    }

    pub fn commit_stable(&self, generation: u64, session_id: String) -> Vec<AcpNotification> {
        let mut state = self.state.lock().unwrap();
        if state.route.generation() != generation {
            return Vec::new();
        }
        state.route = SessionRoute::Stable {
            session_id: session_id.clone(),
            generation,
            accepting_reverse: false,
            prompt_epoch: 0,
        };
        state.active_prompt = None;
        state.warned_buffer_full = false;
        state
            .new_buffer
            .drain(..)
            .filter_map(|item| (item.session_id == session_id).then_some(item.notification))
            .collect()
    }

    pub fn fail_transition(&self, generation: u64) -> Vec<ClaimedInteraction> {
        let mut state = self.state.lock().unwrap();
        if state.route.generation() != generation {
            return Vec::new();
        }
        state.route = SessionRoute::NoSession { generation };
        state.active_prompt = None;
        state.new_buffer.clear();
        drain_pending(&mut state, ClaimCause::LifecycleDrain, |_| true)
    }

    pub fn mark_deleted(&self, session_id: &str) {
        let mut state = self.state.lock().unwrap();
        state.deleted_session_ids.insert(session_id.to_string());
    }

    pub fn is_current_session(&self, session_id: &str) -> bool {
        if session_id.is_empty() {
            return true;
        }
        let state = self.state.lock().unwrap();
        if state.deleted_session_ids.contains(session_id) {
            return false;
        }
        match &state.route {
            SessionRoute::Stable {
                session_id: current,
                ..
            } => current == session_id,
            SessionRoute::Transitioning {
                kind: TransitionKind::Load,
                target: Some(target),
                ..
            } => target == session_id,
            _ => false,
        }
    }

    pub fn route_ordinary(
        &self,
        session_id: String,
        notification: AcpNotification,
    ) -> OrdinaryDecision {
        if session_id.is_empty() {
            return OrdinaryDecision::Forward(Box::new(notification));
        }
        let mut state = self.state.lock().unwrap();
        if state.deleted_session_ids.contains(&session_id) {
            return OrdinaryDecision::Drop;
        }
        match &state.route {
            SessionRoute::Stable {
                session_id: current,
                ..
            } if current == &session_id => OrdinaryDecision::Forward(Box::new(notification)),
            SessionRoute::Transitioning {
                kind: TransitionKind::Load,
                target: Some(target),
                ..
            } if target == &session_id => OrdinaryDecision::Forward(Box::new(notification)),
            SessionRoute::Transitioning {
                kind: TransitionKind::New,
                target: None,
                ..
            } => {
                if state.new_buffer.len() < NEW_SESSION_EVENT_BUFFER_CAPACITY {
                    state.new_buffer.push_back(BufferedOrdinaryNotification {
                        session_id,
                        notification,
                    });
                } else if !state.warned_buffer_full {
                    state.warned_buffer_full = true;
                    tracing::warn!(
                        capacity = NEW_SESSION_EVENT_BUFFER_CAPACITY,
                        "ACP client: new-session ordinary notification buffer full"
                    );
                }
                OrdinaryDecision::Buffered
            }
            _ => OrdinaryDecision::Drop,
        }
    }

    pub fn register_reverse(
        &self,
        kind: ReverseInteractionKind,
        id: RequestId,
        session_id: Option<&str>,
        params: Value,
    ) -> RegisterDecision {
        let mut state = self.state.lock().unwrap();
        let Some(session_id) = session_id else {
            return RegisterDecision::Settle { kind, id };
        };
        if state.client_instance_id == 0 || state.deleted_session_ids.contains(session_id) {
            return RegisterDecision::Settle { kind, id };
        }
        let (generation, prompt_epoch) = match &state.route {
            SessionRoute::Stable {
                session_id: current,
                generation,
                accepting_reverse: true,
                prompt_epoch,
            } if current == session_id => (*generation, *prompt_epoch),
            _ => return RegisterDecision::Settle { kind, id },
        };
        let Some(next_token) = state.next_token.checked_add(1) else {
            return RegisterDecision::Settle { kind, id };
        };
        let token = state.next_token;
        state.next_token = next_token;
        let owner = InteractionOwner {
            client_instance_id: state.client_instance_id,
            token,
            session_id: session_id.to_string(),
            generation,
            prompt_epoch,
            kind,
        };
        let request_id_json = serde_json::to_string(&id)
            .expect("RequestId Number/String serialization is infallible");
        state.pending.insert(
            token,
            PendingInteractionEntry {
                owner: owner.clone(),
                request_id: id,
            },
        );
        RegisterDecision::Forward(RegisteredReverseRequest {
            owner,
            request_id_json,
            params,
        })
    }

    pub fn is_pending_owner(&self, owner: &InteractionOwner) -> bool {
        self.state
            .lock()
            .unwrap()
            .pending
            .get(&owner.token)
            .is_some_and(|entry| entry.owner == *owner)
    }

    pub fn claim(&self, owner: &InteractionOwner, cause: ClaimCause) -> Option<ClaimedInteraction> {
        let mut state = self.state.lock().unwrap();
        let matches = state
            .pending
            .get(&owner.token)
            .is_some_and(|entry| entry.owner == *owner);
        if !matches {
            return None;
        }
        let entry = state.pending.remove(&owner.token).unwrap();
        Some(ClaimedInteraction {
            owner: entry.owner,
            request_id: entry.request_id,
            cause,
        })
    }

    pub fn open_prompt(&self, request_id: Option<String>) -> Result<PromptLease, &'static str> {
        let mut state = self.state.lock().unwrap();
        let client_instance_id = state.client_instance_id;
        let (session_id, generation, next_epoch) = match &state.route {
            SessionRoute::Stable {
                session_id,
                generation,
                prompt_epoch,
                ..
            } => (
                session_id.clone(),
                *generation,
                prompt_epoch
                    .checked_add(1)
                    .ok_or("prompt epoch exhausted")?,
            ),
            _ => return Err("no stable session"),
        };
        let marker = PromptMarker {
            client_instance_id,
            session_id,
            generation,
            prompt_epoch: next_epoch,
            request_id,
        };
        if let SessionRoute::Stable {
            accepting_reverse,
            prompt_epoch,
            ..
        } = &mut state.route
        {
            *accepting_reverse = true;
            *prompt_epoch = next_epoch;
        }
        state.active_prompt = Some(marker.clone());
        Ok(PromptLease {
            lifecycle: self.clone(),
            marker,
            armed: true,
        })
    }

    pub fn close_prompt_exact(
        &self,
        marker: &PromptMarker,
        cause: ClaimCause,
    ) -> Vec<ClaimedInteraction> {
        let mut state = self.state.lock().unwrap();
        if state.active_prompt.as_ref() != Some(marker) {
            return Vec::new();
        }
        state.active_prompt = None;
        if let SessionRoute::Stable {
            session_id,
            generation,
            accepting_reverse,
            prompt_epoch,
        } = &mut state.route
            && session_id == &marker.session_id
            && *generation == marker.generation
            && *prompt_epoch == marker.prompt_epoch
        {
            *accepting_reverse = false;
        }
        drain_pending(&mut state, cause, |owner| {
            owner.client_instance_id == marker.client_instance_id
                && owner.session_id == marker.session_id
                && owner.generation == marker.generation
                && owner.prompt_epoch == marker.prompt_epoch
        })
    }

    pub fn close_prompt_by_wire_identity(
        &self,
        session_id: &str,
        request_id: Option<&str>,
    ) -> Vec<ClaimedInteraction> {
        let Some(request_id) = request_id.filter(|id| !id.is_empty()) else {
            return Vec::new();
        };
        let marker = {
            let state = self.state.lock().unwrap();
            state.active_prompt.as_ref().and_then(|marker| {
                (marker.session_id == session_id
                    && marker.request_id.as_deref() == Some(request_id))
                .then(|| marker.clone())
            })
        };
        marker
            .map(|marker| self.close_prompt_exact(&marker, ClaimCause::TurnTerminal))
            .unwrap_or_default()
    }

    pub fn cancel_active_prompt(&self) -> Vec<ClaimedInteraction> {
        let marker = self.state.lock().unwrap().active_prompt.clone();
        marker
            .map(|marker| self.close_prompt_exact(&marker, ClaimCause::TurnTerminal))
            .unwrap_or_default()
    }

    pub fn transport_terminal(&self) -> Vec<ClaimedInteraction> {
        let mut state = self.state.lock().unwrap();
        if let SessionRoute::Stable {
            accepting_reverse, ..
        } = &mut state.route
        {
            *accepting_reverse = false;
        }
        state.active_prompt = None;
        drain_pending(&mut state, ClaimCause::TransportTerminal, |_| true)
    }

    #[cfg(test)]
    pub(crate) fn force_stable(&self, session_id: &str, accepting_reverse: bool) {
        let mut state = self.state.lock().unwrap();
        state.route = SessionRoute::Stable {
            session_id: session_id.to_string(),
            generation: 1,
            accepting_reverse,
            prompt_epoch: u64::from(accepting_reverse),
        };
        state.active_prompt = accepting_reverse.then(|| PromptMarker {
            client_instance_id: state.client_instance_id,
            session_id: session_id.to_string(),
            generation: 1,
            prompt_epoch: 1,
            request_id: Some("test-prompt".into()),
        });
    }
}

impl Default for InteractionLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

fn drain_pending(
    state: &mut InteractionLifecycleState,
    cause: ClaimCause,
    mut predicate: impl FnMut(&InteractionOwner) -> bool,
) -> Vec<ClaimedInteraction> {
    let tokens: Vec<u64> = state
        .pending
        .iter()
        .filter_map(|(token, entry)| predicate(&entry.owner).then_some(*token))
        .collect();
    tokens
        .into_iter()
        .filter_map(|token| state.pending.remove(&token))
        .map(|entry| ClaimedInteraction {
            owner: entry.owner,
            request_id: entry.request_id,
            cause,
        })
        .collect()
}

#[derive(Debug)]
pub struct TransitionStart {
    pub generation: u64,
    pub from: Option<String>,
    pub claims: Vec<ClaimedInteraction>,
}

pub struct PromptLease {
    lifecycle: InteractionLifecycle,
    marker: PromptMarker,
    armed: bool,
}

pub struct TransitionLease {
    lifecycle: InteractionLifecycle,
    generation: u64,
    armed: bool,
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

#[cfg(test)]
#[path = "interaction_lifecycle_test.rs"]
mod tests;
