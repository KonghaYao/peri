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
    restore: tokio::sync::Mutex<bool>,
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
struct WithdrawalIdentity {
    command_id: String,
    fingerprint: u64,
    expected_revision: u64,
    expected_control_generation: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct StagedEnqueueInput {
    #[serde(flatten)]
    input: UserInput,
    #[serde(default)]
    enqueue_publication: Option<WorkCommand>,
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
            restore: tokio::sync::Mutex::new(false),
        });
        mailbox
    }

    pub async fn refresh_durable(&self) -> Result<UserInputQueueSnapshot, UserInputQueueError> {
        self.refresh_inputs(&[]).await
    }

    async fn refresh_inputs(
        &self,
        requested: &[String],
    ) -> Result<UserInputQueueSnapshot, UserInputQueueError> {
        let durable = self.durable.as_ref().ok_or(UserInputQueueError::Closed)?;
        let head = durable
            .inspect(&self.session_id, WorkSelector::Head)
            .await?;
        let mut restored = durable.restore.lock().await;
        let mut input_ids = self
            .state
            .lock()
            .records
            .iter()
            .filter(|record| {
                matches!(
                    record.state,
                    UserInputState::Queued | UserInputState::Dispatching | UserInputState::Claimed
                )
            })
            .map(|record| record.input.input_id.clone())
            .collect::<Vec<_>>();
        for input_id in requested {
            if !input_ids.contains(input_id) {
                input_ids.push(input_id.clone());
            }
        }
        let mut drafts = if !*restored {
            durable
                .draft_page(&self.session_id, WorkSelector::UnresolvedInputs)
                .await?
        } else {
            Vec::new()
        };
        for input_id in input_ids {
            if drafts.iter().any(|draft| draft.input_id == input_id) {
                continue;
            }
            if let Some(draft) = durable.draft(&self.session_id, &input_id).await? {
                drafts.push(draft);
            }
        }
        for draft in drafts {
            let input = durable.input(&self.session_id, &draft.content).await?;
            let delivery = match &draft.publication_id {
                Some(delivery_id) => durable.delivery(&self.session_id, delivery_id).await?,
                None => None,
            };
            let (status, publication_id, generation) = if let Some(delivery) = &delivery {
                let identity = identity(delivery)?;
                let withdrawn = delivery
                    .disposition
                    .as_deref()
                    .and_then(|value| value.strip_prefix("withdrawn: "))
                    .and_then(|value| serde_json::from_str::<WithdrawalIdentity>(value).ok());
                let status = if draft.status == StagedUserInputStatus::Queued {
                    UserInputState::Queued
                } else if draft.status == StagedUserInputStatus::Withdrawn {
                    UserInputState::Withdrawn
                } else if delivery.disposition.is_some() {
                    if withdrawn.is_some_and(|authorization| {
                        authorization.expected_control_generation.is_some()
                    }) {
                        UserInputState::Queued
                    } else {
                        UserInputState::Withdrawn
                    }
                } else if delivery.projection.is_some() {
                    UserInputState::Delivered
                } else if delivery.processing_id.is_some() {
                    UserInputState::Claimed
                } else {
                    UserInputState::Dispatching
                };
                (
                    status,
                    Some(delivery.delivery_id.clone()),
                    Some(identity.publication_generation),
                )
            } else {
                (
                    match draft.status {
                        StagedUserInputStatus::Queued => UserInputState::Queued,
                        StagedUserInputStatus::Withdrawn => UserInputState::Withdrawn,
                        StagedUserInputStatus::Published => {
                            return Err(UserInputQueueError::OutcomeUnknown)
                        }
                    },
                    None,
                    None,
                )
            };
            let mut state = self.state.lock();
            let position = state
                .records
                .iter()
                .position(|record| record.input.input_id == input.input_id);
            let record = if let Some(position) = position {
                &mut state.records[position]
            } else {
                state.records.push(InputRecord {
                    fingerprint: compute_fingerprint(&input),
                    input: input.clone(),
                    state: status,
                    handed_off: false,
                    publication_id: None,
                    publication_generation: None,
                });
                state.records.last_mut().expect("record inserted")
            };
            if record.state == UserInputState::Delivered
                && matches!(
                    status,
                    UserInputState::Claimed | UserInputState::Dispatching
                )
            {
                continue;
            }
            record.input = input;
            record.state = status;
            record.publication_id = publication_id;
            record.publication_generation = generation;
            if status == UserInputState::Dispatching && !record.handed_off {
                let delivery = delivery
                    .as_ref()
                    .ok_or(UserInputQueueError::OutcomeUnknown)?;
                let mut queued = QueuedMessage::prompt(
                    MessageSource::UserInput,
                    BaseMessage::Human {
                        id: delivery.publication.event.content.message_id,
                        content: record.input.content.clone(),
                    },
                );
                queued.delivery_id = Some(MessageId::from(
                    uuid::Uuid::parse_str(&delivery.delivery_id)
                        .map_err(|_| UserInputQueueError::InvalidIdentity)?,
                ));
                self.inbox.handle().push(queued);
                record.handed_off = true;
            }
            state.projected_revision = state.projected_revision.max(head.head.change_seq);
            state.revision = state.revision.max(head.head.change_seq);
        }
        let snapshot = self.snapshot();
        *restored = true;
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
        self.validate_request(
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        validate_input_id(&request.input_id)?;
        let fingerprint = compute_fingerprint(("takeback", request, control_generation));
        let mut operations = durable.operations.lock().await;
        if !operations.contains_key(&request.command_id) {
            ensure_not_frozen(&operations)?;
            let mutation = mutation_id(
                &format!(
                    "{}:{}:{}",
                    self.session_id, durable.lifecycle, request.command_id
                ),
                &request.input_id,
                "withdraw",
            );
            let saved = durable.command(&self.session_id, &mutation).await?;
            let reconcile = saved.is_some();
            let command = if let Some(saved) = saved {
                let valid = match &saved.command.action {
                    WorkAction::WithdrawStagedUserInput {
                        input_id,
                        command_id,
                        fingerprint: original,
                    } => {
                        input_id == &request.input_id
                            && command_id == &request.command_id
                            && *original == fingerprint
                    }
                    WorkAction::WithdrawDelivery {
                        delivery_id,
                        authorization_ref,
                        ..
                    } => {
                        let authorization: WithdrawalIdentity =
                            serde_json::from_str(authorization_ref)
                                .map_err(|_| UserInputQueueError::IdentityConflict)?;
                        let delivery = durable
                            .delivery(&self.session_id, delivery_id)
                            .await?
                            .ok_or(UserInputQueueError::IdentityConflict)?;
                        authorization.command_id == request.command_id
                            && authorization.fingerprint == fingerprint
                            && identity(&delivery)?.input_id == request.input_id
                    }
                    _ => false,
                };
                if !valid {
                    return Err(UserInputQueueError::IdentityConflict);
                }
                saved.command
            } else {
                durable.ensure_confirmed(&self.session_id).await?;
                let draft = durable
                    .draft(&self.session_id, &request.input_id)
                    .await?
                    .ok_or(UserInputQueueError::IdentityConflict)?;
                let head = durable
                    .inspect(&self.session_id, WorkSelector::Head)
                    .await?;
                let action = if draft.status == StagedUserInputStatus::Queued {
                    WorkAction::WithdrawStagedUserInput {
                        input_id: request.input_id.clone(),
                        command_id: request.command_id.clone(),
                        fingerprint,
                    }
                } else {
                    let delivery_id = draft
                        .publication_id
                        .ok_or(UserInputQueueError::IdentityConflict)?;
                    WorkAction::WithdrawDelivery {
                        expected_revision: head.head.change_seq,
                        expected_control_generation: control_generation,
                        delivery_id,
                        authorization_ref: serde_json::to_string(&WithdrawalIdentity {
                            command_id: request.command_id.clone(),
                            fingerprint,
                            expected_revision: head.head.change_seq,
                            expected_control_generation: control_generation,
                        })
                        .map_err(|_| UserInputQueueError::InvalidIdentity)?,
                    }
                };
                WorkCommand {
                    session_id: self.session_id.clone(),
                    recipient_lifecycle: durable.lifecycle,
                    mutation_id: mutation,
                    action,
                }
            };
            operations.insert(
                request.command_id.clone(),
                frozen_operation(fingerprint, vec![command], reconcile),
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
            self.refresh_inputs(input_ids).await?;
            receipt.snapshot = self.snapshot();
            receipt.results = results_for(&self.state.lock(), input_ids);
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
            if let WorkAction::PublishStagedUserInputs { deliveries, .. } = &command.action {
                for delivery in deliveries {
                    let identity: UserInputPublicationIdentity = serde_json::from_str(
                        delivery
                            .event
                            .causation_id
                            .as_deref()
                            .ok_or(UserInputQueueError::IdentityConflict)?,
                    )
                    .map_err(|_| UserInputQueueError::IdentityConflict)?;
                    generations.insert(identity.input_id, identity.publication_generation);
                }
            }
            receipts.push(receipt);
        }
        self.refresh_inputs(input_ids).await?;
        if withdrawing {
            let identities = input_ids
                .iter()
                .map(|input_id| uuid::Uuid::parse_str(input_id).map(MessageId::from))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| UserInputQueueError::InvalidIdentity)?;
            self.inbox.queue().withdraw_user_inputs(&identities);
        }
        let mut state = self.state.lock();
        let mut taken_back = None;
        if withdrawing {
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
        let receipt = UserInputQueueReceipt {
            snapshot: self.snapshot_locked(&state),
            results: results_for(&state, input_ids),
            taken_back,
            work_receipts: receipts,
            publication_generations: generations,
        };
        operation.receipt = Some(receipt.clone());
        operation.uncertain = false;
        drop(state);
        self.publish(receipt.snapshot.clone());
        Ok(receipt)
    }
}

impl DurableMailbox {
    pub(super) async fn inspect(
        &self,
        session_id: &str,
        selector: WorkSelector,
    ) -> Result<WorkInspection, UserInputQueueError> {
        let inspection = self
            .store
            .inspect_work(&WorkQuery::new(session_id, selector))
            .await
            .map_err(|error| UserInputQueueError::DurableRejected(error.to_string()))?;
        if inspection.session_id != session_id || inspection.control.lifecycle != self.lifecycle {
            return Err(UserInputQueueError::StaleSession);
        }
        Ok(inspection)
    }

    async fn drafts(&self, session_id: &str) -> Result<Vec<StagedUserInput>, UserInputQueueError> {
        self.draft_page(session_id, WorkSelector::Drafts).await
    }

    async fn draft_page(
        &self,
        session_id: &str,
        selector: WorkSelector,
    ) -> Result<Vec<StagedUserInput>, UserInputQueueError> {
        let mut query = WorkQuery::new(session_id, selector);
        let mut drafts = Vec::new();
        loop {
            let inspected = self
                .store
                .inspect_work(&query)
                .await
                .map_err(|error| UserInputQueueError::DurableRejected(error.to_string()))?;
            let WorkPage::Drafts(page) = inspected.page else {
                return Err(UserInputQueueError::IdentityConflict);
            };
            drafts.extend(
                page.into_iter()
                    .filter(|record| record.recipient_lifecycle == self.lifecycle),
            );
            let Some(cursor) = inspected.next_cursor else {
                break;
            };
            if query.cursor.as_ref() == Some(&cursor) {
                return Err(UserInputQueueError::OutcomeUnknown);
            }
            query.cursor = Some(cursor);
        }
        Ok(drafts)
    }

    async fn draft(
        &self,
        session_id: &str,
        input_id: &str,
    ) -> Result<Option<StagedUserInput>, UserInputQueueError> {
        let inspection = self
            .inspect(
                session_id,
                WorkSelector::Draft {
                    input_id: input_id.into(),
                },
            )
            .await?;
        let WorkPage::Drafts(mut drafts) = inspection.page else {
            return Err(UserInputQueueError::IdentityConflict);
        };
        Ok(drafts.pop())
    }

    async fn delivery(
        &self,
        session_id: &str,
        delivery_id: &str,
    ) -> Result<Option<Delivery>, UserInputQueueError> {
        let inspection = self
            .inspect(
                session_id,
                WorkSelector::Delivery {
                    delivery_id: delivery_id.into(),
                },
            )
            .await?;
        let WorkPage::Deliveries(mut records) = inspection.page else {
            return Err(UserInputQueueError::IdentityConflict);
        };
        Ok(records.pop())
    }

    async fn command(
        &self,
        session_id: &str,
        mutation_id: &str,
    ) -> Result<Option<OwnedWorkCommand>, UserInputQueueError> {
        let inspection = self
            .inspect(
                session_id,
                WorkSelector::Command {
                    mutation_id: mutation_id.into(),
                },
            )
            .await?;
        let WorkPage::Commands(mut records) = inspection.page else {
            return Err(UserInputQueueError::IdentityConflict);
        };
        Ok(records.pop())
    }

    async fn ensure_confirmed(&self, session_id: &str) -> Result<(), UserInputQueueError> {
        let inspection = self
            .inspect(session_id, WorkSelector::PendingCommands)
            .await?;
        let WorkPage::Commands(commands) = inspection.page else {
            return Err(UserInputQueueError::IdentityConflict);
        };
        if !commands.is_empty() {
            return Err(UserInputQueueError::OutcomeUnknown);
        }
        Ok(())
    }

    async fn input(
        &self,
        session_id: &str,
        reference: &PayloadRef,
    ) -> Result<UserInput, UserInputQueueError> {
        let evidence = self
            .store
            .read_evidence(&EvidenceQuery {
                session_id: session_id.into(),
                reference: reference.clone(),
            })
            .await
            .map_err(|error| UserInputQueueError::DurableRejected(error.to_string()))?;
        evidence
            .validate()
            .map_err(|error| UserInputQueueError::DurableRejected(error.to_string()))?;
        if evidence.reference != *reference {
            return Err(UserInputQueueError::IdentityConflict);
        }
        serde_json::from_slice::<StagedEnqueueInput>(&evidence.bytes)
            .map(|source| source.input)
            .map_err(|_| UserInputQueueError::IdentityConflict)
    }

    async fn staged_source(
        &self,
        session_id: &str,
        reference: &PayloadRef,
    ) -> Result<StagedEnqueueInput, UserInputQueueError> {
        let evidence = self
            .store
            .read_evidence(&EvidenceQuery {
                session_id: session_id.into(),
                reference: reference.clone(),
            })
            .await
            .map_err(|error| UserInputQueueError::DurableRejected(error.to_string()))?;
        evidence
            .validate()
            .map_err(|error| UserInputQueueError::DurableRejected(error.to_string()))?;
        if evidence.reference != *reference {
            return Err(UserInputQueueError::IdentityConflict);
        }
        serde_json::from_slice(&evidence.bytes).map_err(|_| UserInputQueueError::IdentityConflict)
    }

    async fn commit(
        &self,
        command: &WorkCommand,
        reconcile: bool,
    ) -> Result<WorkReceipt, UserInputQueueError> {
        if !reconcile {
            match self.store.apply_work_mutation(command).await {
                Ok(receipt) => return accepted(receipt),
                Err(error)
                    if error.effect()
                        == peri_acp_types::session_resources::MutationOutcome::NotApplied =>
                {
                    return Err(UserInputQueueError::DurableRejected(error.to_string()));
                }
                Err(error) => {
                    tracing::warn!(mutation_id = %command.mutation_id, error = %error, "user input commit requires reconciliation")
                }
            }
        }
        match self.store.resolve_work_mutation(command).await {
            Ok(WorkResolution::Applied { receipt }) => accepted(receipt),
            Ok(WorkResolution::NotApplied) => Err(UserInputQueueError::DurableRejected(
                "Original mutation was not applied".into(),
            )),
            _ => Err(UserInputQueueError::OutcomeUnknown),
        }
    }
}

fn accepted(receipt: WorkReceipt) -> Result<WorkReceipt, UserInputQueueError> {
    match receipt.decision {
        WorkDecision::Accepted => Ok(receipt),
        WorkDecision::Rejected { reason } => {
            Err(UserInputQueueError::DurableRejected(format!("{reason:?}")))
        }
    }
}

fn identity(record: &Delivery) -> Result<UserInputPublicationIdentity, UserInputQueueError> {
    serde_json::from_str(
        record
            .publication
            .event
            .causation_id
            .as_deref()
            .ok_or(UserInputQueueError::IdentityConflict)?,
    )
    .map_err(|_| UserInputQueueError::IdentityConflict)
}

fn mutation_id(command_id: &str, input_id: &str, action: &str) -> String {
    format!(
        "input:{:x}",
        Sha256::digest(
            serde_json::to_vec(&(command_id, input_id, action)).expect("identities serialize")
        )
    )
}

async fn new_publication(
    durable: &DurableMailbox,
    session_id: &str,
    input: UserInput,
    command_id: &str,
    fingerprint: u64,
    draft_revision: u64,
    draft_fingerprint: u64,
) -> Result<PublishDelivery, UserInputQueueError> {
    let payload = PersistedPayload::Message(BaseMessage::Human {
        id: MessageId::from(
            uuid::Uuid::parse_str(&input.input_id)
                .map_err(|_| UserInputQueueError::InvalidIdentity)?,
        ),
        content: input.content,
    });
    let content =
        crate::agent::stages::prepare_work_payload(durable.store.as_ref(), session_id, &payload)
            .await
            .map_err(|error| UserInputQueueError::DurableRejected(error.to_string()))?;
    Ok(PublishDelivery {
        delivery_id: delivery_id(session_id, durable.lifecycle, &input.input_id, command_id),
        event: WorkEvent {
            producer_namespace: format!("peri:user-input:{session_id}"),
            event_id: format!("user-input:{}:{command_id}", input.input_id),
            event_kind: "userInput".into(),
            causation_id: Some(
                serde_json::to_string(&UserInputPublicationIdentity {
                    input_id: input.input_id,
                    publication_generation: command_id.into(),
                    fingerprint,
                    command_id: command_id.into(),
                    draft_binding: StagedUserInputPublicationBinding {
                        draft_revision,
                        draft_fingerprint,
                        canonical_content: content.content.clone(),
                    },
                })
                .map_err(|_| UserInputQueueError::InvalidIdentity)?,
            ),
            content,
        },
        purpose: DeliveryPurpose::UserInput,
        policy: MessagePolicy::ensure_processing(),
    })
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
) -> Result<(), UserInputQueueError> {
    if operations.values().any(|operation| operation.uncertain) {
        return Err(UserInputQueueError::OutcomeUnknown);
    }
    Ok(())
}

fn frozen_operation(
    fingerprint: u64,
    commands: Vec<WorkCommand>,
    reconcile: bool,
) -> FrozenOperation {
    FrozenOperation {
        fingerprint,
        commands,
        receipt: None,
        attempted: reconcile,
        uncertain: reconcile,
        rejection: None,
    }
}

#[cfg(test)]
#[path = "staging_test.rs"]
mod staging_tests;
#[cfg(test)]
#[path = "durable_test.rs"]
mod tests;
