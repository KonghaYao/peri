//! Reverse interaction registry: owner allocation and first-claim settlement.

use peri_acp::transport::types::RequestId;
use serde_json::Value;

use super::{
    ClaimCause, ClaimedInteraction, InteractionLifecycle, InteractionLifecycleState,
    InteractionOwner, PendingInteractionEntry, RegisterDecision, RegisteredReverseRequest,
    ReverseInteractionKind, SessionRoute,
};

impl InteractionLifecycle {
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
                ..
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

    /// 服务端撤销只给出 wire request id（`$/cancel_request`），按 id 找回并认领；
    /// 已被用户响应或生命周期结算的请求返回 `None`（first-claim 语义不变）。
    pub fn claim_by_request_id(
        &self,
        request_id: &RequestId,
        cause: ClaimCause,
    ) -> Option<ClaimedInteraction> {
        let mut state = self.state.lock().unwrap();
        let token = state
            .pending
            .iter()
            .find(|(_, entry)| &entry.request_id == request_id)
            .map(|(token, _)| *token)?;
        let entry = state.pending.remove(&token)?;
        Some(ClaimedInteraction {
            owner: entry.owner,
            request_id: entry.request_id,
            cause,
        })
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
}

pub(super) fn drain_pending(
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
