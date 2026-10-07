use super::*;
use crate::session::MessageRequirement;
use crate::session_resources::ControlStatus;

#[cfg(test)]
#[path = "terminal_query_test.rs"]
mod terminal_query_test;

impl WorkSnapshot {
    pub fn from_state(query: &WorkQuery, control: ControlState, state: WorkState) -> Self {
        let blocked = control.status != ControlStatus::Active
            || state.has_pending_terminal_obligations_for(control.lifecycle)
            || state.has_unknown_live_work_lifecycle()
            || !state.legacy_unknown.is_empty()
            || state.works.values().any(|work| {
                work.stage == WorkStage::Blocked
                    && state.work_lifecycle(&work.work_id) == Some(control.lifecycle)
            });
        let mut candidates = Vec::new();
        let live: Vec<_> = state
            .works
            .values()
            .filter(|work| !matches!(work.stage, WorkStage::Settled | WorkStage::Abandoned))
            .filter(|work| state.work_lifecycle(&work.work_id) == Some(control.lifecycle))
            .collect();
        if !blocked && query.limit > 0 {
            if !live.is_empty() {
                for work in live.into_iter().take(query.limit as usize) {
                    let requires_recovery = work.stage == WorkStage::ReasonInFlight
                        || work.invocation_ids.iter().any(|invocation_id| {
                            state
                                .invocations
                                .get(invocation_id)
                                .is_some_and(|invocation| {
                                    matches!(
                                        invocation.status,
                                        InvocationStatus::DispatchAccepted
                                            | InvocationStatus::OutcomeUnknown
                                    )
                                })
                        });
                    candidates.push(WorkCandidate {
                        work_id: work.work_id.clone(),
                        work_revision: work.revision,
                        stage: work.stage,
                        batch_id: Some(work.batch_id.clone()),
                        delivery_ids: Vec::new(),
                        requires_recovery,
                    });
                }
            } else {
                let selected = state.claimable_deliveries(control.lifecycle);
                if let Some(work_id) = selected.iter().find(|delivery_id| {
                    state.deliveries[*delivery_id]
                        .publication
                        .policy
                        .requirement
                        == MessageRequirement::Required
                }) {
                    candidates.push(WorkCandidate {
                        work_id: work_id.clone(),
                        work_revision: 0,
                        stage: WorkStage::ReasonReady,
                        batch_id: None,
                        delivery_ids: selected.clone(),
                        requires_recovery: false,
                    });
                }
            }
        }
        Self {
            session_id: query.session_id.clone(),
            control,
            state,
            candidates,
            blocked,
            pending_commands: Vec::new(),
        }
    }

    pub fn validate_admission(&self, admission: &WorkAdmission) -> SessionResourceResult<()> {
        if self.state.works.contains_key(&admission.work_id) {
            self.validate_work_lifecycle(&admission.work_id)?;
        }
        validate_admission_association(
            &self.session_id,
            &self.control,
            &self.candidates,
            self.blocked,
            admission,
        )
    }

    pub fn validate_work_lifecycle(&self, work_id: &str) -> SessionResourceResult<()> {
        if self.state.work_lifecycle(work_id) != Some(self.control.lifecycle) {
            return Err(SessionResourceError::conflict(
                "work batch lifecycle is stale or unknown",
            ));
        }
        Ok(())
    }

    pub fn has_pending_current_work(&self) -> bool {
        self.state.has_pending_work_for(self.control.lifecycle)
    }
}

pub fn validate_admission_association(
    session_id: &str,
    control: &ControlState,
    candidates: &[WorkCandidate],
    blocked: bool,
    admission: &WorkAdmission,
) -> SessionResourceResult<()> {
    if admission.session_id != session_id
        || admission.admission_id.is_empty()
        || admission.instance_id.is_empty()
        || admission.generation_id.is_empty()
        || admission.lifecycle != control.lifecycle
        || admission.control_generation != control.control_generation
        || blocked
        || !candidates.iter().any(|work| {
            work.work_id == admission.work_id && work.work_revision == admission.work_revision
        })
    {
        return Err(SessionResourceError::conflict(
            "work admission association is stale",
        ));
    }
    Ok(())
}

impl WorkState {
    pub fn work_lifecycle(&self, work_id: &str) -> Option<u64> {
        self.works
            .get(work_id)
            .and_then(|work| self.batches.get(&work.batch_id))
            .map(|batch| batch.recipient_lifecycle)
    }

    pub fn has_unknown_live_work_lifecycle(&self) -> bool {
        WorkAvailabilityState::from(self).has_unknown_live_work_lifecycle()
    }

    pub fn has_pending_terminal_obligations_for(&self, lifecycle: u64) -> bool {
        WorkAvailabilityState::from(self).has_pending_terminal_obligations_for(lifecycle)
    }

    pub(super) fn admission_batch(&self, admission: &WorkAdmission) -> Option<&ProcessingBatch> {
        let key = super::availability::admission_batch_key(
            self.works
                .get(&admission.work_id)
                .map(|work| work.batch_id.as_str()),
            &admission.work_id,
            admission.lifecycle,
            self.batches.iter().map(|(id, batch)| {
                (
                    id.as_str(),
                    batch.recipient_lifecycle,
                    batch.processing_delivery_ids.as_slice(),
                )
            }),
        )?;
        self.batches.get(key)
    }

    pub fn has_pending_work_for(&self, lifecycle: u64) -> bool {
        WorkAvailabilityState::from(self).has_pending_work_for(lifecycle)
    }
    pub fn claimable_deliveries(&self, lifecycle: u64) -> Vec<String> {
        WorkAvailabilityState::from(self).claimable_deliveries(lifecycle)
    }
}
