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

use crate::thread::ThreadId;

/// Session — 会话统一入口
///
/// 聚合五个核心实体，提供统一的创建和访问 API。
/// 通过 `Arc<Self>` 共享，外部通过 `Session::new()` 创建。
pub struct Session {
    /// 会话生命周期数据（不可变）
    store: Arc<SessionStore>,
    /// 对话笔录（只追加，RwLock 保护内部可变性）
    transcript: Arc<RwLock<MessageTranscript>>,
    /// 收件箱（独立于 Transcript，会话内持续可变）
    queue: MessageQueue,
    /// 可变配置（Arc 共享，外部写入，循环读取）
    config: Arc<SessionConfig>,
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
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_construction() {
        let cwd: Arc<str> = Arc::from("/tmp/project");
        let frozen = FrozenContext::builder()
            .system_prompt("You are Peri.")
            .claude_md("# Rules")
            .build();
        let session = Session::new(cwd.clone(), frozen, Some("thread-1".into()));

        // 五个实体均可访问
        assert_eq!(&*session.store().cwd, "/tmp/project");
        assert_eq!(&*session.store().frozen.system_prompt, "You are Peri.");
        assert!(session.transcript().read().is_empty());
        assert!(session.queue().is_empty());
        assert_eq!(session.config().permission_mode(), PermissionMode::Default);
    }

    #[test]
    fn test_session_store_access() {
        let cwd: Arc<str> = Arc::from("/tmp");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);

        let store = session.store();
        assert_ne!(store.session_id.as_uuid(), uuid::Uuid::nil());
        assert!(store.thread_id.is_none());
        assert!(!store.is_git_repo());

        store.set_is_git_repo(true);
        assert!(store.is_git_repo());
    }

    #[test]
    fn test_session_transcript_access() {
        let cwd: Arc<str> = Arc::from("/tmp");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);

        // transcript() 返回 Arc clone，可跨线程共享
        let t1 = session.transcript();
        let t2 = session.transcript();
        assert!(Arc::ptr_eq(&t1, &t2), "多次调用应返回同一 Arc");
    }

    #[test]
    fn test_session_queue_access() {
        let cwd: Arc<str> = Arc::from("/tmp");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);

        let q = session.queue();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn test_session_config_access() {
        let cwd: Arc<str> = Arc::from("/tmp");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);

        assert_eq!(session.config().max_iterations(), 500);
        session.config().set_max_iterations(100);
        assert_eq!(session.config().max_iterations(), 100);
    }

    #[test]
    fn test_start_turn_creates_fresh_context() {
        let cwd: Arc<str> = Arc::from("/tmp/project");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd.clone(), frozen, None);

        let ctx = session.start_turn();
        assert_eq!(ctx.current_step(), 0, "新 turn 的 step 应为 0");
        assert_eq!(&*ctx.cwd, "/tmp/project", "turn 应共享 session 的 cwd");
        assert!(!ctx.is_cancelled(), "新 turn 不应已取消");

        // Cancel session config 后 turn 应感知
        session.config().cancel();
        assert!(ctx.is_cancelled(), "turn 应感知 session 级 cancel");
    }

    #[test]
    fn test_start_turn_independent_turn_ids() {
        let cwd: Arc<str> = Arc::from("/tmp");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);

        let ctx1 = session.start_turn();
        let ctx2 = session.start_turn();
        assert_ne!(
            ctx1.turn_id, ctx2.turn_id,
            "每次 start_turn 应生成独立 TurnId"
        );
    }

    #[test]
    fn test_session_is_arc_shared() {
        let cwd: Arc<str> = Arc::from("/tmp");
        let frozen = FrozenContext::builder().build();
        let session = Session::new(cwd, frozen, None);

        // Session::new 返回 Arc<Self>，clone 应指向同一实例
        let clone = Arc::clone(&session);
        assert!(Arc::ptr_eq(&session, &clone));
    }

    #[test]
    fn test_new_with_cancel_and_queue_shares_underlying_queue() {
        // 验证：传入外部 MessageQueue 后，session.queue() 与外部共享底层。
        // 这是 v2 路径 "session 共享 MessageQueue" 修复的核心契约。
        use crate::messages::BaseMessage;
        use crate::messages::MessageContent;
        use crate::session::queue::{MessageKind, MessageSource, QueuedMessage};

        let cwd: Arc<str> = Arc::from("/tmp");
        let frozen = FrozenContext::builder().build();
        let cancel = Arc::new(tokio_util::sync::CancellationToken::new());
        let shared = MessageQueue::new();

        // 在创建 session 前 push 一条——验证 session 内部 queue 能看到
        shared.push(QueuedMessage::new(
            MessageKind::Info,
            MessageSource::SystemInjected,
            BaseMessage::human(MessageContent::text("pre-existing")),
        ));

        let session = Session::new_with_cancel_and_queue(cwd, frozen, None, cancel, shared.clone());

        // session.queue() 应看到外部 push 的消息
        assert_eq!(
            session.queue().len(),
            1,
            "session.queue() 应与外部 shared 共享同一底层 VecDeque"
        );

        // 从 session.queue() push，外部 shared 应看到
        session.queue().push(QueuedMessage::prompt(
            MessageSource::UserInput,
            BaseMessage::human(MessageContent::text("from session")),
        ));
        assert_eq!(shared.len(), 2, "外部 shared 应看到 session 侧 push 的消息");
    }

    #[test]
    fn test_new_with_cancel_and_queue_cancel_propagates() {
        // 验证：cancel_token 仍为 linked（父 cancel 时 session 内 turn 能感知）
        let cwd: Arc<str> = Arc::from("/tmp");
        let frozen = FrozenContext::builder().build();
        let cancel = Arc::new(tokio_util::sync::CancellationToken::new());
        let session = Session::new_with_cancel_and_queue(
            cwd,
            frozen,
            None,
            cancel.clone(),
            MessageQueue::new(),
        );

        let turn = session.start_turn();
        assert!(!turn.is_cancelled());
        cancel.cancel();
        assert!(turn.is_cancelled(), "linked cancel token 应传播到 turn");
    }
}
