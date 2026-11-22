//! 入队、立即发送与取回命令，以及命令回执与发送选择。

use std::collections::HashSet;

use peri_acp_types::messages::MessageId;
use peri_acp_types::session::DispatchUserInputsRequest;
use peri_acp_types::session::EnqueueUserInputRequest;
use peri_acp_types::session::MqCancelOutcome;
use peri_acp_types::session::MqEnqueueDisposition;
use peri_acp_types::session::MqEntry;
use peri_acp_types::session::MqSendStatus;
use peri_acp_types::session::PluginSelection;
use peri_acp_types::session::QueuedMessage;
use peri_acp_types::session::TakeBackUserInputRequest;
use peri_acp_types::session::UserInput;
use peri_acp_types::session::UserInputItemResult;
use peri_acp_types::session::UserInputQueueReceipt;
use peri_acp_types::session::UserInputState;

use crate::session::user_input_mailbox::window::DELIVERY_JOURNAL_CAPACITY;
use crate::session::user_input_mailbox::MailboxState;
use crate::session::user_input_mailbox::UserInputMailbox;
use crate::session::user_input_mailbox::UserInputQueueError;

pub(super) const PENDING_CAPACITY: usize = 32;

/// dispatch 等待空闲会话确认移交的上限；只用于同一次调用内消除显而易见的
/// 「已接纳 vs 已移交」歧义，必须远小于用户可感知的停顿。
pub(super) const DISPATCH_HANDOFF_WINDOW: std::time::Duration =
    std::time::Duration::from_millis(50);

/// 并发 enqueue 的既有行裁决。
///
/// 前置指纹检查与 MQ 提交之间存在窗口；存储对 `Existing` 只比较正文/原稿，
/// 选择不同的请求不能借用先到者的成功回执。
pub(super) fn existing_selection_match(
    disposition: MqEnqueueDisposition,
    entry: &MqEntry,
    requested: &[PluginSelection],
) -> Result<(), UserInputQueueError> {
    if disposition == MqEnqueueDisposition::Existing && entry.plugin_selections != requested {
        return Err(UserInputQueueError::IdentityConflict);
    }
    Ok(())
}

/// 用邮箱内已持久化的身份指纹复核等待期间出现的并发事实。
///
/// `AlreadyHandedOff` 的 `entry` 是本次请求的投影，无法据此比较选择；第一请求的
/// 记录或终态身份可能在 MQ await 期间写入邮箱，指纹包含选择与正文，可作为权威来源。
pub(super) fn ensure_stored_fingerprint(
    stored: Option<u64>,
    requested: u64,
) -> Result<(), UserInputQueueError> {
    if stored.is_some_and(|stored| stored != requested) {
        return Err(UserInputQueueError::IdentityConflict);
    }
    Ok(())
}

/// 撤回回执的插件选择来源。
///
/// 被删除的 MQ 行与名称同批写入，是并发对账下仍可读取的事实；运行期记录可能
/// 已在取消提交后、取回恢复前被剪除。旧行没有选择时才回退记录缓存。
pub(super) fn taken_back_selections(
    entry: &MqEntry,
    record_selections: Option<Vec<PluginSelection>>,
) -> Vec<PluginSelection> {
    if entry.plugin_selections.is_empty() {
        record_selections.unwrap_or_default()
    } else {
        entry.plugin_selections.clone()
    }
}

impl UserInputMailbox {
    pub async fn enqueue(
        &self,
        request: &EnqueueUserInputRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let fingerprint =
            crate::session::user_input_mailbox::state::compute_fingerprint(("enqueue", request));
        self.validate_current(
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        if let Some(receipt) = self.replay(&request.command_id, fingerprint).await? {
            return Ok(receipt);
        }
        crate::session::user_input_mailbox::state::validate_input_id(&request.input_id)?;
        let input_uuid =
            uuid::Uuid::parse_str(&request.input_id).expect("input id was validated above");
        if request.content.is_empty() {
            return Err(UserInputQueueError::EmptyContent);
        }
        let input_fingerprint = crate::session::user_input_mailbox::state::fingerprint_for(
            &request.input_id,
            &request.content,
            &request.original_draft,
            &request.plugin_selections,
        );
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
                let results = crate::session::user_input_mailbox::state::results_for(
                    &state,
                    std::slice::from_ref(&request.input_id),
                );
                crate::session::user_input_mailbox::state::remember_command(
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
                let results = crate::session::user_input_mailbox::state::results_for(
                    &state,
                    std::slice::from_ref(&request.input_id),
                );
                crate::session::user_input_mailbox::state::remember_command(
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
                    .filter(|record| {
                        crate::session::user_input_mailbox::state::is_pending(record.state)
                    })
                    .count()
                    + state.admissions_in_flight
                    >= PENDING_CAPACITY
                    || state.delivery_journal.len()
                        + crate::session::user_input_mailbox::window::reserved_delivery_slots(
                            &state,
                        )
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
        .with_plugin_selections(request.plugin_selections.clone())
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

        let (sent_ready, results, cancel_inserted, conflict) = {
            let mut state = self.state.lock();
            state.admissions_in_flight = state.admissions_in_flight.saturating_sub(1);
            // 并发 enqueue 都可能通过前置指纹检查：MQ 的 Existing 只比较正文/原稿，
            // 选择不同时存储行属于先到者，本次请求必须按身份冲突拒绝。
            let conflict = existing_selection_match(
                outcome.disposition,
                &outcome.entry,
                &request.plugin_selections,
            )
            .and_then(|()| {
                // AlreadyHandedOff 的 entry 是请求投影，选择比较必须回读邮箱权威身份。
                let stored_fingerprint = state
                    .records
                    .iter()
                    .find(|record| record.input_id == request.input_id)
                    .map(|record| record.fingerprint)
                    .or_else(|| {
                        state
                            .terminal_identities
                            .get(&input_uuid)
                            .map(|identity| identity.fingerprint)
                    });
                ensure_stored_fingerprint(stored_fingerprint, input_fingerprint)
            })
            .err();
            if let Some(error) = conflict {
                // 身份冲突发生在 MQ 提交之后：本次 Inserted 的行必须撤回，否则调用方
                // 收到失败后下一次 Receive 仍会消费它。Existing 行属于先到者，不能替
                // 对方删除。
                let cancel_inserted = outcome.disposition == MqEnqueueDisposition::Inserted;
                (Vec::new(), Vec::new(), cancel_inserted, Some(error))
            } else {
                // 撤回与 enqueue 的 MQ await 可以交错：take_back 在等待期间清理了
                // Queued 记录并写入 `Withdrawn` 身份，而本调用仍持有已提交的 MQ 行。
                // 终态身份优先，既不复活待发记录，也要取消本次插入的行。
                let terminal = state
                    .terminal_identities
                    .get(&input_uuid)
                    .map(|identity| identity.state);
                let mut cancel_inserted = false;
                let sent_ready = match outcome.disposition {
                    MqEnqueueDisposition::Inserted | MqEnqueueDisposition::Existing => {
                        if terminal.is_some() {
                            cancel_inserted = true;
                            Vec::new()
                        } else {
                            if !state
                                .records
                                .iter()
                                .any(|record| record.input_id == request.input_id)
                            {
                                let seq = state.next_record_seq();
                                state.records.push(
                                    crate::session::user_input_mailbox::InputRecord::new(
                                        request.input_id.clone(),
                                        input_fingerprint,
                                        seq,
                                        request.content.clone(),
                                        request.original_draft.clone(),
                                        request.plugin_selections.clone(),
                                    ),
                                );
                                state.revision += 1;
                                if state.active.is_none() || state.paused {
                                    state.paused = false;
                                    crate::session::user_input_mailbox::state::promote_next(
                                        &mut state,
                                    );
                                }
                            }
                            if self.wake_suspended_locked(&mut state) {
                                crate::session::user_input_mailbox::state::ready_ids(&state)
                            } else {
                                Vec::new()
                            }
                        }
                    }
                    _ => Vec::new(),
                };
                let results = crate::session::user_input_mailbox::state::results_for(
                    &state,
                    std::slice::from_ref(&request.input_id),
                );
                crate::session::user_input_mailbox::state::remember_command(
                    &mut state,
                    &request.command_id,
                    fingerprint,
                    results.clone(),
                    None,
                );
                (sent_ready, results, cancel_inserted, None)
            }
        };
        if cancel_inserted {
            let id = MessageId::from(input_uuid);
            match self.inbox.queue().cancel_user_input(&id).await {
                Ok(MqCancelOutcome::Cancelled(_) | MqCancelOutcome::NotFound) => {}
                Ok(MqCancelOutcome::AlreadyHandedOff) => {
                    // 已移交的行无法回删；dispatch 结算会按同一 identity 收敛为终态。
                    tracing::warn!(
                        input_id = %request.input_id,
                        "withdrawn input was already handed off during enqueue"
                    );
                }
                Err(error) => {
                    return Err(UserInputQueueError::Store(error.to_string()));
                }
            }
        }
        if let Some(error) = conflict {
            return Err(error);
        }
        self.send_ready_ids(sent_ready, true).await;
        Ok(self.receipt(results, None).await)
    }

    pub async fn dispatch(
        &self,
        request: &DispatchUserInputsRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let fingerprint =
            crate::session::user_input_mailbox::state::compute_fingerprint(("dispatch", request));
        self.validate_current(
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        if let Some(receipt) = self.replay(&request.command_id, fingerprint).await? {
            return Ok(receipt);
        }
        let mut distinct = std::collections::HashSet::new();
        if request.input_ids.iter().any(|id| {
            crate::session::user_input_mailbox::state::validate_input_id(id).is_err()
                || !distinct.insert(id)
        }) {
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
            let mut promoted = crate::session::user_input_mailbox::state::promote_ids(
                &mut state,
                &request.input_ids,
            );
            for record in &state.records {
                if record.state == UserInputState::Dispatching
                    && request.input_ids.contains(&record.input_id)
                    && !promoted.contains(&record.input_id)
                {
                    promoted.push(record.input_id.clone());
                }
            }
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
                // 只有实际 LLM 调用/流式期间才硬取消；工具批次、Compact、idle
                // 与收尾阶段只放行 MQ，由下一次 Receive 在同一 attempt 内摄取。
                // MQ 放行已在前一步完成：必须先移交再中断，避免漏发。
                if state.model_request_in_flight {
                    if let Some(active) = state.active.as_mut() {
                        // 用户在 Stop 后重新发送是明确继续；旧 attempt 仍须先完成收尾。
                        active.reason = crate::session::user_input_mailbox::InterruptReason::Steer;
                        cancel = active.cancel.clone();
                    }
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
                let results = crate::session::user_input_mailbox::state::results_for(
                    &state,
                    &request.input_ids,
                );
                crate::session::user_input_mailbox::state::remember_command(
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
            // 超时/未知不证明未发送；保留 Dispatching，由相同 ID 的显式重试求证。
            return Err(UserInputQueueError::Store(format!(
                "cannot confirm whether {} input(s) were handed off; they remain dispatching",
                unknown.len()
            )));
        }
        Ok(self.receipt(results, None).await)
    }

    pub async fn take_back(
        &self,
        request: &TakeBackUserInputRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let fingerprint =
            crate::session::user_input_mailbox::state::compute_fingerprint(("takeback", request));
        self.validate_current(
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        if let Some(receipt) = self.replay(&request.command_id, fingerprint).await? {
            return Ok(receipt);
        }
        crate::session::user_input_mailbox::state::validate_input_id(&request.input_id)?;
        // 先固定记录身份：等待 MQ 取消期间对账可能清掉 Queued 记录，终态仍须
        // 由这个指纹写入，否则同一 ID 会重新获得执行资格。
        let queued_fingerprint = {
            let state = self.state.lock();
            self.validate(
                &state,
                &request.session_id,
                &request.generation,
                &request.command_id,
            )?;
            state
                .records
                .iter()
                .find(|record| {
                    record.input_id == request.input_id && record.state == UserInputState::Queued
                })
                .map(|record| record.fingerprint)
        };
        let Some(input_fingerprint) = queued_fingerprint else {
            let results = {
                let mut state = self.state.lock();
                let results = crate::session::user_input_mailbox::state::results_for(
                    &state,
                    std::slice::from_ref(&request.input_id),
                );
                crate::session::user_input_mailbox::state::remember_command(
                    &mut state,
                    &request.command_id,
                    fingerprint,
                    results.clone(),
                    None,
                );
                results
            };
            return Ok(self.receipt(results, None).await);
        };
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
                    // 撤回回执必须带上原输入的选择：选择随 input_id 走。
                    // 对账可能在取消提交后、这里恢复前剪掉运行期记录，因此以
                    // 被删除的 MQ 行为准；旧行无选择时才回退记录缓存。
                    let record_selections = state
                        .records
                        .iter()
                        .find(|record| record.input_id == request.input_id)
                        .map(|record| record.plugin_selections.clone());
                    let selections = taken_back_selections(&entry, record_selections);
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
                    crate::session::user_input_mailbox::window::retain_terminal(
                        &mut state,
                        &request.input_id,
                        Some((input_fingerprint, UserInputState::Withdrawn)),
                    );
                    crate::session::user_input_mailbox::window::prune_terminal_records(&mut state);
                    taken_back = crate::session::user_input_mailbox::state::user_input_from_entry(
                        &entry, selections,
                    );
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
            let results = crate::session::user_input_mailbox::state::results_for(
                &state,
                std::slice::from_ref(&request.input_id),
            );
            crate::session::user_input_mailbox::state::remember_command(
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
