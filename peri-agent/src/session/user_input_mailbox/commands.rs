//! 入队、立即发送与取回命令，以及命令回执与发送选择。

use std::collections::HashSet;

use peri_acp_types::{
    messages::MessageId,
    session::{
        DispatchUserInputsRequest, EnqueueUserInputRequest, MqCancelOutcome, MqEnqueueDisposition,
        MqSendStatus, QueuedMessage, TakeBackUserInputRequest, UserInput, UserInputItemResult,
        UserInputQueueReceipt, UserInputState,
    },
};

use super::state::{
    compute_fingerprint, fingerprint_for, is_pending, remember_command, results_for,
    validate_input_id,
};
use super::window::{
    prune_terminal_records, reserved_delivery_slots, retain_terminal, DELIVERY_JOURNAL_CAPACITY,
};
use super::{MailboxState, UserInputMailbox, UserInputQueueError};

pub(super) const PENDING_CAPACITY: usize = 32;

/// dispatch 等待空闲会话确认移交的上限；只用于同一次调用内消除显而易见的
/// 「已接纳 vs 已移交」歧义，必须远小于用户可感知的停顿。
pub(super) const DISPATCH_HANDOFF_WINDOW: std::time::Duration =
    std::time::Duration::from_millis(50);

impl UserInputMailbox {
    pub async fn enqueue(
        &self,
        request: &EnqueueUserInputRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let fingerprint = compute_fingerprint(("enqueue", request));
        self.validate_current(
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        if let Some(receipt) = self.replay(&request.command_id, fingerprint).await? {
            return Ok(receipt);
        }
        validate_input_id(&request.input_id)?;
        let input_uuid =
            uuid::Uuid::parse_str(&request.input_id).expect("input id was validated above");
        if request.content.is_empty() {
            return Err(UserInputQueueError::EmptyContent);
        }
        let input_fingerprint =
            fingerprint_for(&request.input_id, &request.content, &request.original_draft);
        let early = {
            let mut state = self.state.lock();
            self.validate(
                &state,
                &request.session_id,
                &request.generation,
                &request.command_id,
            )?;
            if let Some(record) = state
                .records
                .iter()
                .find(|record| record.input_id == request.input_id)
            {
                if record.fingerprint != input_fingerprint {
                    return Err(UserInputQueueError::IdentityConflict);
                }
                // 已经在待发或执行中：重复 enqueue 不改写既有事实。
                let results = results_for(&state, std::slice::from_ref(&request.input_id));
                remember_command(
                    &mut state,
                    &request.command_id,
                    fingerprint,
                    results.clone(),
                    None,
                );
                Some(results)
            } else if let Some(identity) = state.terminal_identities.get(&input_uuid) {
                if identity.fingerprint != input_fingerprint {
                    return Err(UserInputQueueError::IdentityConflict);
                }
                let results = results_for(&state, std::slice::from_ref(&request.input_id));
                remember_command(
                    &mut state,
                    &request.command_id,
                    fingerprint,
                    results.clone(),
                    None,
                );
                Some(results)
            } else {
                if state
                    .records
                    .iter()
                    .filter(|record| is_pending(record.state))
                    .count()
                    + state.admissions_in_flight
                    >= PENDING_CAPACITY
                    || state.delivery_journal.len()
                        + reserved_delivery_slots(&state)
                        + state.admissions_in_flight
                        >= DELIVERY_JOURNAL_CAPACITY
                {
                    return Err(UserInputQueueError::Capacity);
                }
                state.admissions_in_flight += 1;
                None
            }
        };
        if let Some(results) = early {
            return Ok(self.receipt(results, None).await);
        }

        let message = QueuedMessage::prompt(
            peri_acp_types::session::MessageSource::UserInput,
            peri_acp_types::messages::BaseMessage::Human {
                id: MessageId::from(input_uuid),
                content: request.content.clone(),
            },
        )
        .with_original_draft(request.original_draft.clone())
        // 用户输入是必须处理的消息；「未 dispatch 不激活」由 MQ 的发送选择
        // （`send_inputs`）保证，而不是把策略降级为 passive。
        .with_policy(peri_acp_types::session::MessagePolicy::ensure_processing());
        let outcome = match self.inbox.queue().enqueue(message).await {
            Ok(outcome) => outcome,
            Err(error) => {
                let mut state = self.state.lock();
                state.admissions_in_flight = state.admissions_in_flight.saturating_sub(1);
                return Err(UserInputQueueError::Store(error.to_string()));
            }
        };

        let (sent_ready, results) = {
            let mut state = self.state.lock();
            state.admissions_in_flight = state.admissions_in_flight.saturating_sub(1);
            let sent_ready = match outcome.disposition {
                MqEnqueueDisposition::Inserted | MqEnqueueDisposition::Existing => {
                    if !state
                        .records
                        .iter()
                        .any(|record| record.input_id == request.input_id)
                    {
                        let seq = state.next_record_seq();
                        state.records.push(super::InputRecord::new(
                            request.input_id.clone(),
                            input_fingerprint,
                            seq,
                            request.content.clone(),
                            request.original_draft.clone(),
                        ));
                        state.revision += 1;
                        if state.active.is_none() || state.paused {
                            state.paused = false;
                            super::state::promote_next(&mut state);
                        }
                    }
                    if self.wake_suspended_locked(&mut state) {
                        super::state::ready_ids(&state)
                    } else {
                        Vec::new()
                    }
                }
                _ => Vec::new(),
            };
            let results = results_for(&state, std::slice::from_ref(&request.input_id));
            remember_command(
                &mut state,
                &request.command_id,
                fingerprint,
                results.clone(),
                None,
            );
            (sent_ready, results)
        };
        self.send_ready_ids(sent_ready, true).await;
        Ok(self.receipt(results, None).await)
    }

    pub async fn dispatch(
        &self,
        request: &DispatchUserInputsRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let fingerprint = compute_fingerprint(("dispatch", request));
        self.validate_current(
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        if let Some(receipt) = self.replay(&request.command_id, fingerprint).await? {
            return Ok(receipt);
        }
        let mut distinct = std::collections::HashSet::new();
        if request
            .input_ids
            .iter()
            .any(|id| validate_input_id(id).is_err() || !distinct.insert(id))
        {
            return Err(UserInputQueueError::InvalidIdentity);
        }
        let promoted = {
            let mut state = self.state.lock();
            self.validate(
                &state,
                &request.session_id,
                &request.generation,
                &request.command_id,
            )?;
            let promoted = super::state::promote_ids(&mut state, &request.input_ids);
            if !promoted.is_empty() {
                state.paused = false;
            }
            promoted
        };
        let ids: Vec<MessageId> = promoted
            .iter()
            .map(|id| MessageId::from(uuid::Uuid::parse_str(id).expect("准入已校验 UUID")))
            .collect();
        let send = if ids.is_empty() {
            None
        } else {
            match self
                .inbox
                .queue()
                .send_inputs_within(ids.clone(), DISPATCH_HANDOFF_WINDOW)
                .await
            {
                Ok(outcome) => Some(outcome),
                Err(error) => {
                    self.revert_promotion(&promoted);
                    return Err(UserInputQueueError::Store(error.to_string()));
                }
            }
        };
        let statuses: Vec<(String, MqSendStatus)> = promoted
            .iter()
            .map(|id| {
                let status = send.as_ref().and_then(|outcome| {
                    uuid::Uuid::parse_str(id)
                        .ok()
                        .and_then(|uuid| outcome.status_of(&MessageId::from(uuid)))
                });
                // 没有结论时按未知处理：不能把它当作「不存在」而删掉待发记录。
                (id.clone(), status.unwrap_or(MqSendStatus::Unknown))
            })
            .collect();
        let accepted: HashSet<MessageId> = statuses
            .iter()
            .filter(|(_, status)| {
                matches!(status, MqSendStatus::Accepted | MqSendStatus::HandedOff)
            })
            .filter_map(|(id, _)| uuid::Uuid::parse_str(id).ok().map(MessageId::from))
            .collect();
        let mut unknown = Vec::new();
        let mut cancel = None;
        let results = {
            let mut state = self.state.lock();
            if !accepted.is_empty() {
                for record in &mut state.records {
                    if promoted.contains(&record.input_id)
                        && accepted
                            .iter()
                            .any(|id| id.as_uuid().to_string() == record.input_id)
                    {
                        record.handed_off = true;
                    }
                }
                if let Some(active) = state.active.as_mut() {
                    // 用户在 Stop 后重新发送是明确继续；旧 attempt 仍须先完成收尾。
                    active.reason = super::InterruptReason::Steer;
                    cancel = active.cancel.clone();
                }
            }
            // 只有存储给出确定结论的 ID 才能离开待发投影；Unknown 保留记录等待重试。
            for (id, status) in &statuses {
                match status {
                    MqSendStatus::Accepted | MqSendStatus::HandedOff => {}
                    MqSendStatus::AlreadyInHistory | MqSendStatus::NotFound => {
                        let Some(index) = state
                            .records
                            .iter()
                            .position(|record| record.input_id == *id)
                        else {
                            continue;
                        };
                        let terminal = uuid::Uuid::parse_str(id)
                            .ok()
                            .and_then(|uuid| state.terminal_identities.get(&uuid))
                            .map(|identity| identity.state);
                        match terminal {
                            Some(terminal) => {
                                state.records[index].state = terminal;
                                state.records[index].handed_off = true;
                            }
                            None => {
                                state.records.remove(index);
                            }
                        }
                        state.revision += 1;
                    }
                    MqSendStatus::Unknown => unknown.push(id.clone()),
                }
            }
            if unknown.is_empty() {
                let results = results_for(&state, &request.input_ids);
                remember_command(
                    &mut state,
                    &request.command_id,
                    fingerprint,
                    results.clone(),
                    None,
                );
                results
            } else {
                Vec::new()
            }
        };
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        if !unknown.is_empty() {
            // 已 Accepted 的记录保持已标记；只回退无法确认的 ID，重试同 ID 是幂等的。
            self.revert_promotion(&unknown);
            return Err(UserInputQueueError::Store(format!(
                "cannot confirm whether {} input(s) were handed off; they remain queued",
                unknown.len()
            )));
        }
        Ok(self.receipt(results, None).await)
    }

    pub async fn take_back(
        &self,
        request: &TakeBackUserInputRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let fingerprint = compute_fingerprint(("takeback", request));
        self.validate_current(
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        if let Some(receipt) = self.replay(&request.command_id, fingerprint).await? {
            return Ok(receipt);
        }
        validate_input_id(&request.input_id)?;
        let queued = {
            let state = self.state.lock();
            self.validate(
                &state,
                &request.session_id,
                &request.generation,
                &request.command_id,
            )?;
            state.records.iter().any(|record| {
                record.input_id == request.input_id && record.state == UserInputState::Queued
            })
        };
        if !queued {
            let results = {
                let mut state = self.state.lock();
                let results = results_for(&state, std::slice::from_ref(&request.input_id));
                remember_command(
                    &mut state,
                    &request.command_id,
                    fingerprint,
                    results.clone(),
                    None,
                );
                results
            };
            return Ok(self.receipt(results, None).await);
        }
        let id = MessageId::from(
            uuid::Uuid::parse_str(&request.input_id).expect("input id was validated above"),
        );
        let cancel_outcome = self
            .inbox
            .queue()
            .cancel_user_input(&id)
            .await
            .map_err(|error| UserInputQueueError::Store(error.to_string()))?;
        let (results, taken_back) = {
            let mut state = self.state.lock();
            let mut taken_back = None;
            match cancel_outcome {
                MqCancelOutcome::Cancelled(entry) => {
                    if let Some(record) = state
                        .records
                        .iter_mut()
                        .find(|record| record.input_id == request.input_id)
                    {
                        record.state = UserInputState::Withdrawn;
                        // 未交付即撤回：释放 enqueue 时预留的日志槽位。
                        record.delivery_reserved = false;
                    }
                    state.revision += 1;
                    retain_terminal(&mut state, &request.input_id);
                    prune_terminal_records(&mut state);
                    taken_back = super::state::user_input_from_entry(&entry);
                }
                MqCancelOutcome::AlreadyHandedOff => {
                    // 已移交历史：不能回删冒充取消；保留运行期记录等待 Receive
                    // 的 Claimed/Delivered 收尾，结果如实反映已移交。
                    if let Some(record) = state
                        .records
                        .iter_mut()
                        .find(|record| record.input_id == request.input_id)
                    {
                        record.handed_off = true;
                        if record.state == UserInputState::Queued {
                            record.state = UserInputState::Claimed;
                            state.revision += 1;
                        }
                    }
                }
                MqCancelOutcome::NotFound => {
                    // 幂等：当前 MQ 与历史都没有这个 ID，不能伪造撤回回执。
                    state.records.retain(|record| {
                        !(record.input_id == request.input_id
                            && matches!(
                                record.state,
                                UserInputState::Queued | UserInputState::Dispatching
                            ))
                    });
                    state.revision += 1;
                }
            }
            let results = results_for(&state, std::slice::from_ref(&request.input_id));
            remember_command(
                &mut state,
                &request.command_id,
                fingerprint,
                results.clone(),
                taken_back.clone(),
            );
            (results, taken_back)
        };
        Ok(self.receipt(results, taken_back).await)
    }

    /// 发送一组 ready 记录；`clear_ready` 为 true 时同时消费运行期 ready 队列。
    pub(super) async fn send_ready_ids(&self, ids: Vec<String>, clear_ready: bool) {
        let message_ids: Vec<MessageId> = ids
            .iter()
            .filter_map(|id| uuid::Uuid::parse_str(id).ok().map(MessageId::from))
            .collect();
        if message_ids.is_empty() {
            return;
        }
        match self.inbox.queue().send_inputs(message_ids).await {
            Ok(outcome) => {
                let mut state = self.state.lock();
                let mut unknown = Vec::new();
                for record in &mut state.records {
                    if !ids.contains(&record.input_id) {
                        continue;
                    }
                    let status = uuid::Uuid::parse_str(&record.input_id)
                        .ok()
                        .and_then(|uuid| outcome.status_of(&MessageId::from(uuid)));
                    match status {
                        // Unknown 不是结论：不宣称已移交，留在 ready 里等下一次重试。
                        Some(MqSendStatus::Unknown) | None => unknown.push(record.input_id.clone()),
                        Some(_) => record.handed_off = true,
                    }
                }
                if clear_ready {
                    state
                        .ready
                        .retain(|id| !ids.contains(id) || unknown.contains(id));
                }
            }
            Err(error) => {
                tracing::warn!(%error, "user input send selection failed; input stays queued");
            }
        }
    }

    fn revert_promotion(&self, ids: &[String]) {
        let mut state = self.state.lock();
        for record in &mut state.records {
            if record.state == UserInputState::Dispatching && ids.contains(&record.input_id) {
                record.state = UserInputState::Queued;
                record.handed_off = false;
            }
        }
        state.ready.retain(|id| !ids.contains(id));
        state.revision += 1;
    }

    fn validate(
        &self,
        state: &MailboxState,
        session: &str,
        generation: &str,
        command: &str,
    ) -> Result<(), UserInputQueueError> {
        if session != self.session_id || generation != self.generation {
            return Err(UserInputQueueError::StaleSession);
        }
        if !state.valid {
            return Err(UserInputQueueError::Closed);
        }
        if command.is_empty() {
            return Err(UserInputQueueError::InvalidIdentity);
        }
        Ok(())
    }

    fn validate_current(
        &self,
        session: &str,
        generation: &str,
        command: &str,
    ) -> Result<(), UserInputQueueError> {
        let state = self.state.lock();
        self.validate(&state, session, generation, command)
    }

    async fn replay(
        &self,
        command: &str,
        fingerprint: u64,
    ) -> Result<Option<UserInputQueueReceipt>, UserInputQueueError> {
        let replayed = {
            let state = self.state.lock();
            let Some(receipt) = state.commands.get(command) else {
                return Ok(None);
            };
            if receipt.fingerprint != fingerprint {
                return Err(UserInputQueueError::IdentityConflict);
            }
            Some((receipt.results.clone(), receipt.taken_back.clone()))
        };
        match replayed {
            Some((results, taken_back)) => Ok(Some(self.receipt(results, taken_back).await)),
            None => Ok(None),
        }
    }

    pub(super) async fn receipt(
        &self,
        results: Vec<UserInputItemResult>,
        taken_back: Option<UserInput>,
    ) -> UserInputQueueReceipt {
        let snapshot = self.snapshot().await;
        self.publish(snapshot.clone());
        UserInputQueueReceipt {
            snapshot,
            results,
            taken_back,
        }
    }
}
