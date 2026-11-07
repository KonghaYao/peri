//! Prompt and execution ownership for the active stable session.

use std::collections::HashSet;

use super::leases::PromptLease;
use super::reverse::drain_pending;
use super::{
    ClaimCause, ClaimedInteraction, ExecutionRuns, InteractionLifecycle, InteractionLifecycleState,
    PromptMarker, SessionRoute, UserInputSnapshotIdentity,
};

impl InteractionLifecycle {
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

    /// 只绑定当前 stable route 发出的快照，迟到响应不能重新打开旧会话。
    pub(crate) fn bind_user_input_generation(
        &self,
        session_id: &str,
        local_generation: u64,
        generation: &str,
    ) -> bool {
        let mut state = self.state.lock().unwrap();
        if generation.is_empty()
            || !matches!(&state.route, SessionRoute::Stable { session_id: current, generation: local, .. }
                if current == session_id && *local == local_generation)
            || state.deleted_session_ids.contains(session_id)
        {
            return false;
        }
        let runs = state
            .execution_runs
            .entry(session_id.to_owned())
            .or_insert_with(|| ExecutionRuns {
                generation: generation.to_owned(),
                local_generation,
                latest_request: None,
                pending_terminal_request: None,
                managed_input: false,
                retired: HashSet::new(),
            });
        if runs.generation != generation {
            // 新服务端实例只在一次明确的本地会话边界后重新绑定。
            if runs.local_generation == local_generation {
                return false;
            }
            runs.generation = generation.to_owned();
            runs.latest_request = None;
            runs.pending_terminal_request = None;
            runs.retired.clear();
        }
        runs.local_generation = local_generation;
        true
    }

    pub(crate) fn matches_user_input_generation(&self, session_id: &str, generation: &str) -> bool {
        let state = self.state.lock().unwrap();
        if state.deleted_session_ids.contains(session_id) {
            return false;
        }
        matches!(&state.route, SessionRoute::Stable { session_id: current, generation: local, .. }
            if current == session_id && state.execution_runs.get(session_id).is_some_and(|runs|
                runs.local_generation == *local && runs.generation == generation))
    }

    pub(crate) fn user_input_snapshot_identity(
        &self,
        session_id: &str,
    ) -> Option<UserInputSnapshotIdentity> {
        let state = self.state.lock().unwrap();
        let SessionRoute::Stable {
            session_id: current,
            generation: local,
            prompt_epoch,
            ..
        } = &state.route
        else {
            return None;
        };
        let runs = state.execution_runs.get(session_id)?;
        (current == session_id
            && runs.local_generation == *local
            && !state.deleted_session_ids.contains(session_id))
        .then(|| UserInputSnapshotIdentity {
            client_instance_id: state.client_instance_id,
            session_id: session_id.to_owned(),
            local_generation: *local,
            generation: runs.generation.clone(),
            prompt_epoch: *prompt_epoch,
            active_prompt: state.active_prompt.clone(),
        })
    }

    /// 服务端已开始的 execution 由 done/Stop/会话边界结束，无短 RPC 的 RAII lease。
    /// 返回 None 表示重复或陈旧事件，调用方不能再次发布 loading 状态。
    pub(crate) fn open_execution(
        &self,
        session_id: &str,
        generation: &str,
        request_id: &str,
        managed_input: bool,
    ) -> Option<Vec<ClaimedInteraction>> {
        let mut state = self.state.lock().unwrap();
        if request_id.is_empty() || state.deleted_session_ids.contains(session_id) {
            return None;
        }
        let (local_generation, next_epoch) = match &state.route {
            SessionRoute::Stable {
                session_id: current,
                generation,
                prompt_epoch,
                ..
            } if current == session_id => (*generation, prompt_epoch.checked_add(1)?),
            _ => return None,
        };
        let runs = state.execution_runs.get(session_id)?;
        if runs.generation != generation
            || runs.local_generation != local_generation
            || runs.retired.contains(request_id)
        {
            return None;
        }
        if state.active_prompt.as_ref().is_some_and(|marker| {
            marker.session_id == session_id && marker.request_id.as_deref() == Some(request_id)
        }) {
            let runs = state.execution_runs.get_mut(session_id)?;
            runs.managed_input = managed_input;
            runs.pending_terminal_request = Some(request_id.to_owned());
            if let Some(previous) = runs.latest_request.replace(request_id.to_owned())
                && previous != request_id
            {
                runs.retired.insert(previous);
            }
            return None;
        }
        let old_marker = state.active_prompt.take();
        let claims = drain_pending(&mut state, ClaimCause::TurnTerminal, |owner| {
            old_marker.as_ref().is_some_and(|marker| {
                owner.session_id == marker.session_id
                    && owner.generation == marker.generation
                    && owner.prompt_epoch == marker.prompt_epoch
            })
        });
        let runs = state.execution_runs.get_mut(session_id)?;
        runs.managed_input = managed_input;
        runs.pending_terminal_request = Some(request_id.to_owned());
        if let Some(previous) = runs.latest_request.replace(request_id.to_owned())
            && previous != request_id
        {
            runs.retired.insert(previous);
        }
        state.active_prompt = Some(PromptMarker {
            client_instance_id: state.client_instance_id,
            session_id: session_id.to_owned(),
            generation: local_generation,
            prompt_epoch: next_epoch,
            request_id: Some(request_id.to_owned()),
        });
        if let SessionRoute::Stable {
            accepting_reverse,
            prompt_epoch,
            ..
        } = &mut state.route
        {
            *accepting_reverse = true;
            *prompt_epoch = next_epoch;
        }
        Some(claims)
    }

    pub(crate) fn active_execution(&self, managed_only: bool) -> Option<(String, String, String)> {
        let state = self.state.lock().unwrap();
        let marker = state.active_prompt.as_ref()?;
        let runs = state.execution_runs.get(&marker.session_id)?;
        let request_id = marker.request_id.as_ref()?;
        ((!managed_only || runs.managed_input)
            && runs.local_generation == marker.generation
            && runs.latest_request.as_ref() == Some(request_id)
            && !runs.retired.contains(request_id))
        .then(|| {
            (
                marker.session_id.clone(),
                runs.generation.clone(),
                request_id.clone(),
            )
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
        retire_execution(&mut state, &marker.session_id, marker.request_id.as_deref());
        if let SessionRoute::Stable {
            session_id,
            generation,
            accepting_reverse,
            prompt_epoch,
            ..
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
            let mut state = self.state.lock().unwrap();
            if let Some(runs) = state.execution_runs.get_mut(session_id)
                && runs.pending_terminal_request.as_deref() == Some(request_id)
            {
                runs.pending_terminal_request = None;
            }
            retire_execution(&mut state, session_id, Some(request_id));
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

    /// 快照可能先恢复下一轮，旧 done 此时不得把新的 loading 清为空闲。
    pub(crate) fn should_forward_prompt_terminal(
        &self,
        session_id: &str,
        request_id: Option<&str>,
    ) -> bool {
        let state = self.state.lock().unwrap();
        if let Some(marker) = &state.active_prompt
            && marker.session_id == session_id
            && marker.request_id.is_some()
        {
            return marker.request_id.as_deref() == request_id;
        }
        let Some(runs) = state.execution_runs.get(session_id) else {
            return true;
        };
        request_id.is_some() && runs.pending_terminal_request.as_deref() == request_id
            || !request_id.is_some_and(|request_id| runs.retired.contains(request_id))
    }

    pub fn cancel_active_prompt(&self) -> Vec<ClaimedInteraction> {
        let marker = self.state.lock().unwrap().active_prompt.clone();
        let claims = marker
            .map(|marker| self.close_prompt_exact(&marker, ClaimCause::TurnTerminal))
            .unwrap_or_default();
        let mut state = self.state.lock().unwrap();
        if let SessionRoute::Stable { prompt_epoch, .. } = &mut state.route {
            *prompt_epoch = prompt_epoch.checked_add(1).expect("prompt epoch exhausted");
        }
        claims
    }
}

fn retire_execution(
    state: &mut InteractionLifecycleState,
    session_id: &str,
    request_id: Option<&str>,
) {
    if let Some(runs) = state.execution_runs.get_mut(session_id)
        && let Some(request_id) = request_id
        && runs.latest_request.as_deref() == Some(request_id)
    {
        runs.retired.insert(request_id.to_owned());
    }
}
