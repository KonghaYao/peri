use super::*;
use peri_acp_types::session_resources::ControlStatus;

impl UserInputMailbox {
    pub(crate) async fn publish_prompt_durable(
        &self,
        request: &EnqueueUserInputRequest,
        ticket: &UserInputRunTicket,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        self.validate_prompt_attempt(ticket)?;
        self.enqueue_durable(request).await?;
        self.validate_prompt_attempt(ticket)?;
        let command_id = format!("prompt:{}", request.command_id);
        let input_ids = vec![request.input_id.clone()];
        self.publish_selection(
            &command_id,
            compute_fingerprint(("prompt", &command_id, &input_ids)),
            &input_ids,
            false,
        )
        .await
    }

    fn validate_prompt_attempt(
        &self,
        ticket: &UserInputRunTicket,
    ) -> Result<(), UserInputQueueError> {
        let state = self.state.lock();
        if !state.valid
            || state.paused
            || !state.active.as_ref().is_some_and(|active| {
                active.ticket == *ticket
                    && !active.managed
                    && active.sdk.is_none()
                    && active.reason == InterruptReason::None
                    && active
                        .cancel
                        .as_ref()
                        .is_some_and(|cancel| !cancel.is_cancelled())
            })
        {
            return Err(UserInputQueueError::DurableRejected(
                "Initial prompt execution attachment is not current".into(),
            ));
        }
        Ok(())
    }

    pub(crate) async fn enter_idle_durable(&self) -> Result<(), UserInputQueueError> {
        self.enter_idle();
        if self.durable.is_some() {
            self.publish_next_durable().await?;
        }
        Ok(())
    }

    pub async fn enqueue_durable(
        &self,
        request: &EnqueueUserInputRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let durable = self.durable.as_ref().ok_or(UserInputQueueError::Closed)?;
        self.validate_request(
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        validate_input_id(&request.input_id)?;
        if request.content.is_empty() {
            return Err(UserInputQueueError::EmptyContent);
        }
        let fingerprint = compute_fingerprint(("enqueue", request));
        let mut operations = durable.operations.lock().await;
        if !operations.contains_key(&request.command_id) {
            ensure_not_frozen(&operations)?;
            let mutation = mutation_id(
                &format!(
                    "{}:{}:{}",
                    self.session_id, durable.lifecycle, request.command_id
                ),
                &request.input_id,
                "stage",
            );
            let saved = durable.command(&self.session_id, &mutation).await?;
            let reconcile = saved.is_some();
            let command = if let Some(saved) = saved {
                let WorkAction::StageUserInput {
                    fingerprint: original,
                    ..
                } = &saved.command.action
                else {
                    return Err(UserInputQueueError::IdentityConflict);
                };
                if *original != fingerprint {
                    return Err(UserInputQueueError::IdentityConflict);
                }
                saved.command
            } else {
                durable.ensure_confirmed(&self.session_id).await?;
                if let Some(prior) = durable.draft(&self.session_id, &request.input_id).await? {
                    if prior.status != StagedUserInputStatus::Withdrawn
                        && (prior.command_id != request.command_id
                            || prior.fingerprint != fingerprint)
                    {
                        return Err(UserInputQueueError::IdentityConflict);
                    }
                }
                let input = UserInput {
                    input_id: request.input_id.clone(),
                    content: request.content.clone(),
                    original_draft: request.original_draft.clone(),
                };
                let enqueue_publication = self
                    .authorize_enqueue_publication(
                        durable,
                        &request.command_id,
                        &input,
                        fingerprint,
                    )
                    .await?;
                let content = crate::agent::stages::prepare_work_evidence(
                    durable.store.as_ref(),
                    &self.session_id,
                    serde_json::to_vec(&StagedEnqueueInput {
                        input,
                        enqueue_publication,
                    })
                    .map_err(|_| UserInputQueueError::InvalidIdentity)?,
                )
                .await
                .map_err(|error| UserInputQueueError::DurableRejected(error.to_string()))?;
                WorkCommand {
                    session_id: self.session_id.clone(),
                    recipient_lifecycle: durable.lifecycle,
                    mutation_id: mutation,
                    action: WorkAction::StageUserInput {
                        input_id: request.input_id.clone(),
                        content,
                        command_id: request.command_id.clone(),
                        fingerprint,
                    },
                }
            };
            operations.insert(
                request.command_id.clone(),
                frozen_operation(fingerprint, vec![command], reconcile),
            );
        }
        let mut receipt = self
            .finish_operation(
                durable,
                &mut operations,
                &request.command_id,
                fingerprint,
                std::slice::from_ref(&request.input_id),
                false,
            )
            .await?;
        let original_source = match &operations[&request.command_id].commands[0].action {
            WorkAction::StageUserInput { content, .. } => content.clone(),
            _ => return Err(UserInputQueueError::IdentityConflict),
        };
        drop(operations);
        let source = durable
            .staged_source(&self.session_id, &original_source)
            .await?;
        if let Some(command) = source.enqueue_publication {
            let WorkAction::PublishStagedUserInputs { deliveries, .. } = &command.action else {
                return Err(UserInputQueueError::IdentityConflict);
            };
            if deliveries.len() != 1 || source.input.input_id != request.input_id {
                return Err(UserInputQueueError::IdentityConflict);
            }
            let publication: UserInputPublicationIdentity = serde_json::from_str(
                deliveries
                    .first()
                    .and_then(|delivery| delivery.event.causation_id.as_deref())
                    .ok_or(UserInputQueueError::IdentityConflict)?,
            )
            .map_err(|_| UserInputQueueError::IdentityConflict)?;
            if publication.input_id != source.input.input_id
                || publication.draft_binding.draft_fingerprint != fingerprint
                || publication.draft_binding.canonical_content
                    != deliveries[0].event.content.content
            {
                return Err(UserInputQueueError::IdentityConflict);
            }
            let evidence = durable
                .store
                .read_evidence(&EvidenceQuery {
                    session_id: self.session_id.clone(),
                    reference: publication.draft_binding.canonical_content.clone(),
                })
                .await
                .map_err(|error| UserInputQueueError::DurableRejected(error.to_string()))?;
            evidence
                .validate()
                .map_err(|error| UserInputQueueError::DurableRejected(error.to_string()))?;
            if evidence.reference != publication.draft_binding.canonical_content {
                return Err(UserInputQueueError::IdentityConflict);
            }
            let encoded = std::str::from_utf8(&evidence.bytes)
                .map_err(|_| UserInputQueueError::IdentityConflict)?;
            let payload = peri_acp_types::store::deserialize_persisted_payload(encoded)
                .map_err(|_| UserInputQueueError::IdentityConflict)?;
            if !matches!(payload, PersistedPayload::Message(BaseMessage::Human { id, content })
                if id == deliveries[0].event.content.message_id
                    && id.as_uuid().to_string() == source.input.input_id
                    && content == source.input.content)
            {
                return Err(UserInputQueueError::IdentityConflict);
            }
            let replay = durable
                .command(&self.session_id, &command.mutation_id)
                .await?
                .is_some();
            let mut operations = durable.operations.lock().await;
            operations
                .entry(publication.command_id.clone())
                .or_insert_with(|| {
                    frozen_operation(publication.fingerprint, vec![command], replay)
                });
            let publication_receipt = self
                .finish_operation(
                    durable,
                    &mut operations,
                    &publication.command_id,
                    publication.fingerprint,
                    std::slice::from_ref(&request.input_id),
                    false,
                )
                .await?;
            receipt
                .work_receipts
                .extend(publication_receipt.work_receipts);
            receipt
                .publication_generations
                .extend(publication_receipt.publication_generations);
        }
        receipt.snapshot = self.snapshot();
        receipt.results = results_for(&self.state.lock(), std::slice::from_ref(&request.input_id));
        Ok(receipt)
    }

    async fn authorize_enqueue_publication(
        &self,
        durable: &DurableMailbox,
        enqueue_id: &str,
        input: &UserInput,
        draft_fingerprint: u64,
    ) -> Result<Option<WorkCommand>, UserInputQueueError> {
        let head = durable
            .inspect(&self.session_id, WorkSelector::Head)
            .await?;
        let abandon_previous = head.control.attempt.is_none()
            && head.head.current_processing_id.is_some()
            && self.state.lock().active.is_none();
        let authorized = {
            let state = self.state.lock();
            state.valid
                && !state.paused
                && head.control.status == ControlStatus::Active
                && ((state.active.is_none() && head.control.attempt.is_none())
                    || (state.suspended
                        && state.active.as_ref().is_some_and(|active| {
                            active.reason == InterruptReason::None
                                && !active
                                    .cancel
                                    .as_ref()
                                    .is_some_and(CancellationToken::is_cancelled)
                        })))
                && (abandon_previous
                    || !state.records.iter().any(|record| {
                        matches!(
                            record.state,
                            UserInputState::Queued
                                | UserInputState::Dispatching
                                | UserInputState::Claimed
                        )
                    }))
        };
        if !authorized {
            return Ok(None);
        }
        let revision = head
            .head
            .change_seq
            .checked_add(1)
            .ok_or(UserInputQueueError::InvalidIdentity)?;
        let command_id = format!("idle:{enqueue_id}:{revision}:0");
        let input_ids = vec![input.input_id.clone()];
        let fingerprint = compute_fingerprint(("idle", &command_id, &input_ids));
        let draft_revision = match durable.draft(&self.session_id, &input.input_id).await? {
            Some(prior) => prior
                .revision
                .checked_add(1)
                .ok_or(UserInputQueueError::InvalidIdentity)?,
            None => 0,
        };
        let delivery = new_publication(
            durable,
            &self.session_id,
            input.clone(),
            &command_id,
            fingerprint,
            draft_revision,
            draft_fingerprint,
        )
        .await?;
        Ok(Some(WorkCommand {
            session_id: self.session_id.clone(),
            recipient_lifecycle: durable.lifecycle,
            mutation_id: mutation_id(
                &format!("{}:{}:{command_id}", self.session_id, durable.lifecycle),
                "selection",
                "publish",
            ),
            action: WorkAction::PublishStagedUserInputs {
                expected_revision: revision,
                expected_control_generation: head.control.control_generation,
                expected_attempt: head.control.attempt,
                interrupt_current: abandon_previous,
                deliveries: vec![delivery],
            },
        }))
    }

    pub async fn dispatch_durable(
        &self,
        request: &DispatchUserInputsRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        self.validate_request(
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        let mut distinct = std::collections::HashSet::new();
        if request.input_ids.is_empty()
            || request.input_ids.len() > MAX_WORK_PAGE_SIZE as usize
            || request
                .input_ids
                .iter()
                .any(|input_id| validate_input_id(input_id).is_err() || !distinct.insert(input_id))
        {
            return Err(UserInputQueueError::InvalidIdentity);
        }
        let receipt = self
            .publish_selection(
                &request.command_id,
                compute_fingerprint(("dispatch", request)),
                &request.input_ids,
                true,
            )
            .await?;
        let durable = self.durable.as_ref().ok_or(UserInputQueueError::Closed)?;
        let operations = durable.operations.lock().await;
        let target = operations
            .get(&request.command_id)
            .and_then(|operation| operation.commands.first())
            .and_then(|command| match &command.action {
                WorkAction::PublishStagedUserInputs {
                    expected_attempt, ..
                } => expected_attempt.clone(),
                _ => None,
            });
        let cancel = {
            let mut state = self.state.lock();
            if state
                .active
                .as_ref()
                .and_then(|active| active.sdk.as_ref())
                .is_some_and(|sdk| Some(&sdk.admission.execution) != target.as_ref())
            {
                return Ok(receipt);
            }
            state.paused = false;
            state.active.as_mut().and_then(|active| {
                if active.reason == InterruptReason::Steer {
                    return None;
                }
                active.reason = InterruptReason::Steer;
                active.cancel.clone()
            })
        };
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        Ok(receipt)
    }

    pub async fn publish_next_durable(&self) -> Result<bool, UserInputQueueError> {
        let durable = self.durable.as_ref().ok_or(UserInputQueueError::Closed)?;
        let head = durable
            .inspect(&self.session_id, WorkSelector::Availability)
            .await?;
        let WorkPage::Availability(availability) = &head.page else {
            return Err(UserInputQueueError::IdentityConflict);
        };
        self.refresh_durable().await?;
        let blocked = {
            let state = self.state.lock();
            !state.valid
                || state.paused
                || availability.blocked
                || head.control.status != ControlStatus::Active
                || state.records.iter().any(|record| {
                    matches!(
                        record.state,
                        UserInputState::Dispatching | UserInputState::Claimed
                    )
                })
                || state.active.as_ref().is_some_and(|active| {
                    !state.suspended
                        || active.reason != InterruptReason::None
                        || active
                            .cancel
                            .as_ref()
                            .is_some_and(CancellationToken::is_cancelled)
                        || active.sdk.as_ref().is_some_and(|sdk| {
                            head.control.attempt.as_ref() != Some(&sdk.admission.execution)
                                || head.control.control_generation
                                    != sdk.admission.control_generation
                        })
                })
                || (state.active.is_none() && head.control.attempt.is_some())
        };
        if blocked {
            return Ok(false);
        }
        durable.ensure_confirmed(&self.session_id).await?;
        let draft = durable
            .drafts(&self.session_id)
            .await?
            .into_iter()
            .filter(|draft| draft.status == StagedUserInputStatus::Queued)
            .min_by_key(|draft| draft.sequence);
        let Some(draft) = draft else { return Ok(false) };
        let command_id = format!(
            "idle:{}:{}:{}",
            draft.command_id, draft.sequence, draft.publication_generation
        );
        let input_ids = vec![draft.input_id];
        self.publish_selection(
            &command_id,
            compute_fingerprint(("idle", &command_id, &input_ids)),
            &input_ids,
            false,
        )
        .await?;
        self.state.lock().suspended = false;
        Ok(true)
    }

    async fn publish_selection(
        &self,
        command_id: &str,
        fingerprint: u64,
        input_ids: &[String],
        interrupt_current: bool,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let durable = self.durable.as_ref().ok_or(UserInputQueueError::Closed)?;
        if input_ids.len() > MAX_WORK_PAGE_SIZE as usize {
            return Err(UserInputQueueError::Capacity);
        }
        let mut operations = durable.operations.lock().await;
        if !operations.contains_key(command_id) {
            ensure_not_frozen(&operations)?;
            let mutation = mutation_id(
                &format!("{}:{}:{command_id}", self.session_id, durable.lifecycle),
                "selection",
                "publish",
            );
            let saved = durable.command(&self.session_id, &mutation).await?;
            let reconcile = saved.is_some();
            let command = if let Some(saved) = saved {
                let WorkAction::PublishStagedUserInputs { deliveries, .. } = &saved.command.action
                else {
                    return Err(UserInputQueueError::IdentityConflict);
                };
                if deliveries.len() != input_ids.len()
                    || deliveries.iter().any(|delivery| {
                        serde_json::from_str::<UserInputPublicationIdentity>(
                            delivery.event.causation_id.as_deref().unwrap_or_default(),
                        )
                        .map_or(true, |identity| {
                            identity.command_id != command_id
                                || identity.fingerprint != fingerprint
                                || !input_ids.contains(&identity.input_id)
                        })
                    })
                {
                    return Err(UserInputQueueError::IdentityConflict);
                }
                saved.command
            } else {
                durable.ensure_confirmed(&self.session_id).await?;
                let mut selected = Vec::new();
                for input_id in input_ids {
                    let record = durable
                        .draft(&self.session_id, input_id)
                        .await?
                        .ok_or(UserInputQueueError::IdentityConflict)?;
                    if record.status != StagedUserInputStatus::Queued {
                        return Err(UserInputQueueError::IdentityConflict);
                    }
                    selected.push(record);
                }
                selected.sort_by_key(|record| record.sequence);
                let mut deliveries = Vec::new();
                for record in selected {
                    let input = durable.input(&self.session_id, &record.content).await?;
                    deliveries.push(
                        new_publication(
                            durable,
                            &self.session_id,
                            input,
                            command_id,
                            fingerprint,
                            record.revision,
                            record.fingerprint,
                        )
                        .await?,
                    );
                }
                let head = durable
                    .inspect(&self.session_id, WorkSelector::Head)
                    .await?;
                WorkCommand {
                    session_id: self.session_id.clone(),
                    recipient_lifecycle: durable.lifecycle,
                    mutation_id: mutation,
                    action: WorkAction::PublishStagedUserInputs {
                        expected_revision: head.head.change_seq,
                        expected_control_generation: head.control.control_generation,
                        expected_attempt: head.control.attempt,
                        interrupt_current,
                        deliveries,
                    },
                }
            };
            operations.insert(
                command_id.into(),
                frozen_operation(fingerprint, vec![command], reconcile),
            );
        }
        self.finish_operation(
            durable,
            &mut operations,
            command_id,
            fingerprint,
            input_ids,
            false,
        )
        .await
    }
}
