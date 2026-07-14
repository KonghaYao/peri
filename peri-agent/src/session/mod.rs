//! Session v2 — 会话统一入口
//!
//! Session 是 peri-agent 的顶层门面，聚合五个核心实体：
//! - [`SessionStore`]：会话生命周期数据（不可变），含 FrozenContext
//! - [`MessageQueue`]：收件箱，异步消息注入
//! - [`SessionConfig`]：可变配置（权限模式、Cancel Token、超时）
//! - [`MessageTranscript`]：对话笔录，只追加不篡改
//! - [`TurnContext`]：单次 turn 上下文，turn 结束即销毁
//!
//! 外部通过 `Session::new()` 创建，按需访问五个实体，通过 `start_turn()` 启动新 turn。
//!
//! ## 异步 Owner（v2 新增）
//!
//! Session 可选地持有三个异步 owner：
//! - [`SessionInbox`](crate::agent::session::SessionInbox)：await-wake 包装器，用于 idle 期间阻塞唤醒
//! - [`CronOwner`](crate::agent::session::CronOwner)：cron trigger → inbox 桥接
//! - [`ChannelOwner`](crate::agent::session::ChannelOwner)：channel notification → inbox 桥接
//!
//! 这些 owner 在 `peri-acp` 层通过 `set_async_owners` 注入。持有 owner 后，
//! cron/channel 事件直接通过 inbox 唤醒 executor，无需 TUI 轮询。
//! 不设置 owner 时，TUI 轮询路径仍然有效（向后兼容）。

pub mod config;
pub mod queue;
pub mod store;
pub mod transcript;
pub mod turn;

pub use config::{PermissionMode, SessionConfig, ThinkingConfig};
pub use queue::{MessageKind, MessageQueue, MessageSource, QueuedMessage};
pub use store::{FrozenContext, FrozenContextBuilder, SessionId, SessionStore};
pub use transcript::{MessageFlags, MessageTranscript, StagedData, TranscriptEntry};
pub use turn::{TurnContext, TurnId};

use std::sync::Arc;

use parking_lot::RwLock;

use crate::agent::session::{
    channel_owner::ChannelOwner, cron_owner::CronOwner, inbox::SessionInbox,
};
use crate::thread::ThreadId;

/// 异步 owner 容器（set-once，RwLock 保护）
#[allow(dead_code)]
pub struct AsyncOwners {
    inbox: SessionInbox,
    cron_owner: Option<CronOwner>,
    channel_owner: Option<ChannelOwner>,
}

/// Session — 会话统一入口
///
/// 聚合五个核心实体，提供统一的创建和访问 API。
/// 通过 `Arc<Self>` 共享，外部通过 `Session::new()` 创建。
///
/// 可选持有异步 owner（inbox / cron / channel），用于直接桥接
/// 异步事件到 executor 的 idle-wake 机制。
pub struct Session {
    /// 会话生命周期数据（不可变）
    store: Arc<SessionStore>,
    /// 对话笔录（只追加，RwLock 保护内部可变性）
    transcript: Arc<RwLock<MessageTranscript>>,
    /// 收件箱（独立于 Transcript，会话内持续可变）
    queue: MessageQueue,
    /// 可变配置（Arc 共享，外部写入，循环读取）
    config: Arc<SessionConfig>,
    /// 异步 owner 容器（set-once，RwLock 保护）。
    /// None 表示未启用 async owner 路径（TUI polling 路径仍有效）。
    /// Some(rwlock) 表示 v2 路径已启用，可后续通过 set_async_owners 注入。
    async_owners: Option<parking_lot::RwLock<Option<AsyncOwners>>>,
}

impl Session {
    /// 创建新 Session
    ///
    /// - `cwd`：工作目录
    /// - `frozen`：会话级不可变上下文（System Prompt / CLAUDE.md / Skills）
    /// - `thread_id`：关联的 Thread ID（可选，用于持久化）
    pub fn new(cwd: Arc<str>, frozen: FrozenContext, thread_id: Option<ThreadId>) -> Arc<Self> {
        let store = Arc::new(SessionStore::new(cwd, frozen, thread_id));
        let transcript = Arc::new(RwLock::new(MessageTranscript::new()));
        let queue = MessageQueue::new();
        let config = Arc::new(SessionConfig::new());
        Arc::new(Self {
            store,
            transcript,
            queue,
            config,
            async_owners: None,
        })
    }

    /// 创建新 Session，复用外部 cancel token（v2 路径用）
    ///
    /// 与 [`Session::new`] 的差异仅在于 cancel_token：传入的 token 是
    /// "linked clone"（`CancellationToken::clone()` 创建的关联 token），
    /// 父 token 取消时本 Session 也能感知。
    pub fn new_with_cancel(
        cwd: Arc<str>,
        frozen: FrozenContext,
        thread_id: Option<ThreadId>,
        cancel_token: Arc<tokio_util::sync::CancellationToken>,
    ) -> Arc<Self> {
        let store = Arc::new(SessionStore::new(cwd, frozen, thread_id));
        let transcript = Arc::new(RwLock::new(MessageTranscript::new()));
        let queue = MessageQueue::new();
        let mut config = SessionConfig::new();
        config.cancel_token = cancel_token;
        let config = Arc::new(config);
        Arc::new(Self {
            store,
            transcript,
            queue,
            config,
            async_owners: None,
        })
    }

    /// 创建新 Session，复用外部 cancel token + 外部共享 MessageQueue（v2 路径用）
    ///
    /// 与 [`Session::new_with_cancel`] 的差异仅在于 queue：传入的 `queue`
    /// 是会话级共享实例（通常由 ACP `AcpSession.v2_message_queue` 持有），
    /// 让每个 turn 构造的 v2 Session 都指向**同一个**底层收件箱。
    ///
    /// **背景**：`MessageQueue` 内部用 `Arc<Mutex<VecDeque>> + Arc<Notify>` 实现，
    /// `clone()` 共享底层数据。因此传入 `queue` 后，Session 内的 queue 与外部
    /// 共享同一份消息流——SubAgent / Hook / GoalSteering 注入的 deferred / info
    /// 消息可被 main agent 的 ReAct 循环看到。
    ///
    /// 不传时（即 [`Session::new_with_cancel`]）每 turn 新建 MessageQueue，
    /// 跨 turn / 跨组件的消息互不可见。
    pub fn new_with_cancel_and_queue(
        cwd: Arc<str>,
        frozen: FrozenContext,
        thread_id: Option<ThreadId>,
        cancel_token: Arc<tokio_util::sync::CancellationToken>,
        queue: MessageQueue,
    ) -> Arc<Self> {
        let store = Arc::new(SessionStore::new(cwd, frozen, thread_id));
        let transcript = Arc::new(RwLock::new(MessageTranscript::new()));
        let mut config = SessionConfig::new();
        config.cancel_token = cancel_token;
        let config = Arc::new(config);
        Arc::new(Self {
            store,
            transcript,
            queue,
            config,
            // v2 路径启用 async owner 容器（RwLock，允许后续 set_async_owners 注入）
            async_owners: Some(parking_lot::RwLock::new(None)),
        })
    }

    /// 会话生命周期数据（不可变）
    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    /// 对话笔录（RwLock 保护）
    pub fn transcript(&self) -> Arc<RwLock<MessageTranscript>> {
        self.transcript.clone()
    }

    /// 收件箱
    pub fn queue(&self) -> &MessageQueue {
        &self.queue
    }

    /// 可变配置
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// 启动新 turn — 创建 TurnContext
    ///
    /// 共享 cwd 和 cancel token，turn 内 step 从 0 开始。
    pub fn start_turn(&self) -> TurnContext {
        TurnContext::new(self.store.cwd.clone(), self.config.cancel_token.clone())
    }

    /// Session-level inbox（await-wake wrapper）。
    ///
    /// 返回 `None` 表示未启用 async owner 路径。
    /// 启用后，ACP executor 的 `run_session_loop` 可在 idle 期间调用
    /// `inbox.await_wake()` 阻塞直到新消息到达。
    ///
    /// 返回 `RwLockReadGuard`，调用者通过 guard 访问 `SessionInbox`。
    /// guard drop 后释放读锁。
    pub fn session_inbox_guard(
        &self,
    ) -> Option<parking_lot::RwLockReadGuard<'_, Option<AsyncOwners>>> {
        self.async_owners.as_ref().map(|m| m.read())
    }

    /// Async owners 读守卫。
    ///
    /// 返回 `RwLockReadGuard<Option<AsyncOwners>>`，调用者可访问
    /// `.inbox` / `.cron_owner` / `.channel_owner`。
    pub fn async_owners_guard(
        &self,
    ) -> Option<parking_lot::RwLockReadGuard<'_, Option<AsyncOwners>>> {
        self.async_owners.as_ref().map(|m| m.read())
    }

    /// 注入异步 owner（SessionInbox + CronOwner + ChannelOwner）。
    ///
    /// 由 `peri-acp` 层在构建 v2 session 后调用，将 cron/channel
    /// 事件直接桥接到 inbox，绕过 TUI 轮询。
    ///
    /// 每个 owner 的 `start()` 方法在此调用前应已执行（background task 已 spawn）。
    /// 此方法仅设置引用，不启动 background task。
    ///
    /// 传入 `None` 的 owner 表示该路径仍由 TUI 轮询处理。
    ///
    /// `inbox` 参数为 `Some` 时必须提供——它是 async owner 路径的核心。
    /// 如果不需要 async owner 路径，不要调用此方法。
    ///
    /// Returns `true` if owners were set successfully, `false` if already set or
    /// no `async_owners` cell was initialized.
    pub fn set_async_owners(
        &self,
        inbox: SessionInbox,
        cron: Option<CronOwner>,
        channel: Option<ChannelOwner>,
    ) -> bool {
        if let Some(rwlock) = &self.async_owners {
            let mut guard = rwlock.write();
            if guard.is_some() {
                tracing::warn!("set_async_owners: already set, ignoring duplicate call");
                return false;
            }
            *guard = Some(AsyncOwners {
                inbox,
                cron_owner: cron,
                channel_owner: channel,
            });
            true
        } else {
            false
        }
    }

    /// 检查 async owner 是否已初始化。
    pub fn has_async_owners(&self) -> bool {
        self.async_owners
            .as_ref()
            .map(|m| m.read().is_some())
            .unwrap_or(false)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
