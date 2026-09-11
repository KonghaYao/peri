//! 用户待发区与执行准入的会话级 owner，普通待办不会参与 MQ 唤醒。

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

use parking_lot::Mutex;
use peri_acp_types::{
    event_v2::StateEvent,
    identity::AgentId,
    messages::{BaseMessage, MessageContent, MessageId},
    session::{
        DispatchUserInputsRequest, EnqueueUserInputRequest, MessageSource, QueuedMessage,
        SessionInbox, TakeBackUserInputRequest, TurnId, UserInput, UserInputItemResult,
        UserInputQueueItem, UserInputQueueReceipt, UserInputQueueSnapshot, UserInputState,
    },
};
use tokio_util::sync::CancellationToken;

const PENDING_CAPACITY: usize = 32;

/// 预留的单次执行身份；失效 ticket 不能启动或结算后来的一次执行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserInputRunTicket {
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserInputAttemptOutcome {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UserInputQueueError {
    #[error("User input queue belongs to a different session generation")]
    StaleSession,
    #[error("User input queue is closed; reload the session")]
    Closed,
    #[error("Input and command identities must be valid and non-empty")]
    InvalidIdentity,
    #[error("Input content must not be empty")]
    EmptyContent,
    #[error("User input queue is full")]
    Capacity,
    #[error("Identity was already used for a different operation")]
    IdentityConflict,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InterruptReason {
    None,
    Steer,
    Stop,
}

struct ActiveRun {
    ticket: UserInputRunTicket,
    cancel: Option<CancellationToken>,
    reason: InterruptReason,
    outcome: Option<UserInputAttemptOutcome>,
    managed: bool,
}

struct InputRecord {
    input: UserInput,
    fingerprint: u64,
    state: UserInputState,
    handed_off: bool,
}

struct CommandReceipt {
    fingerprint: u64,
    results: Vec<UserInputItemResult>,
    taken_back: Option<UserInput>,
}

struct MailboxState {
    revision: u64,
    records: Vec<InputRecord>,
    ready: Vec<String>,
    commands: HashMap<String, CommandReceipt>,
    active: Option<ActiveRun>,
    paused: bool,
    suspended: bool,
    valid: bool,
}

/// 由宿主持有跨 turn 实例，Agent 独占队列状态、取消原因和执行准入判定。
pub struct UserInputMailbox {
    session_id: String,
    generation: String,
    inbox: Arc<SessionInbox>,
    state: Mutex<MailboxState>,
    emit: Arc<dyn Fn(StateEvent) + Send + Sync>,
    control_turn: TurnId,
    control_agent: AgentId,
}

impl UserInputMailbox {
    pub fn new(
        session_id: String,
        inbox: Arc<SessionInbox>,
        emit: Arc<dyn Fn(StateEvent) + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self {
            session_id,
            generation: uuid::Uuid::now_v7().to_string(),
            inbox,
            state: Mutex::new(MailboxState {
                revision: 0,
                records: Vec::new(),
                ready: Vec::new(),
                commands: HashMap::new(),
                active: None,
                paused: false,
                suspended: false,
                valid: true,
            }),
            emit,
            control_turn: TurnId::new(),
            control_agent: AgentId::new(),
        })
    }

    pub fn snapshot(&self) -> UserInputQueueSnapshot {
        self.snapshot_locked(&self.state.lock())
    }

    pub fn generation(&self) -> &str {
        &self.generation
    }

    pub(crate) fn has_handed_off_inputs(&self) -> bool {
        self.state
            .lock()
            .records
            .iter()
            .any(|record| record.state == UserInputState::Dispatching && record.handed_off)
    }

    pub(crate) fn is_managed_attempt(&self) -> bool {
        self.state
            .lock()
            .active
            .as_ref()
            .is_some_and(|active| active.managed)
    }

    pub fn enqueue(
        &self,
        request: &EnqueueUserInputRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let fingerprint = compute_fingerprint(("enqueue", request));
        let mut state = self.state.lock();
        self.validate(
            &state,
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        if let Some(receipt) = self.replay(&state, &request.command_id, fingerprint)? {
            return Ok(receipt);
        }
        validate_input_id(&request.input_id)?;
        if request.content.is_empty() {
            return Err(UserInputQueueError::EmptyContent);
        }
        let input = UserInput {
            input_id: request.input_id.clone(),
            content: request.content.clone(),
            original_draft: request.original_draft.clone(),
        };
        let input_fingerprint = compute_fingerprint(&input);
        if let Some(record) = state
            .records
            .iter()
            .find(|record| record.input.input_id == input.input_id)
        {
            if record.fingerprint != input_fingerprint {
                return Err(UserInputQueueError::IdentityConflict);
            }
        } else {
            if state
                .records
                .iter()
                .filter(|record| is_pending(record.state))
                .count()
                >= PENDING_CAPACITY
            {
                return Err(UserInputQueueError::Capacity);
            }
            state.records.push(InputRecord {
                input,
                fingerprint: input_fingerprint,
                state: UserInputState::Queued,
                handed_off: false,
            });
            state.revision += 1;
            // 挂起标记可能晚于取消抵达；只有仍可运行的同一 attempt 可以原地唤醒。
            let can_wake_suspended = state.suspended
                && state.active.as_ref().is_some_and(|active| {
                    active.reason == InterruptReason::None
                        && active
                            .cancel
                            .as_ref()
                            .is_some_and(|cancel| !cancel.is_cancelled())
                });
            if state.active.is_none() || state.paused {
                state.paused = false;
                promote_all(&mut state);
                if can_wake_suspended {
                    self.handoff_locked(&mut state);
                }
            } else if can_wake_suspended {
                promote_ids(&mut state, std::slice::from_ref(&request.input_id));
                self.handoff_locked(&mut state);
            }
        }
        let results = results_for(&state, std::slice::from_ref(&request.input_id));
        let receipt = self.remember(&mut state, &request.command_id, fingerprint, results, None);
        drop(state);
        self.publish(receipt.snapshot.clone());
        Ok(receipt)
    }

    pub fn dispatch(
        &self,
        request: &DispatchUserInputsRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let fingerprint = compute_fingerprint(("dispatch", request));
        let mut state = self.state.lock();
        self.validate(
            &state,
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        if let Some(receipt) = self.replay(&state, &request.command_id, fingerprint)? {
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
        let accepted = promote_ids(&mut state, &request.input_ids);
        let cancel = if accepted {
            state.paused = false;
            state.active.as_mut().and_then(|active| {
                // 用户在 Stop 后重新发送是明确继续；旧 attempt 仍须先完成收尾。
                active.reason = InterruptReason::Steer;
                active.cancel.clone()
            })
        } else {
            None
        };
        let results = results_for(&state, &request.input_ids);
        let receipt = self.remember(&mut state, &request.command_id, fingerprint, results, None);
        drop(state);
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        self.publish(receipt.snapshot.clone());
        Ok(receipt)
    }

    pub fn take_back(
        &self,
        request: &TakeBackUserInputRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        let fingerprint = compute_fingerprint(("takeback", request));
        let mut state = self.state.lock();
        self.validate(
            &state,
            &request.session_id,
            &request.generation,
            &request.command_id,
        )?;
        if let Some(receipt) = self.replay(&state, &request.command_id, fingerprint)? {
            return Ok(receipt);
        }
        validate_input_id(&request.input_id)?;
        let taken_back = state
            .records
            .iter_mut()
            .find(|record| {
                record.input.input_id == request.input_id && record.state == UserInputState::Queued
            })
            .map(|record| {
                record.state = UserInputState::Withdrawn;
                let input = record.input.clone();
                discard_payload(record);
                input
            });
        if taken_back.is_some() {
            state.revision += 1;
        }
        let results = results_for(&state, std::slice::from_ref(&request.input_id));
        let receipt = self.remember(
            &mut state,
            &request.command_id,
            fingerprint,
            results,
            taken_back,
        );
        drop(state);
        self.publish(receipt.snapshot.clone());
        Ok(receipt)
    }

    /// 只预留一次执行，不交接消息；宿主取得既有 prompt lock 后再 attach。
    pub fn reserve_run(&self) -> Option<UserInputRunTicket> {
        let mut state = self.state.lock();
        if !state.valid || state.paused || state.active.is_some() || state.ready.is_empty() {
            return None;
        }
        let ticket = new_ticket();
        state.active = Some(ActiveRun {
            ticket: ticket.clone(),
            cancel: None,
            reason: InterruptReason::None,
            outcome: None,
            managed: true,
        });
        Some(ticket)
    }

    /// 使用真实执行 token 绑定预留身份；交接与 Stop 在 owner 内顺序裁决。
    pub fn attach_attempt(&self, ticket: &UserInputRunTicket, cancel: CancellationToken) -> bool {
        let mut state = self.state.lock();
        if !state.valid || state.paused {
            return false;
        }
        let Some(active) = state
            .active
            .as_mut()
            .filter(|active| active.ticket == *ticket && active.cancel.is_none())
        else {
            return false;
        };
        active.cancel = Some(cancel);
        // 预留期间的 dispatch 还没有真正中断一个执行。
        active.reason = InterruptReason::None;
        self.handoff_locked(&mut state);
        state.revision += 1;
        let snapshot = self.snapshot_locked(&state);
        drop(state);
        self.publish(snapshot);
        true
    }

    /// 旧 prompt / 后台 continuation 也注册真实执行，以统一判断运行中准入。
    pub fn attach_external_attempt(
        &self,
        cancel: CancellationToken,
        resume_pending: bool,
    ) -> Option<UserInputRunTicket> {
        let mut state = self.state.lock();
        if !state.valid || state.active.is_some() {
            return None;
        }
        let ticket = new_ticket();
        state.active = Some(ActiveRun {
            ticket: ticket.clone(),
            cancel: Some(cancel),
            reason: InterruptReason::None,
            outcome: None,
            managed: false,
        });
        state.revision += 1;
        if resume_pending {
            state.paused = false;
            promote_all(&mut state);
            self.handoff_locked(&mut state);
        }
        let snapshot = self.snapshot_locked(&state);
        drop(state);
        self.publish(snapshot);
        Some(ticket)
    }

    pub(crate) fn active_run_ticket(&self) -> Option<UserInputRunTicket> {
        self.state
            .lock()
            .active
            .as_ref()
            .map(|active| active.ticket.clone())
    }

    /// 宿主须可靠投递此 Agent 事件后才开始执行，确保客户端 HITL lease 已存在。
    pub fn run_started_event(&self, ticket: &UserInputRunTicket) -> Option<StateEvent> {
        let state = self.state.lock();
        if !state.valid
            || !state
                .active
                .as_ref()
                .is_some_and(|active| active.ticket == *ticket && active.cancel.is_some())
        {
            return None;
        }
        Some(StateEvent::UserInputRunStarted {
            turn_id: self.control_turn,
            agent_id: self.control_agent,
            generation: self.generation.clone(),
            request_id: ticket.id.clone(),
        })
    }

    /// Agent 在 transcript flush 和 forwarder drain 后记录权威终态。
    pub(crate) fn record_attempt_outcome(
        &self,
        ticket: &UserInputRunTicket,
        outcome: UserInputAttemptOutcome,
    ) {
        let mut state = self.state.lock();
        if let Some(active) = state
            .active
            .as_mut()
            .filter(|active| active.ticket == *ticket)
        {
            active.outcome = Some(outcome);
        }
    }

    pub(crate) fn is_steer_interruption(&self, ticket: &UserInputRunTicket) -> bool {
        self.state.lock().active.as_ref().is_some_and(|active| {
            active.ticket == *ticket && active.reason == InterruptReason::Steer
        })
    }

    /// fallback 只处理尚未进入 Agent 的早退；正常终态优先采用 Agent 的记录。
    pub fn finish_attempt(&self, ticket: &UserInputRunTicket, fallback: UserInputAttemptOutcome) {
        let mut state = self.state.lock();
        if !state
            .active
            .as_ref()
            .is_some_and(|active| active.ticket == *ticket)
        {
            return;
        }
        let active = state.active.take().expect("已核对执行身份");
        state.revision += 1;
        state.suspended = false;
        let outcome = active.outcome.unwrap_or(fallback);
        if !state.valid {
            return;
        }
        if outcome == UserInputAttemptOutcome::Failed
            || (outcome == UserInputAttemptOutcome::Interrupted
                && active.reason == InterruptReason::None)
        {
            state.paused = true;
            self.reclaim_locked(&mut state);
        } else if state.paused {
            self.reclaim_locked(&mut state);
        } else if active.reason == InterruptReason::None
            && outcome == UserInputAttemptOutcome::Completed
        {
            // 已接受的立即发送须优先完成本次工作的延续，再释放普通待办。
            if state.ready.is_empty() {
                promote_all(&mut state);
            }
        }
        let snapshot = self.snapshot_locked(&state);
        drop(state);
        self.publish(snapshot);
    }

    /// 宿主任务在开始或执行途中被丢弃时，保留确认尚未被 Receive 领取的输入。
    pub fn fail_reserved(&self, ticket: &UserInputRunTicket) {
        let cancel = self
            .state
            .lock()
            .active
            .as_ref()
            .filter(|active| active.ticket == *ticket)
            .and_then(|active| active.cancel.clone());
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        self.finish_attempt(ticket, UserInputAttemptOutcome::Failed);
    }

    pub fn stop(&self) {
        self.stop_matching(None);
    }

    /// 携带服务端执行身份的 Stop；迟到请求不得取消后来的执行。
    pub fn stop_attempt(&self, request_id: &str, generation: &str) -> bool {
        self.stop_matching(Some((request_id, generation)))
    }

    fn stop_matching(&self, target: Option<(&str, &str)>) -> bool {
        let mut state = self.state.lock();
        if let Some((request_id, generation)) = target {
            if generation != self.generation
                || !state.valid
                || !state.active.as_ref().is_some_and(|active| {
                    active.managed && active.cancel.is_some() && active.ticket.id == request_id
                })
            {
                return false;
            }
        }
        state.paused = true;
        state.suspended = false;
        let cancel = state.active.as_mut().and_then(|active| {
            active.reason = InterruptReason::Stop;
            active.cancel.clone()
        });
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.cancel.is_none())
        {
            state.active = None;
        }
        self.reclaim_locked(&mut state);
        let snapshot = self.snapshot_locked(&state);
        drop(state);
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        self.publish(snapshot);
        true
    }

    /// 会话关闭或持久化不确定时冻结当前 owner，旧 generation 永不重新准入。
    pub fn invalidate(&self) {
        self.state.lock().valid = false;
        self.stop();
    }

    pub(crate) fn set_suspended(&self, suspended: bool) {
        self.state.lock().suspended = suspended;
    }

    /// Receive 已从 MQ 独占取走这些 ID；任何 Stop 都不得再将它们恢复 queued。
    pub(crate) fn mark_claimed(&self, ids: &[MessageId]) {
        let mut state = self.state.lock();
        let mut changed = false;
        for record in &mut state.records {
            if record.state == UserInputState::Dispatching && matches_id(record, ids) {
                record.state = UserInputState::Claimed;
                changed = true;
            }
        }
        if !changed {
            return;
        }
        state.revision += 1;
        let snapshot = self.snapshot_locked(&state);
        drop(state);
        self.publish(snapshot);
    }

    /// 返回本次首次接纳的注册输入 ID，调用方在本轮 render FIFO 发聊天事件。
    pub(crate) fn mark_delivered(&self, ids: &[MessageId]) -> Vec<String> {
        let mut state = self.state.lock();
        let mut delivered = Vec::new();
        for record in &mut state.records {
            if record.state == UserInputState::Claimed && matches_id(record, ids) {
                record.state = UserInputState::Delivered;
                delivered.push(record.input.input_id.clone());
                discard_payload(record);
            }
        }
        if delivered.is_empty() {
            return delivered;
        }
        state.revision += 1;
        let snapshot = self.snapshot_locked(&state);
        drop(state);
        self.publish(snapshot);
        delivered
    }

    fn handoff_locked(&self, state: &mut MailboxState) {
        let ready = std::mem::take(&mut state.ready);
        let messages = ready
            .into_iter()
            .filter_map(|id| {
                let record = state
                    .records
                    .iter_mut()
                    .find(|record| record.input.input_id == id)?;
                if record.state != UserInputState::Dispatching || record.handed_off {
                    return None;
                }
                let id = MessageId::from(uuid::Uuid::parse_str(&id).expect("准入已校验 UUID"));
                record.handed_off = true;
                Some(QueuedMessage::prompt(
                    MessageSource::UserInput,
                    BaseMessage::Human {
                        id,
                        content: record.input.content.clone(),
                    },
                ))
            })
            .collect();
        self.inbox.handle().push_batch(messages);
    }

    fn reclaim_locked(&self, state: &mut MailboxState) {
        let ids: Vec<_> = state
            .records
            .iter()
            .filter(|record| record.state == UserInputState::Dispatching && record.handed_off)
            .filter_map(|record| {
                uuid::Uuid::parse_str(&record.input.input_id)
                    .ok()
                    .map(MessageId::from)
            })
            .collect();
        let withdrawn = self.inbox.queue().withdraw_user_inputs(&ids);
        let withdrawn_ids: Vec<_> = withdrawn
            .iter()
            .filter_map(|message| message.message().map(BaseMessage::id))
            .collect();
        let mut changed = false;
        for record in &mut state.records {
            if record.state == UserInputState::Dispatching
                && (!record.handed_off || matches_id(record, &withdrawn_ids))
            {
                record.state = UserInputState::Queued;
                record.handed_off = false;
                changed = true;
            }
        }
        state.ready.clear();
        if changed {
            state.revision += 1;
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

    fn snapshot_locked(&self, state: &MailboxState) -> UserInputQueueSnapshot {
        UserInputQueueSnapshot {
            session_id: self.session_id.clone(),
            generation: self.generation.clone(),
            revision: state.revision,
            active_request_id: state
                .active
                .as_ref()
                .filter(|active| active.managed && active.cancel.is_some())
                .map(|active| active.ticket.id.clone()),
            items: state
                .records
                .iter()
                .filter(|record| is_pending(record.state))
                .map(|record| UserInputQueueItem {
                    input_id: record.input.input_id.clone(),
                    content: record.input.content.clone(),
                    original_draft: record.input.original_draft.clone(),
                    state: record.state,
                })
                .collect(),
        }
    }

    fn remember(
        &self,
        state: &mut MailboxState,
        command: &str,
        fingerprint: u64,
        results: Vec<UserInputItemResult>,
        taken_back: Option<UserInput>,
    ) -> UserInputQueueReceipt {
        state.commands.insert(
            command.to_owned(),
            CommandReceipt {
                fingerprint,
                results: results.clone(),
                taken_back: taken_back.clone(),
            },
        );
        UserInputQueueReceipt {
            snapshot: self.snapshot_locked(state),
            results,
            taken_back,
        }
    }

    fn replay(
        &self,
        state: &MailboxState,
        command: &str,
        fingerprint: u64,
    ) -> Result<Option<UserInputQueueReceipt>, UserInputQueueError> {
        let Some(receipt) = state.commands.get(command) else {
            return Ok(None);
        };
        if receipt.fingerprint != fingerprint {
            return Err(UserInputQueueError::IdentityConflict);
        }
        Ok(Some(UserInputQueueReceipt {
            snapshot: self.snapshot_locked(state),
            results: receipt.results.clone(),
            taken_back: receipt.taken_back.clone(),
        }))
    }

    fn publish(&self, snapshot: UserInputQueueSnapshot) {
        (self.emit)(StateEvent::UserInputQueueChanged {
            turn_id: self.control_turn,
            agent_id: self.control_agent,
            snapshot,
        });
    }
}

fn new_ticket() -> UserInputRunTicket {
    UserInputRunTicket {
        id: uuid::Uuid::now_v7().to_string(),
    }
}

fn validate_input_id(id: &str) -> Result<(), UserInputQueueError> {
    let parsed = uuid::Uuid::parse_str(id).map_err(|_| UserInputQueueError::InvalidIdentity)?;
    if parsed.to_string() != id {
        return Err(UserInputQueueError::InvalidIdentity);
    }
    Ok(())
}

fn is_pending(state: UserInputState) -> bool {
    matches!(
        state,
        UserInputState::Queued | UserInputState::Dispatching | UserInputState::Claimed
    )
}

fn matches_id(record: &InputRecord, ids: &[MessageId]) -> bool {
    uuid::Uuid::parse_str(&record.input.input_id)
        .ok()
        .is_some_and(|id| ids.contains(&MessageId::from(id)))
}

fn discard_payload(record: &mut InputRecord) {
    record.input.content = MessageContent::text("");
    record.input.original_draft.clear();
}

fn promote_all(state: &mut MailboxState) {
    let ids: Vec<_> = state
        .records
        .iter()
        .filter(|record| record.state == UserInputState::Queued)
        .map(|record| record.input.input_id.clone())
        .collect();
    promote_ids(state, &ids);
}

fn promote_ids(state: &mut MailboxState, ids: &[String]) -> bool {
    let mut changed = false;
    // 单次 ALL 按服务端原顺序；后续命令追加到 ready，保持 B 后 A 的接受顺序。
    for record in &mut state.records {
        if record.state == UserInputState::Queued && ids.contains(&record.input.input_id) {
            record.state = UserInputState::Dispatching;
            record.handed_off = false;
            state.ready.push(record.input.input_id.clone());
            changed = true;
        }
    }
    if changed {
        state.revision += 1;
    }
    changed
}

fn results_for(state: &MailboxState, ids: &[String]) -> Vec<UserInputItemResult> {
    ids.iter()
        .map(|id| UserInputItemResult {
            input_id: id.clone(),
            state: state
                .records
                .iter()
                .find(|record| record.input.input_id == *id)
                .map(|record| record.state)
                .unwrap_or(UserInputState::Unknown),
        })
        .collect()
}

fn compute_fingerprint<T: serde::Serialize>(value: T) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(&value)
        .expect("队列 DTO 可序列化")
        .hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
#[path = "user_input_mailbox_test.rs"]
mod tests;
