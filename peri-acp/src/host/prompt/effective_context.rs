//! Hydrate and guard the effective transcript shared by user prompts and autonomous turns.

use crate::host::{history_window, ownership, ContextOwnership, SessionState, SharedSessions};
use crate::transport::types::AcpError;

/// Return the generation owning the prompt context. A deferred window and a
/// recoverable failed handoff both read the authoritative active transcript here.
/// An explicit stop during that read returns None before the executor receives ownership.
pub(in super::super) async fn ensure_resident_context(
    sessions: &SharedSessions,
    session_id: &str,
    controller: &peri_controller::Controller,
    resources: &dyn peri_acp_types::session_resources::SessionResources,
) -> Result<Option<u64>, AcpError> {
    let (thread_id, generation, structure, ownership, cancel) = {
        let mut states = sessions.lock().await;
        let state = states
            .get_mut(session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        ownership::mark_stale_if_dead(state);
        if state.context_ownership == ContextOwnership::HandedOff {
            return Err(AcpError::new(
                -32010,
                "Session context is owned by an in-flight prompt",
            ));
        }
        (
            state.thread_id.clone(),
            state.session_generation,
            state.structure_version,
            state.context_ownership,
            state.cancel_token.clone(),
        )
    };
    let settle = async {
        // Resident also checks the persistence barrier. Another writer may have failed
        // since the host last adopted a transcript; a read alone cannot settle that write.
        if let Err(error) = resources.drain_persistence(&thread_id).await {
            tracing::warn!(session_id, %error, "持久化尚未结清，拒绝开始新一轮执行");
            if let Some(state) = sessions.lock().await.get_mut(session_id) {
                if state.session_generation == generation
                    && state.structure_version == structure
                    && state.context_ownership == ownership
                    && state.cancel_token == cancel
                {
                    discard_uncertain_context(state);
                }
            }
            let detail = format!(
                "Session persistence is not settled; explicit persistence recovery is required before retrying: {error}"
            );
            return Err(AcpError::new(-32010, detail));
        }
        Ok(())
    };
    if ownership == ContextOwnership::Resident {
        settle.await?;
        return Ok(Some(generation));
    }
    let restore = async {
        settle.await?;
        #[cfg(test)]
        crate::host::prompt_dispatch::admission_gate::wait(&format!("{session_id}:context-read"))
            .await;
        crate::dispatch::load_active_session_payloads(controller, &thread_id).await
    };
    let payloads = match &cancel {
        Some(token) => token.run_until_cancelled(restore).await.transpose()?,
        None => Some(restore.await?),
    };
    let mut states = sessions.lock().await;
    let state = states
        .get_mut(session_id)
        .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
    if state.closing || state.close_in_flight > 0 {
        return Err(AcpError::new(
            -32010,
            "Session is closing before prompt execution",
        ));
    }
    // A close/reopen, rewind or superseding turn may have replaced this read.
    if state.session_generation != generation
        || state.structure_version != structure
        || cancel
            .as_ref()
            .is_some_and(|token| state.cancel_token.as_ref() != Some(token))
    {
        return Err(AcpError::new(
            -32010,
            "Session changed while loading context",
        ));
    }
    if cancel.as_ref().is_some_and(|token| token.is_cancelled()) {
        return Ok(None);
    }
    let Some(payloads) = payloads else {
        return Ok(None);
    };
    if state.context_ownership == ContextOwnership::Resident {
        return Ok(Some(generation));
    }
    if state.context_ownership != ownership {
        return Err(AcpError::new(
            -32010,
            "Session context owner changed while loading",
        ));
    }
    state.history_payloads = payloads;
    state.context_ownership = ContextOwnership::Resident;
    let restored_memory = peri_agent::session::memory_probe::observations_enabled()
        .then(|| state.history_memory_snapshot());
    drop(states);
    if let Some(snapshot) = restored_memory {
        peri_agent::session::memory_probe::log_payload_set("host_restore", session_id, &snapshot);
    }
    Ok(Some(generation))
}

/// 执行发起失败后把热态标记为不可继续；正文已被移交，不能凭旧副本伪造成结果。
pub(super) async fn mark_reload_required(sessions: &SharedSessions, session_id: &str) {
    if let Some(state) = sessions.lock().await.get_mut(session_id) {
        discard_uncertain_context(state);
    }
}

fn discard_uncertain_context(state: &mut SessionState) {
    state.history_payloads.clear();
    state.context_ownership = ContextOwnership::ReloadRequired;
    state.cancel_token = None;
    history_window::invalidate(state);
}
