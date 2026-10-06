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
        self.works.values().any(|work| {
            !matches!(work.stage, WorkStage::Settled | WorkStage::Abandoned)
                && self.work_lifecycle(&work.work_id).is_none()
        })
    }

    pub fn has_pending_terminal_obligations_for(&self, lifecycle: u64) -> bool {
        self.terminal_obligations.keys().any(|admission_id| {
            !self.terminal_acknowledgements.contains_key(admission_id)
                && self.admissions.get(admission_id).is_none_or(|record| {
                    record.admission.lifecycle == lifecycle
                        && !self.admission_processing_superseded(&record.admission)
                })
        })
    }

    fn admission_processing_superseded(&self, admission: &WorkAdmission) -> bool {
        let mut abandoned = false;
        for batch in self.batches.values().filter(|batch| {
            batch.recipient_lifecycle == admission.lifecycle
                && batch.execution == admission.execution
        }) {
            let mut found_work = false;
            for work in self
                .works
                .values()
                .filter(|work| work.batch_id == batch.batch_id)
            {
                found_work = true;
                match work.stage {
                    WorkStage::Abandoned => abandoned = true,
                    WorkStage::Settled => {}
                    _ => return false,
                }
            }
            if !found_work {
                return false;
            }
        }
        abandoned
    }

    pub fn has_pending_work_for(&self, lifecycle: u64) -> bool {
        !self.legacy_unknown.is_empty()
            || self.has_unknown_live_work_lifecycle()
            || self.has_pending_terminal_obligations_for(lifecycle)
            || self.obligations.iter().any(|(delivery_id, record)| {
                matches!(
                    record.status,
                    ObligationStatus::Pending
                        | ObligationStatus::InProgress
                        | ObligationStatus::Blocked
                ) && self
                    .deliveries
                    .get(delivery_id)
                    .is_none_or(|delivery| delivery.recipient_lifecycle == lifecycle)
            })
            || self.works.values().any(|work| {
                !matches!(work.stage, WorkStage::Settled | WorkStage::Abandoned)
                    && self.work_lifecycle(&work.work_id) == Some(lifecycle)
            })
    }
    pub fn claimable_deliveries(&self, lifecycle: u64) -> Vec<String> {
        let mut pending: Vec<_> = self
            .deliveries
            .iter()
            .filter(|(delivery_id, record)| {
                record.recipient_lifecycle == lifecycle
                    && record.batch_id.is_none()
                    && record.disposition.is_none()
                    && self.obligations.get(*delivery_id).is_none_or(|obligation| {
                        matches!(
                            obligation.status,
                            ObligationStatus::Pending | ObligationStatus::Blocked
                        )
                    })
            })
            .collect();
        pending.sort_by_key(|(_, record)| record.admission_sequence);
        let limit = self.limits.max_batch_size as usize;
        let mut selected: Vec<_> = pending.iter().take(limit).copied().collect();
        if limit > 0
            && !selected.iter().any(|(_, record)| {
                record.publication.policy.requirement == MessageRequirement::Required
            })
        {
            if let Some(required) = pending.iter().find(|(_, record)| {
                record.publication.policy.requirement == MessageRequirement::Required
            }) {
                if selected.len() == limit {
                    selected.pop();
                }
                selected.push(*required);
                selected.sort_by_key(|(_, record)| record.admission_sequence);
            }
        }
        selected
            .into_iter()
            .map(|(delivery_id, _)| delivery_id.clone())
            .collect()
    }
}
