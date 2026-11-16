//! TUI-private semantic ownership for reverse interactions.
//!
//! Wire request ids identify JSON-RPC frames. [`InteractionOwner`] identifies
//! the one UI operation that may settle a reverse request.  Session boundaries,
//! prompt terminals, bridge publication and user responses all compete for that
//! same owner in this module tree.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use peri_acp::transport::types::RequestId;
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use super::super::client::AcpNotification;

/// Maximum initialization notifications retained until a newly created session is committed.
pub const NEW_SESSION_EVENT_BUFFER_CAPACITY: usize = 64;

pub(super) static NEXT_CLIENT_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReverseInteractionKind {
    Permission,
    Elicitation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionKind {
    New,
    Load,
    DeleteCurrent,
}

/// 一次会话操作（预约、建立、切换）的身份。
///
/// 由生命周期在操作开始时分配，并贯穿 `NoSession → Transitioning → Stable`：
/// 队列按这个身份分道，不再从 `unclaimed` 标记预测下一次 generation。预约在
/// 创建时取得身份，因此多次连续预约各自身份明确，最新一个代表用户最新决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionOperationId(pub(super) u64);

/// 新切换使用的操作身份来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransitionOperation {
    /// 采用调用方已持有的身份（预约或准入 cohort）。
    Owned(SessionOperationId),
    /// 沿用队列已登记、尚未落定的身份；没有则新建。启动恢复用它接续排队车道。
    AdoptPending,
    /// 新的一次用户决定：新建身份并取代待定身份。
    New,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRoute {
    Bootstrapping {
        generation: u64,
    },
    Stable {
        session_id: String,
        generation: u64,
        /// 产生这个稳定会话的操作；`None` 仅出现在测试夹具。
        operation: Option<SessionOperationId>,
        accepting_reverse: bool,
        prompt_epoch: u64,
    },
    Transitioning {
        kind: TransitionKind,
        from: Option<String>,
        target: Option<String>,
        generation: u64,
        operation: SessionOperationId,
    },
    NoSession {
        generation: u64,
    },
}

/// 队列准入观察到的会话路由。
///
/// `generation` 在每次切换/建立时递增且提交时复用同一个值；`operation` 是
/// 产生该路由的操作身份。队列按操作身份分道：同一个预约/建立操作从开始到
/// 落定的所有输入共享一条有序车道，不依赖对 generation 的预测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AdmissionRoute {
    Stable {
        session_id: String,
        generation: u64,
        operation: Option<SessionOperationId>,
    },
    Transitioning {
        generation: u64,
        operation: SessionOperationId,
    },
    NoSession {
        generation: u64,
        /// 队列可等待的建立身份；无会话时总会分配，失败重试因此沿用同一车道。
        operation: SessionOperationId,
    },
}

/// Frozen client, session, generation, and prompt identity used to claim one reverse request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InteractionOwner {
    pub client_instance_id: u64,
    pub token: u64,
    pub session_id: String,
    pub generation: u64,
    pub prompt_epoch: u64,
    pub kind: ReverseInteractionKind,
}

impl Default for InteractionOwner {
    fn default() -> Self {
        Self {
            client_instance_id: 0,
            token: 0,
            session_id: "test-session".into(),
            generation: 0,
            prompt_epoch: 0,
            kind: ReverseInteractionKind::Permission,
        }
    }
}

/// Prompt identity retained until an exact terminal notification closes that prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMarker {
    pub client_instance_id: u64,
    pub session_id: String,
    pub generation: u64,
    pub prompt_epoch: u64,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserInputSnapshotIdentity {
    pub(super) client_instance_id: u64,
    pub(super) session_id: String,
    pub(super) local_generation: u64,
    pub(super) generation: String,
    pub(super) prompt_epoch: u64,
    pub(super) active_prompt: Option<PromptMarker>,
}

#[derive(Debug)]
pub(super) struct PendingInteractionEntry {
    pub(super) owner: InteractionOwner,
    pub(super) request_id: RequestId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimCause {
    UserResponse,
    BridgeReject,
    LifecycleDrain,
    TurnTerminal,
    TransportTerminal,
    /// 服务端通过 `$/cancel_request` 撤销请求（如提问超时）。
    ServerCancelled,
}

#[derive(Debug)]
pub struct ClaimedInteraction {
    pub owner: InteractionOwner,
    pub request_id: RequestId,
    pub cause: ClaimCause,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionExpiryReason {
    BridgeRejected,
    LifecycleDrain,
    TurnTerminal,
    TransportTerminal,
    ResponseTransportFailed,
    /// 服务端撤销了挂起请求（提问超时），本地 UI 须结算并释放输入。
    ServerCancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionUiOutcome {
    Resolved { result: String },
    Expired { reason: InteractionExpiryReason },
}

#[derive(Debug)]
pub struct RegisteredReverseRequest {
    pub owner: InteractionOwner,
    pub request_id_json: String,
    pub params: Value,
}

#[derive(Debug)]
pub enum RegisterDecision {
    Forward(RegisteredReverseRequest),
    Settle {
        kind: ReverseInteractionKind,
        id: RequestId,
    },
}

#[derive(Debug)]
pub enum OrdinaryDecision {
    Forward(Box<AcpNotification>),
    Buffered,
    Drop,
}

#[derive(Debug)]
pub(super) struct BufferedOrdinaryNotification {
    pub(super) session_id: String,
    pub(super) notification: AcpNotification,
}

#[derive(Debug)]
pub(super) struct ExecutionRuns {
    pub(super) generation: String,
    pub(super) local_generation: u64,
    pub(super) latest_request: Option<String>,
    pub(super) pending_terminal_request: Option<String>,
    pub(super) managed_input: bool,
    pub(super) retired: HashSet<String>,
}

#[derive(Debug)]
pub(super) struct InteractionLifecycleState {
    pub(super) client_instance_id: u64,
    pub(super) route: SessionRoute,
    /// 最近一次会话决定的操作身份；无会话时队列沿用它等待建立。
    pub(super) current_operation: Option<SessionOperationId>,
    pub(super) next_operation_id: u64,
    pub(super) next_token: u64,
    pub(super) deleted_session_ids: HashSet<String>,
    pub(super) pending: BTreeMap<u64, PendingInteractionEntry>,
    pub(super) active_prompt: Option<PromptMarker>,
    pub(super) execution_runs: HashMap<String, ExecutionRuns>,
    pub(super) new_buffer: VecDeque<BufferedOrdinaryNotification>,
    /// `commit_stable` 已切路由，但调用方的初始化（可能等待 RPC）尚未结束。
    /// 置位期间到达的通知继续排入 `new_buffer`，保证 flush 期间 FIFO 不被插队。
    pub(super) new_flush_pending: bool,
    pub(super) warned_buffer_full: bool,
}

/// Own session routing and first-claim settlement under one operation gate.
#[derive(Clone)]
pub struct InteractionLifecycle {
    pub(super) operation_gate: Arc<AsyncMutex<()>>,
    pub(super) state: Arc<Mutex<InteractionLifecycleState>>,
    pub(super) drop_settlement_tx: Arc<Mutex<Option<mpsc::UnboundedSender<ClaimedInteraction>>>>,
}

impl std::fmt::Debug for InteractionLifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InteractionLifecycle")
            .finish_non_exhaustive()
    }
}

impl InteractionLifecycle {
    #[must_use]
    pub fn new() -> Self {
        let client_instance_id = NEXT_CLIENT_INSTANCE_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .unwrap_or(0);
        Self {
            operation_gate: Arc::new(AsyncMutex::new(())),
            state: Arc::new(Mutex::new(InteractionLifecycleState {
                client_instance_id,
                route: SessionRoute::Bootstrapping { generation: 0 },
                current_operation: None,
                next_operation_id: 1,
                next_token: 1,
                deleted_session_ids: HashSet::new(),
                pending: BTreeMap::new(),
                active_prompt: None,
                execution_runs: HashMap::new(),
                new_buffer: VecDeque::new(),
                new_flush_pending: false,
                warned_buffer_full: false,
            })),
            drop_settlement_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Serialize interaction publication, prompt ownership, and session transitions.
    #[must_use]
    pub fn operation_gate(&self) -> &AsyncMutex<()> {
        &self.operation_gate
    }
}

impl Default for InteractionLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct TransitionStart {
    pub generation: u64,
    pub from: Option<String>,
    pub claims: Vec<ClaimedInteraction>,
}
