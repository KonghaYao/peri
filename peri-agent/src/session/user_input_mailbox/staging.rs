use super::*;
use peri_acp_types::session_resources::ControlStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PublicationBlock {
    Invalid,
    Paused,
    WorkBlocked,
    ControlInactive,
    ActiveRunning,
    ActiveInterrupted,
    ActiveCancelled,
    ActiveIdentityMismatch,
    UnattachedAttempt,
    Dispatching,
    Claimed,
    Empty,
}

impl PublicationBlock {
    fn detect(state: &MailboxState, snapshot: &WorkSnapshot) -> Option<Self> {
        if !state.valid {
            return Some(Self::Invalid);
        }
        if state.paused {
            return Some(Self::Paused);
        }
        if snapshot.blocked {
            return Some(Self::WorkBlocked);
        }
        if snapshot.control.status != ControlStatus::Active {
            return Some(Self::ControlInactive);
        }
        if let Some(active) = &state.active {
            if !state.suspended {
                return Some(Self::ActiveRunning);
            }
            if active.reason != InterruptReason::None {
                return Some(Self::ActiveInterrupted);
            }
            if active
                .cancel
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                return Some(Self::ActiveCancelled);
            }
            if active.sdk.as_ref().is_some_and(|sdk| {
                snapshot.control.attempt.as_ref() != Some(&sdk.admission.execution)
                    || snapshot.control.control_generation != sdk.admission.control_generation
            }) {
                return Some(Self::ActiveIdentityMismatch);
            }
        } else if snapshot.control.attempt.is_some() {
            return Some(Self::UnattachedAttempt);
        }
        state.records.iter().find_map(|record| match record.state {
            UserInputState::Dispatching => Some(Self::Dispatching),
            UserInputState::Claimed => Some(Self::Claimed),
            _ => None,
        })
    }
}

enum InputSelection {
    Automatic,
    InterruptCurrent,
    /// 暂停后的用户新输入：固定无 attempt 预期，由发布事务自动 Resume 控制状态。
    ResumeNewTask,
    Authorized(Box<WorkCommand>),
}

#[derive(Serialize, Deserialize)]
struct StagedEnqueueInput {
    #[serde(flatten)]
    input: UserInput,
    #[serde(default)]
    enqueue_publication: Option<WorkCommand>,
}

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
            InputSelection::Automatic,
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

    #[tracing::instrument(skip_all, fields(input_id = %request.input_id, command_id = %request.command_id))]
    pub async fn enqueue_durable(
        &self,
        request: &EnqueueUserInputRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let durable = self.durable.as_ref().ok_or(UserInputQueueError::Closed)?;
        let fingerprint = compute_fingerprint(("enqueue", request));
        let lock_started = std::time::Instant::now();
        let mut operations = durable.operations.lock().await;
        tracing::debug!(
            duration_us = lock_started.elapsed().as_micros() as u64,
            "mailbox enqueue operation lock acquired"
        );
        self.validate_request(
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        validate_input_id(&request.input_id)?;
        if request.content.is_empty() {
            return Err(UserInputQueueError::EmptyContent);
        }
        // 首次接纳时是否已处于暂停：决定这条输入是"暂停后的新提交"还是"暂停前的旧待办"。
        // 重放同一 command 不重新取得该授权。
        let mut resume_intent_candidate = false;
        if !operations.contains_key(&request.command_id) {
            ensure_not_frozen(&operations, std::slice::from_ref(&request.input_id))?;
            let snapshot = durable.load(&self.session_id).await?;
            resume_intent_candidate =
                self.state.lock().paused || snapshot.control.status == ControlStatus::Paused;
            let staged = snapshot
                .state
                .staged_user_inputs
                .get(&request.input_id)
                .filter(|record| record.command_id == request.command_id);
            let pending = if staged.is_some() {
                Vec::new()
            } else {
                recover_pending(
                    &snapshot,
                    &request.command_id,
                    fingerprint,
                    std::slice::from_ref(&request.input_id),
                )?
            };
            let was_pending = !pending.is_empty();
            let input = UserInput {
                input_id: request.input_id.clone(),
                content: request.content.clone(),
                original_draft: request.original_draft.clone(),
            };
            let input_json = if let Some(record) = staged {
                if record.fingerprint != fingerprint {
                    return Err(UserInputQueueError::IdentityConflict);
                }
                record.input_json.clone()
            } else {
                serde_json::to_string(&StagedEnqueueInput {
                    enqueue_publication: self.authorize_enqueue_publication(
                        &snapshot,
                        &request.command_id,
                        &input,
                    )?,
                    input,
                })
                .map_err(|_| UserInputQueueError::InvalidIdentity)?
            };
            let command = WorkCommand {
                session_id: self.session_id.clone(),
                recipient_lifecycle: durable.lifecycle,
                mutation_id: mutation_id(
                    &format!(
                        "{}:{}:{}",
                        self.session_id, durable.lifecycle, request.command_id
                    ),
                    &request.input_id,
                    "stage",
                ),
                action: WorkAction::StageUserInput {
                    input_json,
                    command_id: request.command_id.clone(),
                    fingerprint,
                },
            };
            operations.insert(
                request.command_id.clone(),
                frozen_operation(
                    fingerprint,
                    if was_pending { pending } else { vec![command] },
                    was_pending,
                ),
            );
        }
        let stage_started = std::time::Instant::now();
        let (mut receipt, mut snapshot) = self
            .finish_operation(
                durable,
                &mut operations,
                &request.command_id,
                fingerprint,
                std::slice::from_ref(&request.input_id),
                false,
            )
            .await?;
        tracing::debug!(
            duration_us = stage_started.elapsed().as_micros() as u64,
            "mailbox enqueue stage confirmed"
        );
        drop(operations);
        if let Some(record) = snapshot
            .state
            .staged_user_inputs
            .get(&request.input_id)
            .filter(|record| {
                record.command_id == request.command_id
                    && record.status == StagedUserInputStatus::Queued
                    && record.publication_generation == 0
            })
        {
            let staged: StagedEnqueueInput = serde_json::from_str(&record.input_json)
                .map_err(|_| UserInputQueueError::IdentityConflict)?;
            if let Some(command) = staged.enqueue_publication {
                let command_id = format!("idle:{}:{}:0", request.command_id, record.sequence);
                let input_ids = vec![request.input_id.clone()];
                let publication_started = std::time::Instant::now();
                let (publication_receipt, publication_snapshot) = self
                    .publish_selection_with_snapshot(
                        &command_id,
                        compute_fingerprint(("idle", &command_id, &input_ids)),
                        &input_ids,
                        InputSelection::Authorized(Box::new(command)),
                        Some(snapshot),
                    )
                    .await?;
                tracing::debug!(
                    duration_us = publication_started.elapsed().as_micros() as u64,
                    "mailbox enqueue publication confirmed"
                );
                snapshot = publication_snapshot;
                receipt
                    .work_receipts
                    .extend(publication_receipt.work_receipts);
                receipt
                    .publication_generations
                    .extend(publication_receipt.publication_generations);
                let mut state = self.state.lock();
                state.suspended = false;
                if resume_intent_candidate {
                    // 发布事务已按新任务恢复会话，暂停位随之解除。
                    state.paused = false;
                }
            }
        }
        receipt.snapshot = self.snapshot();
        receipt.results = results_for(&self.state.lock(), std::slice::from_ref(&request.input_id));
        let idle_id = format!(
            "idle:{}:{}:0",
            request.command_id, receipt.work_receipts[0].revision
        );
        let mutation = mutation_id(
            &format!("{}:{}:{idle_id}", self.session_id, durable.lifecycle),
            "selection",
            "publish",
        );
        if receipt.publication_generations.is_empty() {
            if let Some(command) = snapshot.state.user_input_publications.get(&mutation) {
                let publication_receipt = match durable.store.resolve_work_mutation(command).await {
                    Ok(WorkResolution::Applied { receipt }) => accepted(receipt)?,
                    _ => return Err(UserInputQueueError::OutcomeUnknown),
                };
                receipt.work_receipts.push(publication_receipt);
                receipt
                    .publication_generations
                    .insert(request.input_id.clone(), idle_id);
                project_withdrawn_results(&mut receipt, std::slice::from_ref(command), &snapshot);
            }
        }
        // 暂停期间提交、且此刻仍未发布的输入携带恢复授权：
        // 收尾完成后由 publish_next_durable 按新任务发布并自动解除暂停。
        if resume_intent_candidate
            && receipt
                .results
                .first()
                .is_some_and(|result| result.state == UserInputState::Queued)
        {
            self.state
                .lock()
                .resume_intents
                .insert(request.input_id.clone());
            // 收尾可能在本次写入前后恰好结束，宿主收尾任务不会为此再被唤醒；
            // 这里立即尝试一次发布。失败时授权与待发记录都保留，等待后续唤醒。
            match self.publish_next_durable().await {
                Ok(_) => {
                    receipt.snapshot = self.snapshot();
                    receipt.results =
                        results_for(&self.state.lock(), std::slice::from_ref(&request.input_id));
                }
                Err(error) => {
                    tracing::warn!(session_id = %self.session_id, %error, "resume publication unconfirmed");
                }
            }
        }
        Ok(receipt)
    }

    fn authorize_enqueue_publication(
        &self,
        snapshot: &WorkSnapshot,
        enqueue_id: &str,
        input: &UserInput,
    ) -> Result<Option<WorkCommand>, UserInputQueueError> {
        self.project_publications(snapshot);
        let has_inactive_work = snapshot.state.works.values().any(|work| {
            !matches!(work.stage, WorkStage::Settled | WorkStage::Abandoned)
                && snapshot.state.work_lifecycle(&work.work_id) == Some(snapshot.control.lifecycle)
        });
        let state = self.state.lock();
        let has_processing_input = state.records.iter().any(|record| {
            matches!(
                record.state,
                UserInputState::Dispatching | UserInputState::Claimed
            )
        });
        if !state.valid
            || !matches!(
                snapshot.control.status,
                ControlStatus::Active | ControlStatus::Paused
            )
            || !snapshot.pending_commands.is_empty()
        {
            return Ok(None);
        }
        let paused = state.paused || snapshot.control.status == ControlStatus::Paused;
        let new_task = state.active.is_none()
            && snapshot.control.attempt.is_none()
            && (has_inactive_work || !has_processing_input);
        // 显式停止、暂停或执行失败之后的用户新提交是"有明确恢复语义的发送"：
        // 空闲新输入按新任务发布，由发布事务自动 Resume；旧待办与自动路径保持暂停。
        let automatic = !paused
            && !snapshot.blocked
            && !has_processing_input
            && state.active.as_ref().is_some_and(|active| {
                state.suspended
                    && active.reason == InterruptReason::None
                    && !active
                        .cancel
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled)
                    && !active.sdk.as_ref().is_some_and(|sdk| {
                        snapshot.control.attempt.as_ref() != Some(&sdk.admission.execution)
                            || snapshot.control.control_generation
                                != sdk.admission.control_generation
                    })
            })
            && !snapshot.state.staged_user_inputs.values().any(|record| {
                record.recipient_lifecycle == snapshot.control.lifecycle
                    && record.status == StagedUserInputStatus::Queued
            });
        if !new_task && !automatic {
            return Ok(None);
        }
        drop(state);
        let expected_revision = snapshot
            .state
            .revision
            .checked_add(1)
            .ok_or(UserInputQueueError::InvalidIdentity)?;
        let command_id = format!("idle:{enqueue_id}:{expected_revision}:0");
        let input_ids = vec![input.input_id.clone()];
        let publication = new_publication(
            &self.session_id,
            snapshot.control.lifecycle,
            input.clone(),
            &command_id,
            compute_fingerprint(("idle", &command_id, &input_ids)),
        )?;
        let WorkAction::PublishDelivery { delivery } = publication.action else {
            unreachable!()
        };
        Ok(Some(WorkCommand {
            session_id: self.session_id.clone(),
            recipient_lifecycle: snapshot.control.lifecycle,
            mutation_id: mutation_id(
                &format!(
                    "{}:{}:{command_id}",
                    self.session_id, snapshot.control.lifecycle
                ),
                "selection",
                "publish",
            ),
            action: WorkAction::PublishStagedUserInputs {
                expected_revision,
                expected_control_generation: snapshot.control.control_generation,
                expected_attempt: if new_task {
                    None
                } else {
                    snapshot.control.attempt.clone()
                },
                interrupt_current: new_task,
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
            || request
                .input_ids
                .iter()
                .any(|id| validate_input_id(id).is_err() || !distinct.insert(id))
        {
            return Err(UserInputQueueError::InvalidIdentity);
        }
        let receipt = self
            .publish_selection(
                &request.command_id,
                compute_fingerprint(("dispatch", request)),
                &request.input_ids,
                InputSelection::InterruptCurrent,
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
        let snapshot = durable.load(&self.session_id).await?;
        self.project_publications(&snapshot);
        let resuming = self.resuming_new_task(&snapshot);
        if !resuming {
            let blocked = PublicationBlock::detect(&self.state.lock(), &snapshot);
            if let Some(reason) = blocked {
                return Ok(self.record_publication_block(reason, &snapshot));
            }
        }
        let resume_intents = self.state.lock().resume_intents.clone();
        let Some(record) = snapshot
            .state
            .staged_user_inputs
            .values()
            .filter(|record| {
                record.recipient_lifecycle == durable.lifecycle
                    && record.status == StagedUserInputStatus::Queued
                    && (!resuming || resume_intents.contains(&record.input_id))
            })
            .min_by_key(|record| record.sequence)
        else {
            return Ok(self.record_publication_block(PublicationBlock::Empty, &snapshot));
        };
        let command_id = format!(
            "idle:{}:{}:{}",
            record.command_id, record.sequence, record.publication_generation
        );
        let ids = vec![record.input_id.clone()];
        let selection = if resuming {
            InputSelection::ResumeNewTask
        } else {
            InputSelection::Automatic
        };
        self.publish_selection_with_snapshot(
            &command_id,
            compute_fingerprint(("idle", &command_id, &ids)),
            &ids,
            selection,
            Some(snapshot),
        )
        .await?;
        {
            let mut state = self.state.lock();
            state.suspended = false;
            if resuming {
                // 发布事务已按新任务恢复会话：解除暂停位与恢复授权。
                state.paused = false;
                state.resume_intents.remove(&ids[0]);
            }
        }
        *durable.publication_block.lock() = None;
        Ok(true)
    }

    /// 暂停（Stop/Pause/失败）之后用户明确提交的输入，在无活跃执行时按新任务恢复。
    /// 暂停之前入队的旧待办不获得该授权，保持等待显式发送。
    fn resuming_new_task(&self, snapshot: &WorkSnapshot) -> bool {
        let state = self.state.lock();
        if !state.valid
            || state.active.is_some()
            || snapshot.control.attempt.is_some()
            || !matches!(
                snapshot.control.status,
                ControlStatus::Active | ControlStatus::Paused
            )
            || (!state.paused && snapshot.control.status != ControlStatus::Paused)
            || state.resume_intents.is_empty()
        {
            return false;
        }
        snapshot.state.staged_user_inputs.values().any(|record| {
            record.recipient_lifecycle == snapshot.control.lifecycle
                && record.status == StagedUserInputStatus::Queued
                && state.resume_intents.contains(&record.input_id)
        })
    }

    fn record_publication_block(&self, reason: PublicationBlock, snapshot: &WorkSnapshot) -> bool {
        let durable = self.durable.as_ref().expect("durable publication");
        let mut previous = durable.publication_block.lock();
        // 轮询和多个调用方共用去重状态，只在阻塞原因变化时记录，不输出输入内容。
        if *previous != Some(reason) {
            *previous = Some(reason);
            let state = self.state.lock();
            tracing::debug!(
                session_id = %self.session_id,
                ?reason,
                lifecycle = snapshot.control.lifecycle,
                control_generation = snapshot.control.control_generation,
                work_revision = snapshot.state.revision,
                queued = state.records.iter().filter(|r| r.state == UserInputState::Queued).count(),
                dispatching = state.records.iter().filter(|r| r.state == UserInputState::Dispatching).count(),
                claimed = state.records.iter().filter(|r| r.state == UserInputState::Claimed).count(),
                "pending input publication deferred"
            );
        }
        false
    }

    async fn publish_selection(
        &self,
        command_id: &str,
        fingerprint: u64,
        input_ids: &[String],
        selection: InputSelection,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        self.publish_selection_with_snapshot(command_id, fingerprint, input_ids, selection, None)
            .await
            .map(|(receipt, _)| receipt)
    }

    async fn publish_selection_with_snapshot(
        &self,
        command_id: &str,
        fingerprint: u64,
        input_ids: &[String],
        selection: InputSelection,
        confirmed_snapshot: Option<WorkSnapshot>,
    ) -> Result<(UserInputQueueReceipt, WorkSnapshot), UserInputQueueError> {
        let durable = self.durable.as_ref().ok_or(UserInputQueueError::Closed)?;
        let mut operations = durable.operations.lock().await;
        if !operations.contains_key(command_id) {
            ensure_not_frozen(&operations, input_ids)?;
            let snapshot = match confirmed_snapshot {
                Some(snapshot) => snapshot,
                None => durable.load(&self.session_id).await?,
            };
            self.project_publications(&snapshot);
            let pending = recover_pending(&snapshot, command_id, fingerprint, input_ids)?;
            let mutation = mutation_id(
                &format!("{}:{}:{command_id}", self.session_id, durable.lifecycle),
                "selection",
                "publish",
            );
            let saved = snapshot.state.user_input_publications.get(&mutation);
            let commands = if !pending.is_empty() {
                pending
            } else if let Some(command) = saved {
                let WorkAction::PublishStagedUserInputs { deliveries, .. } = &command.action else {
                    return Err(UserInputQueueError::IdentityConflict);
                };
                if deliveries.len() != input_ids.len()
                    || deliveries.iter().any(|delivery| {
                        serde_json::from_str::<PublicationIdentity>(
                            delivery.event.causation_id.as_deref().unwrap_or_default(),
                        )
                        .map_or(true, |identity| {
                            identity.command_id != command_id
                                || identity.fingerprint != fingerprint
                                || !input_ids.contains(&identity.input.input_id)
                        })
                    })
                {
                    return Err(UserInputQueueError::IdentityConflict);
                }
                vec![command.clone()]
            } else if let InputSelection::Authorized(command) = selection {
                if command.session_id != self.session_id
                    || command.recipient_lifecycle != durable.lifecycle
                    || command.mutation_id != mutation
                {
                    return Err(UserInputQueueError::IdentityConflict);
                }
                vec![*command]
            } else {
                let mut selected: Vec<_> = snapshot
                    .state
                    .staged_user_inputs
                    .values()
                    .filter(|record| {
                        input_ids.contains(&record.input_id)
                            && record.recipient_lifecycle == durable.lifecycle
                    })
                    .collect();
                if selected.len() != input_ids.len()
                    || selected
                        .iter()
                        .any(|record| record.status != StagedUserInputStatus::Queued)
                {
                    return Err(UserInputQueueError::IdentityConflict);
                }
                selected.sort_by_key(|record| record.sequence);
                let mut deliveries = Vec::new();
                for record in selected {
                    let input = serde_json::from_str(&record.input_json)
                        .map_err(|_| UserInputQueueError::IdentityConflict)?;
                    let command = new_publication(
                        &self.session_id,
                        durable.lifecycle,
                        input,
                        command_id,
                        fingerprint,
                    )?;
                    let WorkAction::PublishDelivery { delivery } = command.action else {
                        unreachable!()
                    };
                    deliveries.push(delivery);
                }
                let (expected_control_generation, expected_attempt, interrupt_current) =
                    match selection {
                        InputSelection::Automatic => (
                            snapshot.control.control_generation,
                            snapshot.control.attempt.clone(),
                            false,
                        ),
                        InputSelection::InterruptCurrent => (
                            snapshot.control.control_generation,
                            snapshot.control.attempt.clone(),
                            true,
                        ),
                        InputSelection::ResumeNewTask => {
                            (snapshot.control.control_generation, None, true)
                        }
                        InputSelection::Authorized(_) => unreachable!(),
                    };
                vec![WorkCommand {
                    session_id: self.session_id.clone(),
                    recipient_lifecycle: durable.lifecycle,
                    mutation_id: mutation,
                    action: WorkAction::PublishStagedUserInputs {
                        expected_revision: snapshot.state.revision,
                        expected_control_generation,
                        expected_attempt,
                        interrupt_current,
                        deliveries,
                    },
                }]
            };
            let reconcile = saved.is_some() || !snapshot.pending_commands.is_empty();
            operations.insert(
                command_id.into(),
                frozen_operation(fingerprint, commands, reconcile),
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

    pub(super) fn project_staged_inputs(&self, state: &mut MailboxState, snapshot: &WorkSnapshot) {
        let mut staged: Vec<_> = snapshot
            .state
            .staged_user_inputs
            .values()
            .filter(|record| record.recipient_lifecycle == snapshot.control.lifecycle)
            .collect();
        staged.sort_by_key(|record| record.sequence);
        for staged in staged {
            let status = match staged.status {
                StagedUserInputStatus::Queued => UserInputState::Queued,
                StagedUserInputStatus::Published => {
                    state.resume_intents.remove(&staged.input_id);
                    continue;
                }
                StagedUserInputStatus::Withdrawn => {
                    state.resume_intents.remove(&staged.input_id);
                    UserInputState::Withdrawn
                }
            };
            let Ok(input) = serde_json::from_str::<UserInput>(&staged.input_json) else {
                continue;
            };
            let position = state
                .records
                .iter()
                .position(|record| record.input.input_id == staged.input_id);
            let record = if let Some(position) = position {
                &mut state.records[position]
            } else {
                state.records.push(InputRecord {
                    input: input.clone(),
                    fingerprint: compute_fingerprint(&input),
                    state: status,
                    handed_off: false,
                    publication_id: None,
                    publication_generation: None,
                });
                state.records.last_mut().expect("staged input inserted")
            };
            if record.handed_off {
                let id = MessageId::from(
                    uuid::Uuid::parse_str(&record.input.input_id).expect("validated staged input"),
                );
                self.inbox.queue().withdraw_user_inputs(&[id]);
            }
            record.input = input;
            record.state = status;
            record.handed_off = false;
            record.publication_id = None;
            record.publication_generation = None;
        }
    }
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
