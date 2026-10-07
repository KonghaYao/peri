use super::*;
use peri_acp_types::session::MessagePolicy;
use peri_acp_types::session_resources::{work::*, SessionResources};
use peri_acp_types::store::PersistedPayload;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[path = "staging.rs"]
mod staging;

pub(super) struct DurableMailbox {
    pub(super) store: Arc<dyn SessionResources>,
    pub(super) lifecycle: u64,
    operations: tokio::sync::Mutex<HashMap<String, FrozenOperation>>,
    publication_block: Mutex<Option<staging::PublicationBlock>>,
}

struct FrozenOperation {
    fingerprint: u64,
    commands: Vec<WorkCommand>,
    receipt: Option<UserInputQueueReceipt>,
    attempted: bool,
    uncertain: bool,
    rejection: Option<UserInputQueueError>,
}

#[derive(Serialize, Deserialize)]
struct PublicationIdentity {
    input: UserInput,
    publication_generation: String,
    fingerprint: u64,
    command_id: String,
}

#[derive(Serialize, Deserialize)]
struct WithdrawalIdentity {
    command_id: String,
    fingerprint: u64,
    expected_revision: u64,
    expected_control_generation: Option<u64>,
}

impl UserInputMailbox {
    pub fn new_durable(
        session_id: String,
        inbox: Arc<SessionInbox>,
        emit: Arc<dyn Fn(StateEvent) + Send + Sync>,
        store: Arc<dyn SessionResources>,
        lifecycle: u64,
    ) -> Arc<Self> {
        let mut mailbox = Self::new_unbound(session_id, inbox, emit);
        let inner = Arc::get_mut(&mut mailbox).expect("new mailbox has a single owner");
        inner.generation = format!("{}:{lifecycle}", inner.session_id);
        inner.durable = Some(DurableMailbox {
            store,
            lifecycle,
            operations: tokio::sync::Mutex::new(HashMap::new()),
            publication_block: Mutex::new(None),
        });
        mailbox
    }

    pub async fn refresh_durable(&self) -> Result<UserInputQueueSnapshot, UserInputQueueError> {
        let durable = self.durable.as_ref().ok_or(UserInputQueueError::Closed)?;
        let snapshot = durable.load(&self.session_id).await?;
        self.project_publications(&snapshot);
        let snapshot = self.snapshot();
        self.publish(snapshot.clone());
        Ok(snapshot)
    }

    pub async fn take_back_durable(
        &self,
        request: &TakeBackUserInputRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        self.take_back_with_control(request, None).await
    }

    async fn take_back_with_control(
        &self,
        request: &TakeBackUserInputRequest,
        control_generation: Option<u64>,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let durable = self.durable.as_ref().ok_or(UserInputQueueError::Closed)?;
        let fingerprint = compute_fingerprint(("takeback", request, control_generation));
        let mut operations = durable.operations.lock().await;
        self.validate_request(
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        validate_input_id(&request.input_id)?;
        if !operations.contains_key(&request.command_id) {
            ensure_not_frozen(&operations, std::slice::from_ref(&request.input_id))?;
            let snapshot = durable.load(&self.session_id).await?;
            self.project_publications(&snapshot);
            let recovered = recover_pending(
                &snapshot,
                &request.command_id,
                fingerprint,
                std::slice::from_ref(&request.input_id),
            )?;
            let was_pending = !recovered.is_empty();
            let replay_record = snapshot.state.deliveries.values().find(|record| {
                record
                    .disposition
                    .as_deref()
                    .and_then(|disposition| disposition.strip_prefix("withdrawn: "))
                    .and_then(|json| serde_json::from_str::<WithdrawalIdentity>(json).ok())
                    .is_some_and(|identity| identity.command_id == request.command_id)
            });
            let prior = replay_record.or_else(|| find_publication(&snapshot, &request.input_id));
            let previous = prior
                .and_then(|record| record.disposition.as_deref())
                .and_then(|disposition| disposition.strip_prefix("withdrawn: "))
                .and_then(|json| serde_json::from_str::<WithdrawalIdentity>(json).ok());
            if previous.as_ref().is_some_and(|identity| {
                identity.command_id == request.command_id && identity.fingerprint != fingerprint
            }) {
                return Err(UserInputQueueError::IdentityConflict);
            }
            let replay = previous.filter(|identity| identity.command_id == request.command_id);
            let authorization = replay.unwrap_or(WithdrawalIdentity {
                command_id: request.command_id.clone(),
                fingerprint,
                expected_revision: snapshot.state.revision,
                expected_control_generation: control_generation,
            });
            let commands = if was_pending {
                recovered
            } else if snapshot
                .state
                .staged_user_inputs
                .get(&request.input_id)
                .is_some_and(|record| {
                    record.recipient_lifecycle == durable.lifecycle
                        && (record.status == StagedUserInputStatus::Queued
                            || record
                                .withdrawal
                                .as_ref()
                                .is_some_and(|(command_id, _)| command_id == &request.command_id))
                })
            {
                vec![WorkCommand {
                    session_id: self.session_id.clone(),
                    recipient_lifecycle: durable.lifecycle,
                    mutation_id: mutation_id(
                        &format!(
                            "{}:{}:{}",
                            self.session_id, durable.lifecycle, request.command_id
                        ),
                        &request.input_id,
                        "withdraw-staged",
                    ),
                    action: WorkAction::WithdrawStagedUserInput {
                        input_id: request.input_id.clone(),
                        command_id: request.command_id.clone(),
                        fingerprint,
                    },
                }]
            } else {
                prior
                    .filter(|record| {
                        record.disposition.is_none()
                            || authorization.expected_revision != snapshot.state.revision
                    })
                    .map(|record| WorkCommand {
                        session_id: self.session_id.clone(),
                        recipient_lifecycle: durable.lifecycle,
                        mutation_id: mutation_id(
                            &format!(
                                "{}:{}:{}",
                                self.session_id, durable.lifecycle, request.command_id
                            ),
                            &request.input_id,
                            "withdraw",
                        ),
                        action: WorkAction::WithdrawDelivery {
                            expected_revision: authorization.expected_revision,
                            expected_control_generation: authorization.expected_control_generation,
                            delivery_id: record.publication.delivery_id.clone(),
                            authorization_ref: serde_json::to_string(&authorization)
                                .expect("withdrawal identity serializes"),
                        },
                    })
                    .into_iter()
                    .collect()
            };
            operations.insert(
                request.command_id.clone(),
                FrozenOperation {
                    fingerprint,
                    commands,
                    receipt: None,
                    attempted: was_pending,
                    uncertain: was_pending,
                    rejection: None,
                },
            );
        }
        self.finish_operation(
            durable,
            &mut operations,
            &request.command_id,
            fingerprint,
            std::slice::from_ref(&request.input_id),
            true,
        )
        .await
    }

    pub async fn reclaim_unclaimed_durable(
        &self,
        command_id: &str,
        control_generation: u64,
    ) -> Result<UserInputQueueSnapshot, UserInputQueueError> {
        self.stop();
        let snapshot = self.refresh_durable().await?;
        for item in snapshot.items {
            if item.state != UserInputState::Dispatching {
                continue;
            }
            let request = TakeBackUserInputRequest {
                session_id: self.session_id.clone(),
                generation: self.generation.clone(),
                command_id: mutation_id(command_id, &item.input_id, "stop"),
                input_id: item.input_id,
            };
            let receipt = self
                .take_back_with_control(&request, Some(control_generation))
                .await?;
            if let Some(input) = receipt.taken_back {
                let mut state = self.state.lock();
                if let Some(record) = state
                    .records
                    .iter_mut()
                    .find(|record| record.input.input_id == input.input_id)
                {
                    record.state = UserInputState::Queued;
                    record.handed_off = false;
                    state.revision += 1;
                }
            }
        }
        Ok(self.snapshot())
    }

    fn validate_request(
        &self,
        session: &str,
        generation: &str,
        command: &str,
    ) -> Result<(), UserInputQueueError> {
        self.validate(&self.state.lock(), session, generation, command)?;
        if command.len() > 256 {
            return Err(UserInputQueueError::InvalidIdentity);
        }
        Ok(())
    }

    async fn finish_operation(
        &self,
        durable: &DurableMailbox,
        operations: &mut HashMap<String, FrozenOperation>,
        command_id: &str,
        fingerprint: u64,
        input_ids: &[String],
        withdrawing: bool,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let operation = operations
            .get_mut(command_id)
            .ok_or(UserInputQueueError::IdentityConflict)?;
        if operation.fingerprint != fingerprint {
            return Err(UserInputQueueError::IdentityConflict);
        }
        if let Some(error) = &operation.rejection {
            return Err(error.clone());
        }
        if let Some(receipt) = &operation.receipt {
            let mut receipt = receipt.clone();
            let snapshot = durable.load(&self.session_id).await?;
            self.project_publications(&snapshot);
            project_withdrawn_results(&mut receipt, &operation.commands, &snapshot);
            receipt.snapshot = self.snapshot();
            return Ok(receipt);
        }
        let reconcile = operation.attempted;
        operation.attempted = true;
        operation.uncertain = true;
        let mut receipts = Vec::new();
        let mut generations = std::collections::BTreeMap::new();
        for command in &operation.commands {
            let receipt = match durable.commit(command, reconcile).await {
                Ok(receipt) => receipt,
                Err(error) => {
                    if matches!(error, UserInputQueueError::DurableRejected(_)) {
                        operation.uncertain = false;
                        operation.rejection = Some(error.clone());
                    }
                    return Err(error);
                }
            };
            let deliveries = match &command.action {
                WorkAction::PublishDelivery { delivery } => std::slice::from_ref(delivery),
                WorkAction::PublishStagedUserInputs { deliveries, .. } => deliveries.as_slice(),
                _ => &[],
            };
            for delivery in deliveries {
                let identity: PublicationIdentity = serde_json::from_str(
                    delivery.event.causation_id.as_deref().unwrap_or_default(),
                )
                .map_err(|_| UserInputQueueError::IdentityConflict)?;
                generations.insert(identity.input.input_id, identity.publication_generation);
            }
            receipts.push(receipt);
        }
        let snapshot = durable.load(&self.session_id).await?;
        self.project_publications(&snapshot);
        let mut state = self.state.lock();
        let mut taken_back = None;
        if withdrawing {
            for command in &operation.commands {
                if let WorkAction::WithdrawStagedUserInput { input_id, .. } = &command.action {
                    if let Some(record) = snapshot.state.staged_user_inputs.get(input_id) {
                        if record.status == StagedUserInputStatus::Withdrawn {
                            taken_back = serde_json::from_str(&record.input_json).ok();
                        }
                    }
                }
                if let WorkAction::WithdrawDelivery { delivery_id, .. } = &command.action {
                    if let Some(delivery) = snapshot.state.deliveries.get(delivery_id) {
                        if delivery.disposition.is_some() {
                            taken_back = identity(delivery).map(|identity| identity.input);
                        }
                    }
                }
            }
            for record in &mut state.records {
                if input_ids.contains(&record.input.input_id)
                    && matches!(
                        record.state,
                        UserInputState::Queued | UserInputState::Withdrawn
                    )
                {
                    record.state = UserInputState::Withdrawn;
                    record.handed_off = false;
                    taken_back = Some(record.input.clone());
                }
            }
            state.revision += 1;
        }
        let mut receipt = UserInputQueueReceipt {
            snapshot: self.snapshot_locked(&state),
            results: results_for(&state, input_ids),
            taken_back,
            work_receipts: receipts,
            publication_generations: generations,
        };
        project_withdrawn_results(&mut receipt, &operation.commands, &snapshot);
        operation.receipt = Some(receipt.clone());
        operation.uncertain = false;
        drop(state);
        self.publish(receipt.snapshot.clone());
        Ok(receipt)
    }

    pub(super) fn project_publications(&self, snapshot: &WorkSnapshot) {
        let mut latest = std::collections::BTreeMap::<String, &DeliveryRecord>::new();
        for record in snapshot.state.deliveries.values().filter(|record| {
            record.publication.purpose == DeliveryPurpose::UserInput
                && self
                    .durable
                    .as_ref()
                    .is_some_and(|durable| record.recipient_lifecycle == durable.lifecycle)
        }) {
            if let Some(identity) = identity(record) {
                let prior = latest.entry(identity.input.input_id).or_insert(record);
                if record.admission_sequence > prior.admission_sequence {
                    *prior = record;
                }
            }
        }
        let mut publications: Vec<_> = latest.into_values().collect();
        publications.sort_by_key(|record| record.admission_sequence);
        let mut state = self.state.lock();
        if snapshot.state.revision < state.projected_revision {
            return;
        }
        state.projected_revision = snapshot.state.revision;
        self.project_staged_inputs(&mut state, snapshot);
        state.revision = state.revision.max(snapshot.state.revision);
        for delivery in publications {
            let Some(identity) = identity(delivery) else {
                continue;
            };
            if snapshot
                .state
                .staged_user_inputs
                .get(&identity.input.input_id)
                .is_some_and(|record| {
                    record.recipient_lifecycle == snapshot.control.lifecycle
                        && record.publication_id.as_deref()
                            != Some(&delivery.publication.delivery_id)
                })
            {
                continue;
            }
            let withdrawn = delivery
                .disposition
                .as_deref()
                .and_then(|disposition| disposition.strip_prefix("withdrawn: "))
                .and_then(|json| serde_json::from_str::<WithdrawalIdentity>(json).ok());
            let status = if delivery.disposition.is_some() {
                if withdrawn.is_some_and(|identity| identity.expected_control_generation.is_some())
                {
                    UserInputState::Queued
                } else {
                    UserInputState::Withdrawn
                }
            } else if delivery.projected {
                UserInputState::Delivered
            } else if delivery.batch_id.is_some() {
                UserInputState::Claimed
            } else {
                UserInputState::Dispatching
            };
            let index = state
                .records
                .iter()
                .position(|record| record.input.input_id == identity.input.input_id);
            let record = if let Some(index) = index {
                &mut state.records[index]
            } else {
                state.records.push(InputRecord {
                    fingerprint: compute_fingerprint(&identity.input),
                    input: identity.input.clone(),
                    state: status,
                    handed_off: false,
                    publication_id: None,
                    publication_generation: None,
                });
                state.records.last_mut().expect("record inserted")
            };
            if record.state == UserInputState::Delivered && status == UserInputState::Claimed {
                continue;
            }
            if record.state == UserInputState::Queued
                && status == UserInputState::Withdrawn
                && record.publication_id.as_deref() == Some(&delivery.publication.delivery_id)
            {
                continue;
            }
            let changed = record.state != status;
            record.publication_generation = Some(identity.publication_generation);
            record.input = identity.input;
            record.state = status;
            record.publication_id = Some(delivery.publication.delivery_id.clone());
            if status == UserInputState::Dispatching && !record.handed_off {
                let mut queued = QueuedMessage::prompt(
                    MessageSource::UserInput,
                    BaseMessage::Human {
                        id: delivery.publication.event.content.message_id,
                        content: record.input.content.clone(),
                    },
                );
                queued.delivery_id = uuid::Uuid::parse_str(&delivery.publication.delivery_id)
                    .ok()
                    .map(MessageId::from);
                queued.admission_sequence = Some(delivery.admission_sequence);
                record.handed_off = true;
                self.inbox.handle().push_batch(vec![queued]);
            }
            if matches!(status, UserInputState::Withdrawn | UserInputState::Queued)
                && record.handed_off
            {
                let id = delivery.publication.event.content.message_id;
                self.inbox.queue().withdraw_user_inputs(&[id]);
                record.handed_off = false;
            }
            if changed {
                state.revision += 1;
            }
        }
    }
}

impl DurableMailbox {
    async fn load(&self, session_id: &str) -> Result<WorkSnapshot, UserInputQueueError> {
        let snapshot = self
            .store
            .load_session_work(&WorkQuery {
                session_id: session_id.into(),
                limit: 1,
            })
            .await
            .map_err(|_| UserInputQueueError::OutcomeUnknown)?;
        if snapshot.control.lifecycle != self.lifecycle {
            return Err(UserInputQueueError::StaleSession);
        }
        Ok(snapshot)
    }

    async fn commit(
        &self,
        command: &WorkCommand,
        reconcile: bool,
    ) -> Result<WorkReceipt, UserInputQueueError> {
        if reconcile {
            match self.store.resolve_work_mutation(command).await {
                Ok(WorkResolution::Applied { receipt }) => return accepted(receipt),
                Ok(WorkResolution::NotApplied) => {
                    return Err(UserInputQueueError::DurableRejected(
                        "Original mutation was not applied".into(),
                    ))
                }
                _ => return Err(UserInputQueueError::OutcomeUnknown),
            }
        }
        match self.store.apply_work_mutation(command).await {
            Ok(receipt) => accepted(receipt),
            Err(_) => match self.store.resolve_work_mutation(command).await {
                Ok(WorkResolution::Applied { receipt }) => accepted(receipt),
                Ok(WorkResolution::NotApplied) => Err(UserInputQueueError::DurableRejected(
                    "Original mutation was not applied".into(),
                )),
                _ => Err(UserInputQueueError::OutcomeUnknown),
            },
        }
    }
}

fn accepted(receipt: WorkReceipt) -> Result<WorkReceipt, UserInputQueueError> {
    match &receipt.decision {
        WorkDecision::Accepted => Ok(receipt),
        WorkDecision::Rejected { reason } => {
            Err(UserInputQueueError::DurableRejected(format!("{reason:?}")))
        }
    }
}

fn identity(record: &DeliveryRecord) -> Option<PublicationIdentity> {
    serde_json::from_str(record.publication.event.causation_id.as_deref()?).ok()
}

fn find_publication<'snapshot>(
    snapshot: &'snapshot WorkSnapshot,
    input_id: &str,
) -> Option<&'snapshot DeliveryRecord> {
    snapshot
        .state
        .deliveries
        .values()
        .filter(|record| {
            record.publication.purpose == DeliveryPurpose::UserInput
                && record.recipient_lifecycle == snapshot.control.lifecycle
                && identity(record).is_some_and(|identity| identity.input.input_id == input_id)
        })
        .max_by_key(|record| record.admission_sequence)
}

fn mutation_id(command_id: &str, input_id: &str, action: &str) -> String {
    format!(
        "input:{:x}",
        Sha256::digest(
            serde_json::to_vec(&(command_id, input_id, action)).expect("identities serialize")
        )
    )
}

fn publication_command(
    session_id: &str,
    lifecycle: u64,
    command_id: &str,
    delivery: PublishDelivery,
) -> WorkCommand {
    WorkCommand {
        session_id: session_id.into(),
        recipient_lifecycle: lifecycle,
        mutation_id: mutation_id(
            &format!("{session_id}:{lifecycle}:{command_id}"),
            &delivery.event.content.message_id.as_uuid().to_string(),
            "publish",
        ),
        action: WorkAction::PublishDelivery { delivery },
    }
}

fn new_publication(
    session_id: &str,
    lifecycle: u64,
    input: UserInput,
    command_id: &str,
    fingerprint: u64,
) -> Result<WorkCommand, UserInputQueueError> {
    let message = PersistedPayload::Message(BaseMessage::Human {
        id: MessageId::from(
            uuid::Uuid::parse_str(&input.input_id)
                .map_err(|_| UserInputQueueError::InvalidIdentity)?,
        ),
        content: input.content.clone(),
    });
    let event_id = format!("user-input:{}:{command_id}", input.input_id);
    let identity = PublicationIdentity {
        input,
        publication_generation: command_id.into(),
        fingerprint,
        command_id: command_id.into(),
    };
    let delivery = PublishDelivery {
        delivery_id: delivery_id(session_id, lifecycle, &identity.input.input_id, command_id),
        event: WorkEvent {
            producer_namespace: format!("peri:user-input:{session_id}"),
            event_id,
            event_kind: "userInput".into(),
            causation_id: Some(
                serde_json::to_string(&identity)
                    .map_err(|_| UserInputQueueError::InvalidIdentity)?,
            ),
            content: WorkPayload::from_payload(&message)
                .map_err(|_| UserInputQueueError::InvalidIdentity)?,
        },
        purpose: DeliveryPurpose::UserInput,
        policy: MessagePolicy::ensure_processing(),
    };
    Ok(publication_command(
        session_id, lifecycle, command_id, delivery,
    ))
}

fn delivery_id(
    session_id: &str,
    lifecycle: u64,
    input_id: &str,
    publication_generation: &str,
) -> String {
    let digest = Sha256::digest(
        serde_json::to_vec(&(
            "peri.user-input.v1",
            session_id,
            lifecycle,
            input_id,
            publication_generation,
        ))
        .expect("publication identity serializes"),
    );
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn ensure_not_frozen(
    operations: &HashMap<String, FrozenOperation>,
    input_ids: &[String],
) -> Result<(), UserInputQueueError> {
    if operations.values().any(|operation| {
        operation.uncertain
            && operation
                .commands
                .iter()
                .any(|command| match &command.action {
                    WorkAction::PublishDelivery { delivery } => {
                        input_ids.contains(&delivery.event.content.message_id.as_uuid().to_string())
                    }
                    WorkAction::PublishStagedUserInputs { deliveries, .. } => {
                        deliveries.iter().any(|delivery| {
                            input_ids
                                .contains(&delivery.event.content.message_id.as_uuid().to_string())
                        })
                    }
                    WorkAction::StageUserInput { input_json, .. } => {
                        serde_json::from_str::<UserInput>(input_json)
                            .map_or(true, |input| input_ids.contains(&input.input_id))
                    }
                    WorkAction::WithdrawStagedUserInput { input_id, .. } => {
                        input_ids.contains(input_id)
                    }
                    WorkAction::WithdrawDelivery { .. } => true,
                    _ => false,
                })
    }) {
        return Err(UserInputQueueError::OutcomeUnknown);
    }
    Ok(())
}

fn project_withdrawn_results(
    receipt: &mut UserInputQueueReceipt,
    commands: &[WorkCommand],
    snapshot: &WorkSnapshot,
) {
    for command in commands {
        let deliveries = match &command.action {
            WorkAction::PublishDelivery { delivery } => std::slice::from_ref(delivery),
            WorkAction::PublishStagedUserInputs { deliveries, .. } => deliveries.as_slice(),
            WorkAction::WithdrawDelivery { delivery_id, .. } => snapshot
                .state
                .deliveries
                .get(delivery_id)
                .map(|record| std::slice::from_ref(&record.publication))
                .unwrap_or(&[]),
            _ => &[],
        };
        for delivery in deliveries {
            if snapshot
                .state
                .deliveries
                .get(&delivery.delivery_id)
                .is_some_and(|record| record.disposition.is_some())
            {
                for result in &mut receipt.results {
                    if result.input_id == delivery.event.content.message_id.as_uuid().to_string() {
                        result.state = UserInputState::Withdrawn;
                    }
                }
            }
        }
    }
}

fn recover_pending(
    snapshot: &WorkSnapshot,
    command_id: &str,
    fingerprint: u64,
    input_ids: &[String],
) -> Result<Vec<WorkCommand>, UserInputQueueError> {
    let mut recovered = Vec::new();
    for command in &snapshot.pending_commands {
        if let WorkAction::PublishStagedUserInputs { deliveries, .. } = &command.action {
            let identities = deliveries
                .iter()
                .map(|delivery| {
                    serde_json::from_str::<PublicationIdentity>(
                        delivery.event.causation_id.as_deref().unwrap_or_default(),
                    )
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| UserInputQueueError::IdentityConflict)?;
            if identities
                .iter()
                .any(|identity| identity.command_id == command_id)
            {
                if identities.len() != input_ids.len()
                    || identities.iter().any(|identity| {
                        identity.command_id != command_id
                            || identity.fingerprint != fingerprint
                            || !input_ids.contains(&identity.input.input_id)
                    })
                {
                    return Err(UserInputQueueError::IdentityConflict);
                }
                recovered.push(command.clone());
            }
            continue;
        }
        let association = match &command.action {
            WorkAction::StageUserInput {
                input_json,
                command_id,
                fingerprint,
            } => serde_json::from_str::<UserInput>(input_json)
                .ok()
                .map(|input| (command_id.clone(), *fingerprint, input.input_id)),
            WorkAction::WithdrawStagedUserInput {
                input_id,
                command_id,
                fingerprint,
            } => Some((command_id.clone(), *fingerprint, input_id.clone())),
            WorkAction::PublishDelivery { delivery } => delivery
                .event
                .causation_id
                .as_deref()
                .and_then(|json| serde_json::from_str::<PublicationIdentity>(json).ok())
                .map(|identity| {
                    (
                        identity.command_id,
                        identity.fingerprint,
                        identity.input.input_id,
                    )
                }),
            WorkAction::WithdrawDelivery {
                authorization_ref,
                delivery_id,
                ..
            } => serde_json::from_str::<WithdrawalIdentity>(authorization_ref)
                .ok()
                .and_then(|identity| {
                    snapshot.state.deliveries.get(delivery_id).map(|delivery| {
                        (
                            identity.command_id,
                            identity.fingerprint,
                            delivery
                                .publication
                                .event
                                .content
                                .message_id
                                .as_uuid()
                                .to_string(),
                        )
                    })
                }),
            _ => None,
        };
        if let Some((owner, original_fingerprint, input_id)) = association {
            if owner != command_id {
                continue;
            }
            if original_fingerprint != fingerprint || !input_ids.contains(&input_id) {
                return Err(UserInputQueueError::IdentityConflict);
            }
            recovered.push(command.clone());
        }
    }
    if !snapshot.pending_commands.is_empty() && recovered.is_empty() {
        return Err(UserInputQueueError::OutcomeUnknown);
    }
    Ok(recovered)
}

#[cfg(test)]
#[path = "durable_test.rs"]
mod tests;

#[cfg(test)]
#[path = "staging_test.rs"]
mod staging_tests;

#[cfg(test)]
#[path = "staging_resume_test.rs"]
mod staging_resume_tests;
