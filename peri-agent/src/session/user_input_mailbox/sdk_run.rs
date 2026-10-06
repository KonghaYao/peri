use super::*;
use peri_acp_types::session_resources::work::{
    DeliveryPurpose, WorkAdmission, WorkPage, WorkQuery, WorkSelector,
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
        let snapshot = durable
            .inspect(
                &self.session_id,
                WorkSelector::Admission {
                    admission_id: admission.admission_id.clone(),
                },
            )
            .await?;
        let registered = matches!(&snapshot.page, WorkPage::Admissions(records) if records.iter().any(|record| {
            record.admission == *admission && record.leaving_evidence_id.is_none()
                && !record.entering_mutation_id.is_empty()
        }));
        if admission.session_id != self.session_id
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
        self.refresh_durable().await?;
        let inspection = durable
            .store
            .inspect_work(&WorkQuery::new(
                &self.session_id,
                WorkSelector::ProcessingDeliveries {
                    processing_id: admission.work_id.clone(),
                },
            ))
            .await
            .map_err(|error| UserInputQueueError::DurableRejected(error.to_string()))?;
        let WorkPage::Deliveries(deliveries) = inspection.page else {
            return Err(UserInputQueueError::IdentityConflict);
        };
        if inspection.next_cursor.is_some() {
            return Err(UserInputQueueError::Capacity);
        }
        let input_ids = deliveries
            .iter()
            .filter(|delivery| {
                delivery.participates_in_reason
                    && delivery.publication.purpose == DeliveryPurpose::UserInput
            })
            .map(|delivery| delivery.publication.event.content.message_id)
            .collect();
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
