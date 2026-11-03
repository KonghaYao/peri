//! 用户待发区与执行准入的会话级 owner，普通待办在 idle 时逐条交接给 MQ。
//!
//! 待发事实（内容、原稿、是否存在）只存在 MQ；本模块的运行期记录仅保存
//! 调度状态、身份与交付槽位（`docs/design/thread-mq.md` §4/§6）。

use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::{Hash, Hasher},
    sync::Arc,
};

use parking_lot::Mutex;
use peri_acp_types::{
    event_v2::StateEvent,
    identity::AgentId,
    messages::{BaseMessage, MessageContent, MessageId},
    session::{
        DispatchUserInputsRequest, EnqueueUserInputRequest, MessagePolicy, MessageSource,
        MqCancelOutcome, MqEnqueueDisposition, MqEntry, MqFilter, QueuedMessage, SessionInbox,
        TakeBackUserInputRequest, TurnId, UserInput, UserInputItemResult, UserInputQueueItem,
        UserInputQueueReceipt, UserInputQueueSnapshot, UserInputQueueTerminalItem, UserInputState,
    },
    store::PersistedPayload,
};
use tokio_util::sync::CancellationToken;

mod window;
#[cfg(test)]
use window::TERMINAL_CAPACITY;
use window::{
    prune_terminal_records, reserved_delivery_slots, retain_terminal, PendingDeliveryNotification,
    TerminalDisplay, TerminalIdentity, DELIVERY_JOURNAL_CAPACITY,
};

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

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
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
    #[error("Thread MQ storage failed: {0}")]
    Store(String),
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

/// 运行期调度记录；内容与原稿一律回查 MQ，不在此复制。
struct InputRecord {
    input_id: String,
    fingerprint: u64,
    state: UserInputState,
    handed_off: bool,
    /// 交付日志槽位在 enqueue 时预留；交付时转为日志条目，撤回时释放。
    /// 这样 `mark_delivered` 永远不会在消息已写入 transcript 后才因容量失败。
    delivery_reserved: bool,
}

struct CommandReceipt {
    fingerprint: u64,
    results: Vec<UserInputItemResult>,
    taken_back: Option<UserInput>,
}

struct MailboxState {
    revision: u64,
    records: Vec<InputRecord>,
    // generation 内的终态身份不可淘汰，否则同 ID 会重新获得执行资格。
    //
    // 生命周期：只随 generation 结束（会话重载/进程重建）回收。客户端可以在
    // 同一 generation 内无限期重提任一旧 ID，因此不存在不破坏幂等的回收时点；
    // 有界 LRU 会让被淘汰的 ID 重新可执行，不能采用。
    //
    // 空间：input_id 已是规范 UUID，这里用 16B UUID 作为紧凑身份键（而不是
    // 36B 字符串 + 堆分配）。每条记录约 16B key + 8B fingerprint + 1B state，
    // 加 HashMap 开销约 40-56B；100 万条输入约 40-56 MiB。这是精确幂等的代价。
    terminal_identities: HashMap<uuid::Uuid, TerminalIdentity>,
    terminal_display: Vec<TerminalDisplay>,
    ready: Vec<String>,
    commands: HashMap<String, CommandReceipt>,
    active: Option<ActiveRun>,
    /// 正在等待 MQ 提交的接纳数；容量判定必须把这段异步窗口计入，
    /// 否则并发 enqueue 可以同时通过检查（MQ 才是事实源，不能先写记录）。
    admissions_in_flight: usize,
    paused: bool,
    suspended: bool,
    valid: bool,
    delivered_notifications: HashSet<String>,
    delivery_journal: VecDeque<PendingDeliveryNotification>,
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

/// 组装快照所需的运行期切片；获取后必须解锁再查询 MQ。
struct SnapshotSeed {
    revision: u64,
    active_request_id: Option<String>,
    terminal_items: Vec<UserInputQueueTerminalItem>,
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
                terminal_identities: HashMap::new(),
                terminal_display: Vec::new(),
                ready: Vec::new(),
                commands: HashMap::new(),
                active: None,
                admissions_in_flight: 0,
                paused: false,
                suspended: false,
                valid: true,
                delivered_notifications: HashSet::new(),
                delivery_journal: VecDeque::new(),
            }),
            emit,
            control_turn: TurnId::new(),
            control_agent: AgentId::new(),
        })
    }

    /// 从 MQ 恢复尚未移交的用户输入；不 promote、不发送、不启动执行。
    ///
    /// ACP 在会话加载/构造后调用。重复调用按 ID 合并，保留既有运行期状态。
    pub async fn hydrate(&self) -> Result<(), UserInputQueueError> {
        let entries = self.query_user_inputs().await?;
        let mut state = self.state.lock();
        let mut changed = false;
        for entry in entries {
            let Some((content, original_draft)) = user_input_payload(&entry) else {
                tracing::warn!(
                    input_id = %entry.id.as_uuid(),
                    "MQ user input row has a non-human payload; skipped during hydrate"
                );
                continue;
            };
            let input_id = entry.id.as_uuid().to_string();
            if state
                .records
                .iter()
                .any(|record| record.input_id == input_id)
                || state.terminal_identities.contains_key(&entry.id.as_uuid())
            {
                continue;
            }
            state.records.push(InputRecord {
                fingerprint: fingerprint_for(&input_id, &content, &original_draft),
                input_id,
                state: UserInputState::Queued,
                handed_off: false,
                delivery_reserved: true,
            });
            changed = true;
        }
        if changed {
            state.revision += 1;
        }
        Ok(())
    }

    pub async fn snapshot(&self) -> UserInputQueueSnapshot {
        let entries = self.query_user_inputs().await;
        let mut state = self.state.lock();
        let (contents, known) = match &entries {
            Ok(entries) => {
                let mut contents = HashMap::with_capacity(entries.len());
                for entry in entries {
                    if let Some((content, original_draft)) = user_input_payload(entry) {
                        contents.insert(entry.id.as_uuid().to_string(), (content, original_draft));
                    }
                }
                (contents, true)
            }
            Err(error) => {
                tracing::warn!(%error, "user input snapshot: MQ query failed; content unavailable");
                (HashMap::new(), false)
            }
        };
        if known {
            // MQ 行消失说明已被 Receive 消费或取消。只清理尚未发送的
            // Queued；Dispatching/Claimed 仍可能处于交付确认窗口，必须保留。
            let before = state.records.len();
            state.records.retain(|record| {
                record.state != UserInputState::Queued || contents.contains_key(&record.input_id)
            });
            if state.records.len() != before {
                state.revision += 1;
            }
        }
        let items = state
            .records
            .iter()
            .filter(|record| is_pending(record.state))
            .filter_map(|record| {
                let (content, original_draft) = contents.get(&record.input_id)?.clone();
                Some(UserInputQueueItem {
                    input_id: record.input_id.clone(),
                    content,
                    original_draft,
                    state: record.state,
                })
            })
            .collect();
        let seed = snapshot_seed(&state);
        drop(state);
        assemble_snapshot(&self.session_id, &self.generation, seed, items)
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
            MessageSource::UserInput,
            BaseMessage::Human {
                id: MessageId::from(input_uuid),
                content: request.content.clone(),
            },
        )
        .with_original_draft(request.original_draft.clone())
        // 用户输入是必须处理的消息；「未 dispatch 不激活」由 MQ 的发送选择
        // （`send_inputs`）保证，而不是把策略降级为 passive。
        .with_policy(MessagePolicy::ensure_processing());
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
                        state.records.push(InputRecord {
                            input_id: request.input_id.clone(),
                            fingerprint: input_fingerprint,
                            state: UserInputState::Queued,
                            handed_off: false,
                            delivery_reserved: true,
                        });
                        state.revision += 1;
                        if state.active.is_none() || state.paused {
                            state.paused = false;
                            promote_next(&mut state);
                        }
                    }
                    if self.wake_suspended_locked(&mut state) {
                        ready_ids(&state)
                    } else {
                        Vec::new()
                    }
                }
                MqEnqueueDisposition::AlreadyHandedOff => Vec::new(),
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
            let promoted = promote_ids(&mut state, &request.input_ids);
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
            match self.inbox.queue().send_inputs(ids.clone()).await {
                Ok(outcome) => Some(outcome),
                Err(error) => {
                    self.revert_promotion(&promoted);
                    return Err(UserInputQueueError::Store(error.to_string()));
                }
            }
        };
        let accepted: HashSet<MessageId> = send
            .as_ref()
            .map(|outcome| outcome.accepted.iter().map(|entry| entry.id).collect())
            .unwrap_or_default();
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
                    active.reason = InterruptReason::Steer;
                    cancel = active.cancel.clone();
                }
            }
            // missing 表示已移交或不存在：不能继续显示为 Dispatching。
            let missing: Vec<&String> = promoted
                .iter()
                .filter(|id| {
                    !send.as_ref().is_some_and(|outcome| {
                        outcome
                            .accepted
                            .iter()
                            .any(|e| e.id.as_uuid().to_string() == **id)
                    })
                })
                .collect();
            for id in missing {
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
            let results = results_for(&state, &request.input_ids);
            remember_command(
                &mut state,
                &request.command_id,
                fingerprint,
                results.clone(),
                None,
            );
            results
        };
        if let Some(cancel) = cancel {
            cancel.cancel();
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
                    taken_back = user_input_from_entry(&entry);
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
    pub async fn attach_attempt(
        &self,
        ticket: &UserInputRunTicket,
        cancel: CancellationToken,
    ) -> bool {
        let ready = {
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
            state.revision += 1;
            ready_ids(&state)
        };
        self.send_ready_ids(ready, true).await;
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
        true
    }

    /// 旧 prompt / 后台 continuation 也注册真实执行，以统一判断运行中准入。
    pub async fn attach_external_attempt(
        &self,
        cancel: CancellationToken,
        resume_pending: bool,
    ) -> Option<UserInputRunTicket> {
        let (ticket, ready) = {
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
                promote_next(&mut state);
            }
            (ticket, ready_ids(&state))
        };
        self.send_ready_ids(ready, true).await;
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
        Some(ticket)
    }

    /// 当前预留的执行身份。
    ///
    /// 宿主在 attach 之前绑定取消令牌、或核对在途 ticket 时使用；只读快照，
    /// 不改变预留状态。
    pub fn active_run_ticket(&self) -> Option<UserInputRunTicket> {
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

    /// 当前 attempt 是否被用户暂存区发送打断（steer 而非 Stop）。
    ///
    /// 必须在 `finish_attempt` 之前判定：收尾会移除 active 记录，之后恒为 false。
    pub fn is_steer_interruption(&self, ticket: &UserInputRunTicket) -> bool {
        self.state.lock().active.as_ref().is_some_and(|active| {
            active.ticket == *ticket && active.reason == InterruptReason::Steer
        })
    }

    /// 本 ticket 是否仍持有执行准入；宿主据此判断一次尝试是否已结算。
    pub fn is_active_ticket(&self, ticket: &UserInputRunTicket) -> bool {
        self.state
            .lock()
            .active
            .as_ref()
            .is_some_and(|active| active.ticket == *ticket)
    }

    /// fallback 只处理尚未进入 Agent 的早退；正常终态优先采用 Agent 的记录。
    pub async fn finish_attempt(
        &self,
        ticket: &UserInputRunTicket,
        fallback: UserInputAttemptOutcome,
    ) {
        let (reclaim, promote) = {
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
            let reclaim = outcome == UserInputAttemptOutcome::Failed
                || (outcome == UserInputAttemptOutcome::Interrupted
                    && active.reason == InterruptReason::None)
                || state.paused;
            let promote = !reclaim
                && active.reason == InterruptReason::None
                && outcome == UserInputAttemptOutcome::Completed;
            if reclaim {
                state.paused = true;
            }
            (reclaim, promote)
        };
        if reclaim {
            self.reclaim().await;
        } else if promote {
            // 已接受的立即发送优先；普通待办每次只释放队首一条。
            // 这里只做运行期 promotion；真实 `send_inputs` 留给下一次 attach。
            let mut state = self.state.lock();
            promote_next(&mut state);
        }
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
    }

    /// 宿主任务在开始或执行途中被丢弃时，保留确认尚未被 Receive 领取的输入。
    ///
    /// 只结算仍属于本 ticket 的预留：该预留若已被后续执行接管，再复位会把
    /// 下一条输入一起暂停。Drop 路径不能 await，只能做保守的运行期复位；
    /// MQ 行仍在，下一次 `hydrate`/`reclaim` 会按持久事实核对。
    pub fn fail_reserved(&self, ticket: &UserInputRunTicket) {
        let (cancel, reset_ids) = {
            let mut state = self.state.lock();
            let cancel = {
                let Some(active) = state
                    .active
                    .as_ref()
                    .filter(|active| active.ticket == *ticket)
                else {
                    return;
                };
                active.cancel.clone()
            };
            state.active = None;
            state.revision += 1;
            state.paused = true;
            state.suspended = false;
            for record in &mut state.records {
                if record.state == UserInputState::Dispatching {
                    record.state = UserInputState::Queued;
                    record.handed_off = false;
                }
            }
            state.ready.clear();
            let reset_ids: Vec<MessageId> = state
                .records
                .iter()
                .filter(|record| record.state == UserInputState::Queued && !record.handed_off)
                .filter_map(|record| {
                    uuid::Uuid::parse_str(&record.input_id)
                        .ok()
                        .map(MessageId::from)
                })
                .collect();
            (cancel, reset_ids)
        };
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        self.inbox.queue().withdraw_send_inputs(&reset_ids);
    }

    pub async fn stop(&self) {
        self.stop_matching(None).await;
    }

    /// 携带服务端执行身份的 Stop；迟到请求不得取消后来的执行。
    pub async fn stop_attempt(&self, request_id: &str, generation: &str) -> bool {
        self.stop_matching(Some((request_id, generation))).await
    }

    async fn stop_matching(&self, target: Option<(&str, &str)>) -> bool {
        let cancel = {
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
            cancel
        };
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        self.reclaim().await;
        true
    }

    /// 会话关闭或持久化不确定时冻结当前 owner，旧 generation 永不重新准入。
    ///
    /// 关闭路径不能再查询 MQ；MQ 行留给重载后的 `hydrate`，这里只失效运行期状态。
    pub fn invalidate(&self) {
        let mut state = self.state.lock();
        if !state.valid {
            return;
        }
        state.valid = false;
        state.paused = true;
        state.suspended = false;
        state.active = None;
        state.ready.clear();
        for record in &mut state.records {
            if record.state == UserInputState::Dispatching {
                record.state = UserInputState::Queued;
                record.handed_off = false;
            }
        }
        state.revision += 1;
    }

    /// idle 边界与 enqueue 共用同一把锁，覆盖输入在挂起前后到达的两种顺序。
    pub(crate) async fn enter_idle(&self) {
        let ready = {
            let mut state = self.state.lock();
            state.suspended = true;
            match self.wake_suspended_locked(&mut state) {
                true => ready_ids(&state),
                false => Vec::new(),
            }
        };
        if ready.is_empty() {
            return;
        }
        self.send_ready_ids(ready, true).await;
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
    }

    pub(crate) fn leave_idle(&self) {
        self.state.lock().suspended = false;
    }

    fn wake_suspended_locked(&self, state: &mut MailboxState) -> bool {
        // 迟到的 idle 不能恢复 Stop、立即发送收尾或已经取消的旧 attempt。
        if !state.valid
            || state.paused
            || !state.suspended
            || !state.active.as_ref().is_some_and(|active| {
                active.reason == InterruptReason::None
                    && active
                        .cancel
                        .as_ref()
                        .is_some_and(|cancel| !cancel.is_cancelled())
            })
        {
            return false;
        }
        // ready 可能在 enqueue 与 idle 之间已经完成 promotion；不能因未再次
        // promote 而跳过交接，否则输入会停在 ready 且当前执行收不到。
        if state.ready.is_empty() && !promote_next(state) {
            return false;
        }
        // 交接即占用本次 idle，后续 enqueue 不能在 Receive 开始前追加第二条。
        state.suspended = false;
        true
    }

    /// Receive 已从 MQ 独占取走这些 ID；任何 Stop 都不得再将它们恢复 queued。
    pub(crate) async fn mark_claimed(&self, ids: &[MessageId]) {
        let changed = {
            let mut state = self.state.lock();
            let mut changed = false;
            for record in &mut state.records {
                if record.state == UserInputState::Dispatching && matches_id(record, ids) {
                    record.state = UserInputState::Claimed;
                    changed = true;
                }
            }
            if changed {
                state.revision += 1;
            }
            changed
        };
        if !changed {
            return;
        }
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
    }

    /// 返回本次首次接纳的注册输入 ID，调用方在本轮 render FIFO 发聊天事件。
    ///
    /// 日志槽位在 enqueue 时已预留，因此这里不会因容量在 transcript 写入后
    /// 失败；预留随日志条目转移，泵确认后才释放。
    pub(crate) async fn mark_delivered(
        &self,
        inputs: &[(MessageId, MessageContent)],
    ) -> Vec<String> {
        self.mark_delivery_notifications(inputs).await
    }

    /// 事件泵的补发来源：按 Receive 实际交付顺序返回尚未推送的确认。
    ///
    /// 调用方必须先推送、再 [`Self::ack_delivery_notification`]；本方法只复制，
    /// 不持有锁等待 UI。进程内日志不持久化，重启后不承诺恢复展示。
    pub(crate) fn pending_delivery_notifications(&self) -> Vec<PendingDeliveryNotification> {
        self.state.lock().delivery_journal.iter().cloned().collect()
    }

    pub(crate) fn ack_delivery_notification(&self, input_id: &str) {
        let mut state = self.state.lock();
        if let Some(index) = state
            .delivery_journal
            .iter()
            .position(|item| item.input_id == input_id)
        {
            state.delivery_journal.remove(index);
        }
    }

    async fn mark_delivery_notifications(
        &self,
        inputs: &[(MessageId, MessageContent)],
    ) -> Vec<String> {
        let delivered = {
            let mut state = self.state.lock();
            if !state.valid {
                return Vec::new();
            }
            let mut delivered = Vec::new();
            {
                let MailboxState {
                    records,
                    delivered_notifications,
                    delivery_journal,
                    ..
                } = &mut *state;
                // Receive 的 ids 是 MQ 实际消费顺序，不可按 enqueue 的 records 顺序补确认。
                for (id, content) in inputs {
                    let Some(record) = records.iter_mut().find(|record| matches_id(record, &[*id]))
                    else {
                        continue;
                    };
                    if matches!(
                        record.state,
                        UserInputState::Claimed | UserInputState::Delivered
                    ) && delivered_notifications.insert(record.input_id.clone())
                    {
                        debug_assert!(
                            record.delivery_reserved,
                            "accepted input must hold a delivery journal slot"
                        );
                        if !record.delivery_reserved
                            && delivery_journal.len() >= DELIVERY_JOURNAL_CAPACITY
                        {
                            tracing::error!(
                                input_id = %record.input_id,
                                "delivery reservation invariant broken; journal temporarily over capacity"
                            );
                        }
                        record.state = UserInputState::Delivered;
                        record.delivery_reserved = false;
                        delivered.push(record.input_id.clone());
                        // 正文在记录清理之前进入有序日志：事件通道即使丢掉
                        // UserInputDelivered，日志条目仍会在本轮尾部补发，不会静默丢失。
                        delivery_journal.push_back(PendingDeliveryNotification {
                            input_id: record.input_id.clone(),
                            content: content.clone(),
                            session_seq: None,
                        });
                    }
                }
            }
            if delivered.is_empty() {
                return delivered;
            }
            state.revision += 1;
            for id in &delivered {
                retain_terminal(&mut state, id);
            }
            prune_terminal_records(&mut state);
            delivered
        };
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
        delivered
    }

    /// 仍存在于 MQ 的 Dispatching 记录恢复 Queued；已移交的不恢复。
    async fn reclaim(&self) {
        let ids = {
            let state = self.state.lock();
            if !state.valid {
                return;
            }
            let ids: Vec<String> = state
                .records
                .iter()
                .filter(|record| record.state == UserInputState::Dispatching)
                .map(|record| record.input_id.clone())
                .collect();
            ids
        };
        if ids.is_empty() {
            return;
        }
        let existing: HashSet<String> = match self.query_user_inputs().await {
            Ok(entries) => entries
                .iter()
                .map(|entry| entry.id.as_uuid().to_string())
                .collect(),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "user input reclaim: MQ query failed; keeping dispatching runtime state"
                );
                return;
            }
        };
        let mut reclaimed = Vec::new();
        {
            let mut state = self.state.lock();
            let mut changed = false;
            for record in &mut state.records {
                if record.state == UserInputState::Dispatching
                    && (!record.handed_off || existing.contains(&record.input_id))
                {
                    record.state = UserInputState::Queued;
                    record.handed_off = false;
                    reclaimed.push(record.input_id.clone());
                    changed = true;
                }
            }
            state.ready.clear();
            if changed {
                state.revision += 1;
            }
        }
        let reclaimed_ids: Vec<MessageId> = reclaimed
            .iter()
            .filter_map(|id| uuid::Uuid::parse_str(id).ok().map(MessageId::from))
            .collect();
        if !reclaimed_ids.is_empty() {
            self.inbox.queue().withdraw_send_inputs(&reclaimed_ids);
        }
        let snapshot = self.snapshot().await;
        self.publish(snapshot);
    }

    async fn query_user_inputs(&self) -> Result<Vec<MqEntry>, UserInputQueueError> {
        self.inbox
            .queue()
            .query(MqFilter {
                user_input_only: true,
                ..Default::default()
            })
            .await
            .map_err(|error| UserInputQueueError::Store(error.to_string()))
    }

    /// 发送一组 ready 记录；`clear_ready` 为 true 时同时消费运行期 ready 队列。
    async fn send_ready_ids(&self, ids: Vec<String>, clear_ready: bool) {
        let message_ids: Vec<MessageId> = ids
            .iter()
            .filter_map(|id| uuid::Uuid::parse_str(id).ok().map(MessageId::from))
            .collect();
        if message_ids.is_empty() {
            return;
        }
        match self.inbox.queue().send_inputs(message_ids).await {
            Ok(_) => {
                let mut state = self.state.lock();
                for record in &mut state.records {
                    if ids.contains(&record.input_id) {
                        record.handed_off = true;
                    }
                }
                if clear_ready {
                    state.ready.retain(|id| !ids.contains(id));
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

    async fn receipt(
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

    fn publish(&self, snapshot: UserInputQueueSnapshot) {
        let ids: HashSet<_> = snapshot
            .terminal_items
            .iter()
            .map(|item| item.input_id.clone())
            .collect();
        (self.emit)(StateEvent::UserInputQueueChanged {
            turn_id: self.control_turn,
            agent_id: self.control_agent,
            snapshot,
        });
        // 先发布再允许淘汰；这里只保证进入发布边界，不保证慢客户端收到。
        // 并发发布期间窗口可暂时超过容量，避免尚未发布的新终态被挤掉。
        let mut state = self.state.lock();
        for item in &mut state.terminal_display {
            if ids.contains(&item.input_id) {
                item.published = true;
            }
        }
        prune_terminal_records(&mut state);
    }
}

fn snapshot_seed(state: &MailboxState) -> SnapshotSeed {
    SnapshotSeed {
        revision: state.revision,
        active_request_id: state
            .active
            .as_ref()
            .filter(|active| active.managed && active.cancel.is_some())
            .map(|active| active.ticket.id.clone()),
        // 终态不随 pending 项消失：客户端据此补齐丢失的交付确认。
        terminal_items: state
            .terminal_display
            .iter()
            .filter_map(|item| {
                let input_uuid = uuid::Uuid::parse_str(&item.input_id).ok()?;
                let identity = state.terminal_identities.get(&input_uuid)?;
                Some(UserInputQueueTerminalItem {
                    input_id: item.input_id.clone(),
                    state: identity.state,
                })
            })
            .collect(),
    }
}

fn assemble_snapshot(
    session_id: &str,
    generation: &str,
    seed: SnapshotSeed,
    items: Vec<UserInputQueueItem>,
) -> UserInputQueueSnapshot {
    UserInputQueueSnapshot {
        session_id: session_id.to_owned(),
        generation: generation.to_owned(),
        revision: seed.revision,
        active_request_id: seed.active_request_id,
        items,
        terminal_items: seed.terminal_items,
    }
}

fn remember_command(
    state: &mut MailboxState,
    command: &str,
    fingerprint: u64,
    results: Vec<UserInputItemResult>,
    taken_back: Option<UserInput>,
) {
    state.commands.insert(
        command.to_owned(),
        CommandReceipt {
            fingerprint,
            results,
            taken_back,
        },
    );
}

#[cfg(test)]
impl UserInputMailbox {
    // 旧单元测试以同步断言描述队列；这里 block_on 同一 async 实现，不复制事实源。
    pub fn enqueue_sync(
        &self,
        request: &EnqueueUserInputRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        futures::executor::block_on(self.enqueue(request))
    }

    pub fn dispatch_sync(
        &self,
        request: &DispatchUserInputsRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        futures::executor::block_on(self.dispatch(request))
    }

    pub fn take_back_sync(
        &self,
        request: &TakeBackUserInputRequest,
    ) -> Result<UserInputQueueReceipt, UserInputQueueError> {
        futures::executor::block_on(self.take_back(request))
    }

    pub fn snapshot_sync(&self) -> UserInputQueueSnapshot {
        futures::executor::block_on(self.snapshot())
    }

    pub fn hydrate_sync(&self) -> Result<(), UserInputQueueError> {
        futures::executor::block_on(self.hydrate())
    }

    pub fn attach_attempt_sync(
        &self,
        ticket: &UserInputRunTicket,
        cancel: CancellationToken,
    ) -> bool {
        futures::executor::block_on(self.attach_attempt(ticket, cancel))
    }

    pub fn attach_external_attempt_sync(
        &self,
        cancel: CancellationToken,
        resume_pending: bool,
    ) -> Option<UserInputRunTicket> {
        futures::executor::block_on(self.attach_external_attempt(cancel, resume_pending))
    }

    pub fn finish_attempt_sync(
        &self,
        ticket: &UserInputRunTicket,
        fallback: UserInputAttemptOutcome,
    ) {
        futures::executor::block_on(self.finish_attempt(ticket, fallback));
    }

    pub fn stop_sync(&self) {
        futures::executor::block_on(self.stop());
    }

    pub fn stop_attempt_sync(&self, request_id: &str, generation: &str) -> bool {
        futures::executor::block_on(self.stop_attempt(request_id, generation))
    }

    pub fn enter_idle_sync(&self) {
        futures::executor::block_on(self.enter_idle());
    }

    pub fn mark_claimed_sync(&self, ids: &[MessageId]) {
        futures::executor::block_on(self.mark_claimed(ids));
    }

    pub fn mark_delivered_sync(&self, ids: &[MessageId]) -> Vec<String> {
        let inputs: Vec<_> = ids
            .iter()
            .map(|id| (*id, MessageContent::text("")))
            .collect();
        futures::executor::block_on(self.mark_delivered(&inputs))
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
    uuid::Uuid::parse_str(&record.input_id)
        .ok()
        .is_some_and(|id| ids.contains(&MessageId::from(id)))
}

fn ready_ids(state: &MailboxState) -> Vec<String> {
    state.ready.clone()
}

fn promote_next(state: &mut MailboxState) -> bool {
    if !state.ready.is_empty()
        || state.records.iter().any(|record| {
            matches!(
                record.state,
                UserInputState::Dispatching | UserInputState::Claimed
            )
        })
    {
        return false;
    }
    let next = state
        .records
        .iter()
        .find(|record| record.state == UserInputState::Queued)
        .map(|record| record.input_id.clone());
    next.is_some_and(|id| !promote_ids(state, &[id]).is_empty())
}

fn promote_ids(state: &mut MailboxState, ids: &[String]) -> Vec<String> {
    let mut promoted = Vec::new();
    // 单次 ALL 按服务端原顺序；后续命令追加到 ready，保持 B 后 A 的接受顺序。
    for record in &mut state.records {
        if record.state == UserInputState::Queued && ids.contains(&record.input_id) {
            record.state = UserInputState::Dispatching;
            record.handed_off = false;
            state.ready.push(record.input_id.clone());
            promoted.push(record.input_id.clone());
        }
    }
    if !promoted.is_empty() {
        state.revision += 1;
    }
    promoted
}

fn results_for(state: &MailboxState, ids: &[String]) -> Vec<UserInputItemResult> {
    ids.iter()
        .map(|id| UserInputItemResult {
            input_id: id.clone(),
            state: state
                .records
                .iter()
                .find(|record| record.input_id == *id)
                .map(|record| record.state)
                .or_else(|| {
                    uuid::Uuid::parse_str(id)
                        .ok()
                        .and_then(|input_uuid| state.terminal_identities.get(&input_uuid))
                        .map(|identity| identity.state)
                })
                .unwrap_or(UserInputState::Unknown),
        })
        .collect()
}

fn user_input_payload(entry: &MqEntry) -> Option<(MessageContent, String)> {
    match &entry.payload {
        PersistedPayload::Message(BaseMessage::Human { content, .. }) => Some((
            content.clone(),
            entry.original_draft.clone().unwrap_or_default(),
        )),
        _ => None,
    }
}

fn user_input_from_entry(entry: &MqEntry) -> Option<UserInput> {
    let (content, original_draft) = user_input_payload(entry)?;
    Some(UserInput {
        input_id: entry.id.as_uuid().to_string(),
        content,
        original_draft,
    })
}

fn fingerprint_for(input_id: &str, content: &MessageContent, original_draft: &str) -> u64 {
    compute_fingerprint(("user_input", input_id, content, original_draft))
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
