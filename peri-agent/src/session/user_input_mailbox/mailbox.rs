//! Implementation leaf for `crate::session::user_input_mailbox`.

pub(super) use std::collections::HashMap;
pub(super) use std::collections::HashSet;
pub(super) use std::collections::VecDeque;
pub(super) use std::fmt::{self, Debug, Formatter};
pub(super) use std::sync::Arc;

pub(super) use super::window::{PendingDeliveryNotification, TerminalDisplay, TerminalIdentity};

pub(super) use parking_lot::Mutex;
pub(super) use peri_acp_types::event_v2::StateEvent;
pub(super) use peri_acp_types::identity::AgentId;

pub(super) use peri_acp_types::messages::MessageContent;

pub(super) use peri_acp_types::session::SessionInbox;

pub(super) use peri_acp_types::session::TurnId;
pub(super) use peri_acp_types::session::UserInput;
pub(super) use peri_acp_types::session::UserInputItemResult;
pub(super) use peri_acp_types::session::UserInputQueueTerminalItem;
pub(super) use peri_acp_types::session::UserInputState;

pub(super) use tokio_util::sync::CancellationToken;

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
pub(super) enum InterruptReason {
    None,
    Steer,
    Stop,
}

pub(super) struct ActiveRun {
    pub(super) ticket: UserInputRunTicket,
    pub(super) cancel: Option<CancellationToken>,
    pub(super) reason: InterruptReason,
    pub(super) outcome: Option<UserInputAttemptOutcome>,
    pub(super) managed: bool,
}

/// 运行期调度记录；正文与原稿是投影缓存，对账仍以 MQ 查询结果为准。
pub(super) struct InputRecord {
    pub(super) input_id: String,
    /// 记录进入内存的单调序号：对账只能清理查询开始前已存在的记录。
    pub(super) seq: u64,
    pub(super) fingerprint: u64,
    pub(super) content: MessageContent,
    pub(super) original_draft: String,
    pub(super) state: UserInputState,
    pub(super) handed_off: bool,
    /// 交付日志槽位在 enqueue 时预留；交付时转为日志条目，撤回时释放。
    /// 这样 `mark_delivered` 永远不会在消息已写入 transcript 后才因容量失败。
    pub(super) delivery_reserved: bool,
}

pub(super) struct CommandReceipt {
    pub(super) fingerprint: u64,
    pub(super) results: Vec<UserInputItemResult>,
    pub(super) taken_back: Option<UserInput>,
}

pub(super) struct MailboxState {
    pub(super) revision: u64,
    /// 记录序号的下一个值；用于判断一条记录是否早于某次 MQ 查询。
    pub(super) next_seq: u64,
    pub(super) records: Vec<InputRecord>,
    // generation 内的终态身份不可淘汰，否则同 ID 会重新获得执行资格。
    //
    // 生命周期：只随 generation 结束（会话重载/进程重建）回收。客户端可以在
    // 同一 generation 内无限期重提任一旧 ID，因此不存在不破坏幂等的回收时点；
    // 有界 LRU 会让被淘汰的 ID 重新可执行，不能采用。
    //
    // 空间：input_id 已是规范 UUID，这里用 16B UUID 作为紧凑身份键（而不是
    // 36B 字符串 + 堆分配）。每条记录约 16B key + 8B fingerprint + 1B state，
    // 加 HashMap 开销约 40-56B；100 万条输入约 40-56 MiB。这是精确幂等的代价。
    pub(super) terminal_identities: HashMap<uuid::Uuid, TerminalIdentity>,
    pub(super) terminal_display: Vec<TerminalDisplay>,
    pub(super) ready: Vec<String>,
    pub(super) commands: HashMap<String, CommandReceipt>,
    pub(super) active: Option<ActiveRun>,
    /// 正在等待 MQ 提交的接纳数；容量判定必须把这段异步窗口计入，
    /// 否则并发 enqueue 可以同时通过检查（MQ 才是事实源，不能先写记录）。
    pub(super) admissions_in_flight: usize,
    pub(super) paused: bool,
    pub(super) suspended: bool,
    pub(super) valid: bool,
    pub(super) delivered_notifications: HashSet<String>,
    pub(super) delivery_journal: VecDeque<PendingDeliveryNotification>,
}

/// 由宿主持有跨 turn 实例，Agent 独占队列状态、取消原因和执行准入判定。
pub struct UserInputMailbox {
    pub(super) session_id: String,
    pub(super) generation: String,
    pub(super) inbox: Arc<SessionInbox>,
    pub(super) state: Mutex<MailboxState>,
    pub(super) emit: Arc<dyn Fn(StateEvent) + Send + Sync>,
    pub(super) control_turn: TurnId,
    pub(super) control_agent: AgentId,
}

/// 组装快照所需的运行期切片；与 items 在同一把锁内取得。
pub(super) struct SnapshotSeed {
    pub(super) revision: u64,
    pub(super) active_request_id: Option<String>,
    pub(super) terminal_items: Vec<UserInputQueueTerminalItem>,
}

impl UserInputMailbox {
    #[must_use]
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
                next_seq: 0,
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

    #[must_use]
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
}

#[cfg(test)]
impl UserInputMailbox {
    // 旧单元测试以同步断言描述队列；这里 block_on 同一 async 实现，不复制事实源。
    pub fn enqueue_sync(
        &self,
        request: &peri_acp_types::session::EnqueueUserInputRequest,
    ) -> Result<peri_acp_types::session::UserInputQueueReceipt, UserInputQueueError> {
        futures::executor::block_on(self.enqueue(request))
    }

    pub fn dispatch_sync(
        &self,
        request: &peri_acp_types::session::DispatchUserInputsRequest,
    ) -> Result<peri_acp_types::session::UserInputQueueReceipt, UserInputQueueError> {
        futures::executor::block_on(self.dispatch(request))
    }

    pub fn take_back_sync(
        &self,
        request: &peri_acp_types::session::TakeBackUserInputRequest,
    ) -> Result<peri_acp_types::session::UserInputQueueReceipt, UserInputQueueError> {
        futures::executor::block_on(self.take_back(request))
    }

    #[must_use]
    pub fn snapshot_sync(&self) -> peri_acp_types::session::UserInputQueueSnapshot {
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

    pub fn mark_claimed_sync(&self, ids: &[peri_acp_types::messages::MessageId]) {
        futures::executor::block_on(self.mark_claimed(ids));
    }

    pub fn mark_delivered_sync(&self, ids: &[peri_acp_types::messages::MessageId]) -> Vec<String> {
        let inputs: Vec<_> = ids
            .iter()
            .map(|id| (*id, MessageContent::text("")))
            .collect();
        futures::executor::block_on(self.mark_delivered(&inputs))
    }
}

impl Debug for UserInputMailbox {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserInputMailbox")
            .field("session_id", &self.session_id)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}
