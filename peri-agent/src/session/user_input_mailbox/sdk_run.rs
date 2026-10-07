use super::*;
use peri_acp_types::session_resources::work::{
    DeliveryPurpose, WorkAdmission, WorkDecision, WorkSnapshot,
};

pub(super) struct SdkRunObservation {
    pub(super) admission: WorkAdmission,
    pub(super) input_ids: Vec<MessageId>,
}

impl UserInputMailbox {
    pub fn attach_sdk_attempt(&self, admission: &WorkAdmission, cancel: CancellationToken) -> bool {
        let mut state = self.state.lock();
        if !state.valid || state.paused {
            return false;
        }
        let Some(active) = state.active.as_mut().filter(|active| {
            active
                .sdk
                .as_ref()
                .is_some_and(|sdk| sdk.admission == *admission)
        }) else {
            return false;
        };
        if active.cancel.is_some() {
            return true;
        }
        active.cancel = Some(cancel);
        active.reason = InterruptReason::None;
        self.handoff_locked(&mut state);
        state.revision += 1;
        let snapshot = self.snapshot_locked(&state);
        drop(state);
        self.publish(snapshot);
        true
    }

    pub async fn observe_sdk_run(
        &self,
        admission: &WorkAdmission,
    ) -> Result<UserInputRunTicket, UserInputQueueError> {
        let durable = self.durable.as_ref().ok_or(UserInputQueueError::Closed)?;
        if self.observed_sdk_ticket(admission).is_some() {
            self.validate_sdk_control(admission).await?;
            return self
                .observed_sdk_ticket(admission)
                .ok_or(UserInputQueueError::Closed);
        }
        let snapshot = durable.load(&self.session_id).await?;
        self.observe_sdk_snapshot(admission, &snapshot)
    }

    pub(crate) async fn observe_sdk_run_from_snapshot(
        &self,
        admission: &WorkAdmission,
        snapshot: &WorkSnapshot,
    ) -> Result<UserInputRunTicket, UserInputQueueError> {
        self.validate_sdk_control(admission).await?;
        self.observe_sdk_snapshot(admission, snapshot)
    }

    fn observed_sdk_ticket(&self, admission: &WorkAdmission) -> Option<UserInputRunTicket> {
        let durable = self.durable.as_ref()?;
        let state = self.state.lock();
        if !state.valid
            || admission.session_id != self.session_id
            || admission.lifecycle != durable.lifecycle
        {
            return None;
        }
        state
            .active
            .as_ref()
            .filter(|active| {
                active.outcome.is_none()
                    && active
                        .sdk
                        .as_ref()
                        .is_some_and(|sdk| sdk.admission == *admission)
            })
            .map(|active| active.ticket.clone())
    }

    async fn validate_sdk_control(
        &self,
        admission: &WorkAdmission,
    ) -> Result<(), UserInputQueueError> {
        let durable = self.durable.as_ref().ok_or(UserInputQueueError::Closed)?;
        let started = std::time::Instant::now();
        #[cfg(test)]
        durable
            .control_loads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let control = durable
            .store
            .load_session_control(&self.session_id)
            .await
            .map_err(|_| UserInputQueueError::OutcomeUnknown)?;
        tracing::debug!(session_id = %self.session_id, admission_id = %admission.admission_id,
            duration_us = started.elapsed().as_micros() as u64, "mailbox SDK control validated");
        if admission.session_id != self.session_id
            || admission.lifecycle != durable.lifecycle
            || admission.lifecycle != control.lifecycle
            || admission.control_generation != control.control_generation
            || control.attempt.as_ref() != Some(&admission.execution)
        {
            return Err(UserInputQueueError::DurableRejected(
                "SDK observation no longer matches exact execution control".into(),
            ));
        }
        Ok(())
    }

    fn observe_sdk_snapshot(
        &self,
        admission: &WorkAdmission,
        snapshot: &WorkSnapshot,
    ) -> Result<UserInputRunTicket, UserInputQueueError> {
        let durable = self.durable.as_ref().ok_or(UserInputQueueError::Closed)?;
        let registered = snapshot
            .state
            .admissions
            .get(&admission.admission_id)
            .is_some_and(|record| {
                record.admission == *admission
                    && record.settled_receipt.is_none()
                    && record
                        .entering_receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt.decision == WorkDecision::Accepted)
            });
        if snapshot.session_id != self.session_id
            || admission.session_id != self.session_id
            || admission.lifecycle != durable.lifecycle
            || admission.lifecycle != snapshot.control.lifecycle
            || admission.control_generation != snapshot.control.control_generation
            || snapshot.control.attempt.as_ref() != Some(&admission.execution)
            || !registered
        {
            return Err(UserInputQueueError::DurableRejected(
                "SDK admission is not durably registered for this exact execution".into(),
            ));
        }
        self.project_publications(snapshot);
        let batch_id = snapshot
            .state
            .works
            .get(&admission.work_id)
            .map(|work| work.batch_id.as_str())
            .unwrap_or(&admission.work_id);
        let input_ids = snapshot
            .state
            .batches
            .get(batch_id)
            .map(|batch| {
                batch
                    .processing_delivery_ids
                    .iter()
                    .filter_map(|delivery_id| {
                        snapshot
                            .state
                            .deliveries
                            .get(delivery_id)
                            .filter(|delivery| {
                                delivery.publication.purpose == DeliveryPurpose::UserInput
                            })
                            .map(|delivery| delivery.publication.event.content.message_id)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ticket = UserInputRunTicket {
            id: admission.admission_id.clone(),
        };
        let mut state = self.state.lock();
        if !state.valid {
            return Err(UserInputQueueError::Closed);
        }
        if let Some(active) = &mut state.active {
            if active.ticket == ticket {
                if active
                    .sdk
                    .as_ref()
                    .is_some_and(|sdk| sdk.admission == *admission)
                {
                    return Ok(ticket);
                }
                return Err(UserInputQueueError::IdentityConflict);
            }
            if active
                .sdk
                .as_ref()
                .is_some_and(|sdk| sdk.admission.execution == admission.execution)
            {
                return Err(UserInputQueueError::IdentityConflict);
            }
        }
        state.active = Some(ActiveRun {
            ticket: ticket.clone(),
            cancel: None,
            reason: InterruptReason::None,
            outcome: None,
            managed: true,
            sdk: Some(SdkRunObservation {
                admission: admission.clone(),
                input_ids,
            }),
        });
        state.paused = false;
        state.revision += 1;
        let snapshot = self.snapshot_locked(&state);
        drop(state);
        self.publish(snapshot);
        Ok(ticket)
    }

    pub fn sdk_run_input_ids(&self, ticket: &UserInputRunTicket) -> Vec<MessageId> {
        self.state
            .lock()
            .active
            .as_ref()
            .filter(|active| active.ticket == *ticket)
            .and_then(|active| active.sdk.as_ref())
            .map(|sdk| sdk.input_ids.clone())
            .unwrap_or_default()
    }

    pub fn finish_sdk_run(
        &self,
        admission: &WorkAdmission,
        outcome: UserInputAttemptOutcome,
    ) -> bool {
        let ticket = {
            let state = self.state.lock();
            let Some(active) = state.active.as_ref().filter(|active| {
                active
                    .sdk
                    .as_ref()
                    .is_some_and(|sdk| sdk.admission == *admission)
            }) else {
                return false;
            };
            active.ticket.clone()
        };
        self.finish_attempt(&ticket, outcome);
        true
    }

    pub fn sdk_run_publication_generations(
        &self,
        ticket: &UserInputRunTicket,
    ) -> std::collections::BTreeMap<String, String> {
        let state = self.state.lock();
        let Some(sdk) = state
            .active
            .as_ref()
            .filter(|active| active.ticket == *ticket)
            .and_then(|active| active.sdk.as_ref())
        else {
            return Default::default();
        };
        state
            .records
            .iter()
            .filter(|record| matches_id(record, &sdk.input_ids))
            .filter_map(|record| {
                record
                    .publication_generation
                    .as_ref()
                    .map(|generation| (record.input.input_id.clone(), generation.clone()))
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "sdk_run_test.rs"]
mod tests;
