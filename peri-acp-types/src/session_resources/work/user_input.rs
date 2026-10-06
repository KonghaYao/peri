use super::*;
use crate::session::UserInput;
use crate::session_resources::ControlStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StagedUserInputStatus {
    Queued,
    Published,
    Withdrawn,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StagedUserInput {
    pub recipient_lifecycle: u64,
    pub sequence: u64,
    pub publication_generation: u64,
    pub input_id: String,
    pub input_json: String,
    pub command_id: String,
    pub fingerprint: u64,
    pub status: StagedUserInputStatus,
    pub publication_id: Option<String>,
    pub withdrawal: Option<(String, u64)>,
}

pub(super) fn stage(
    command: &WorkCommand,
    control: &ControlState,
    state: &mut WorkState,
    input_json: &str,
    command_id: &str,
    fingerprint: u64,
) -> Result<(), WorkRejection> {
    let input: UserInput =
        serde_json::from_str(input_json).map_err(|_| WorkRejection::InvalidTransition)?;
    if matches!(
        control.status,
        ControlStatus::Closing | ControlStatus::Closed
    ) || input.content.is_empty()
        || uuid::Uuid::parse_str(&input.input_id).is_err()
        || command_id.is_empty()
    {
        return Err(WorkRejection::InvalidTransition);
    }
    if let Some(prior) = state.staged_user_inputs.get(&input.input_id) {
        if prior.recipient_lifecycle == command.recipient_lifecycle {
            if prior.command_id == command_id {
                return if prior.input_json == input_json && prior.fingerprint == fingerprint {
                    Ok(())
                } else {
                    Err(WorkRejection::Conflict)
                };
            }
            if prior.status != StagedUserInputStatus::Withdrawn {
                return Err(WorkRejection::Conflict);
            }
        } else {
            return Err(WorkRejection::StaleLifecycle);
        }
    }
    let bytes = state
        .staged_user_inputs
        .values()
        .filter(|record| {
            record.recipient_lifecycle == command.recipient_lifecycle
                && (record.status == StagedUserInputStatus::Queued
                    || record.publication_id.as_ref().is_some_and(|id| {
                        state.deliveries.get(id).is_some_and(|delivery| {
                            !delivery.projected && delivery.disposition.is_none()
                        })
                    }))
        })
        .try_fold(input_json.len() as u64, |total, record| {
            total.checked_add(record.input_json.len() as u64)
        })
        .ok_or(WorkRejection::VersionExhausted)?;
    if bytes > state.limits.required_bytes {
        return Err(WorkRejection::Capacity);
    }
    if state
        .staged_user_inputs
        .values()
        .filter(|record| {
            record.recipient_lifecycle == command.recipient_lifecycle
                && (record.status == StagedUserInputStatus::Queued
                    || record.publication_id.as_ref().is_some_and(|id| {
                        state.deliveries.get(id).is_some_and(|delivery| {
                            delivery.disposition.is_none() && !delivery.projected
                        })
                    }))
        })
        .count()
        >= 32
    {
        return Err(WorkRejection::Capacity);
    }
    state.staged_user_inputs.insert(
        input.input_id.clone(),
        StagedUserInput {
            recipient_lifecycle: command.recipient_lifecycle,
            sequence: state
                .revision
                .checked_add(1)
                .ok_or(WorkRejection::VersionExhausted)?,
            publication_generation: 0,
            input_id: input.input_id.clone(),
            input_json: input_json.into(),
            command_id: command_id.into(),
            fingerprint,
            status: StagedUserInputStatus::Queued,
            publication_id: None,
            withdrawal: None,
        },
    );
    Ok(())
}

pub(super) fn withdraw(
    command: &WorkCommand,
    state: &mut WorkState,
    input_id: &str,
    command_id: &str,
    fingerprint: u64,
) -> Result<(), WorkRejection> {
    let record = state
        .staged_user_inputs
        .get_mut(input_id)
        .ok_or(WorkRejection::Conflict)?;
    if record.recipient_lifecycle != command.recipient_lifecycle {
        return Err(WorkRejection::StaleLifecycle);
    }
    if record.withdrawal.as_ref() == Some(&(command_id.into(), fingerprint)) {
        return Ok(());
    }
    if record.status != StagedUserInputStatus::Queued {
        return Err(WorkRejection::InvalidTransition);
    }
    record.status = StagedUserInputStatus::Withdrawn;
    record.withdrawal = Some((command_id.into(), fingerprint));
    Ok(())
}

pub(super) fn publish(
    command: &WorkCommand,
    control: &ControlState,
    state: &mut WorkState,
    deliveries: &[PublishDelivery],
    interrupt_current: bool,
    receipt: &mut WorkReceipt,
    events: &mut Vec<WorkEvent>,
) -> Result<(), WorkRejection> {
    if deliveries.is_empty() {
        return Err(WorkRejection::InvalidTransition);
    }
    let mut selected = std::collections::BTreeSet::new();
    for delivery in deliveries {
        let input_id = delivery.event.content.message_id.as_uuid().to_string();
        let record = state
            .staged_user_inputs
            .get(&input_id)
            .ok_or(WorkRejection::Conflict)?;
        let input: UserInput = serde_json::from_str(&record.input_json)
            .map_err(|_| WorkRejection::InvalidTransition)?;
        if !selected.insert(input_id.clone())
            || record.recipient_lifecycle != command.recipient_lifecycle
            || record.status != StagedUserInputStatus::Queued
            || delivery.purpose != DeliveryPurpose::UserInput
            || delivery.policy != MessagePolicy::ensure_processing()
            || delivery.event.content
                != WorkPayload::from_payload(&PersistedPayload::Message(
                    crate::messages::BaseMessage::Human {
                        id: delivery.event.content.message_id,
                        content: input.content,
                    },
                ))
                .map_err(|_| WorkRejection::InvalidTransition)?
        {
            return Err(WorkRejection::Conflict);
        }
        super::delivery::publish(command, control, state, delivery, receipt, events)?;
        let record = state
            .staged_user_inputs
            .get_mut(&input_id)
            .ok_or(WorkRejection::Conflict)?;
        record.status = StagedUserInputStatus::Published;
        record.publication_id = Some(delivery.delivery_id.clone());
    }
    if interrupt_current {
        let targets: Vec<_> = state
            .works
            .values()
            .filter(|work| {
                !matches!(work.stage, WorkStage::Settled | WorkStage::Abandoned)
                    && state.batches.get(&work.batch_id).is_some_and(|batch| {
                        batch.recipient_lifecycle == command.recipient_lifecycle
                            && control.attempt.as_ref() == Some(&batch.execution)
                    })
            })
            .map(|work| WorkTarget {
                work_id: work.work_id.clone(),
                expected_work_revision: work.revision,
            })
            .collect();
        for target in targets {
            super::processing::abandon(state, &target, "processing superseded by explicit user input selection; external outcomes remain recorded", &command.mutation_id, receipt)?;
        }
    }
    Ok(())
}

pub(super) fn withdraw_published(
    state: &mut WorkState,
    delivery_id: &str,
    requeue: bool,
) -> Result<(), WorkRejection> {
    for record in state.staged_user_inputs.values_mut() {
        if record.publication_id.as_deref() == Some(delivery_id) {
            record.status = if requeue {
                StagedUserInputStatus::Queued
            } else {
                StagedUserInputStatus::Withdrawn
            };
            record.publication_id = None;
            if requeue {
                record.publication_generation = record
                    .publication_generation
                    .checked_add(1)
                    .ok_or(WorkRejection::VersionExhausted)?;
            }
        }
    }
    Ok(())
}
