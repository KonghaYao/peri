//! Session 层契约类型（自 peri-agent 迁入；`peri-agent::session::{queue,turn,runtime}`
//! 与 `peri-agent::agent::session::{inbox,cron_owner}` 保留 re-export 保兼容）。
//!
//! 归位说明（§0 兜底：接口契约归 peri-acp-types）：MQ 消息管理、turn 身份、
//! inbox 唤醒、cron 触发桥、Agent 运行时注册表条目是跨层接口契约——
//! Agent 层持有实现与执行权，ACP / middlewares 只依赖本层契约类型。

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::command::PromptStopReason;
use crate::messages::BaseMessage;
use crate::thread::{CancelPolicy, ThreadId};

// ─── PromptResult（L5：自 peri-acp host/exec/executor.rs 契约化）────────────

/// 单轮 prompt 执行结果（ACP 协议面 / 执行薄壳消费；Agent 层命令执行体与
/// 执行句柄经本类型回传）。
pub struct PromptResult {
    /// 执行后的消息历史。
    pub messages: Vec<BaseMessage>,
    /// 是否执行成功。
    pub ok: bool,
    /// 执行停止原因。
    pub stop_reason: PromptStopReason,
    /// 本轮是否发生 Full Compact 提交并替换了先前的可见历史。
    pub history_replaced_by_compaction: bool,
    /// 执行期间收集的 recall 项（供下一轮注入）。
    pub recall_items: Vec<String>,
}

impl Default for PromptResult {
    /// 防御性回退（结果缺失 / 未执行时使用）：空失败结果。
    fn default() -> Self {
        Self {
            messages: Vec::new(),
            ok: false,
            stop_reason: PromptStopReason::EndTurn,
            history_replaced_by_compaction: false,
            recall_items: Vec::new(),
        }
    }
}

// ─── TurnId ──────────────────────────────────────────────────────────────────

/// Turn 唯一标识符 — UUID v7（时间有序）
///
/// 作为一次 turn 内所有事件的统一纽带。从 LlmCallStart 到 TurnCompleted 全程一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(uuid::Uuid);

impl TurnId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }
}

impl Default for TurnId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TurnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ─── MessageKind ─────────────────────────────────────────────────────────────

/// 消息 Kind — 控制循环唤醒行为
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// 外部主动请求 — drain_all 消费，循环结束后到达同样激活
    Prompt,
    /// 延迟到达的结果 — drain_all 消费，循环结束后到达同样激活
    Defer,
    /// 通知性数据 — drain_all 消费，永不唤醒循环
    Info,
}

impl MessageKind {
    /// 是否能唤醒新 turn
    pub fn wakes_up(self) -> bool {
        matches!(self, Self::Prompt | Self::Defer)
    }
}

// ─── MessageSource ───────────────────────────────────────────────────────────

/// 消息来源 — 用于调试和事件追踪
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSource {
    /// 外部用户输入
    UserInput,
    /// SubAgent 完成
    SubAgentComplete,
    /// 后台 Shell 完成
    ShellComplete,
    /// Goal steering（中途纠正）
    GoalSteering,
    /// Cron 定时触发
    CronTrigger,
    /// Stop hook feedback
    StopHookFeedback,
    /// Channel 消息（微信/Slack 等）
    ChannelMessage,
    /// Hook 系统注入
    SystemInjected,
    /// 工具失败警告
    ToolFailureWarning,
    /// 工作流完成
    WorkflowComplete,
    /// 推测深挖哨兵（SpeculationGuard 注入的提问纪律提醒）
    SpeculationGuard,
}

// ─── QueuedMessage ───────────────────────────────────────────────────────────

/// 一条待投递的消息（v2 富类型）
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    /// 消息 Kind（决定唤醒行为）
    pub kind: MessageKind,
    /// 消息来源
    pub source: MessageSource,
    /// 实际消息内容
    pub message: BaseMessage,
}

impl QueuedMessage {
    pub fn new(kind: MessageKind, source: MessageSource, message: BaseMessage) -> Self {
        Self {
            kind,
            source,
            message,
        }
    }

    /// 快速构造 Prompt 消息（用户输入）
    pub fn prompt(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Prompt, source, message)
    }

    /// 快速构造 Defer 消息（SubAgent/Cron/Channel/Workflow 延迟结果）
    pub fn defer(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Defer, source, message)
    }

    /// 快速构造 Info 消息（SystemReminder/Hook 注入，不唤醒循环）
    pub fn info(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Info, source, message)
    }
}

// ─── MessageQueue ────────────────────────────────────────────────────────────

/// 会话级临时收件箱（v2）
///
/// 内部用 `Arc<Mutex<VecDeque>>` 保证线程安全。`Notify` 用于异步等待新消息。
///
/// RCRA 循环中 Receive 阶段通过 [`drain_all`] 一次性消费全部三类消息；
/// 循环退出后通过 [`has_wake_up`] 检测是否需重新激活。
#[derive(Debug, Clone)]
pub struct MessageQueue {
    inner: Arc<Mutex<VecDeque<QueuedMessage>>>,
    notify: Arc<tokio::sync::Notify>,
}

impl Default for MessageQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageQueue {
    /// 创建空队列
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// 推入一条消息，唤醒等待者
    pub fn push(&self, msg: QueuedMessage) {
        {
            let mut inner = self.inner.lock();
            inner.push_back(msg);
        }
        self.notify.notify_one();
    }

    /// 批量推入消息；空列表为 no-op
    pub fn push_batch(&self, msgs: Vec<QueuedMessage>) {
        if msgs.is_empty() {
            return;
        }
        {
            let mut inner = self.inner.lock();
            inner.extend(msgs);
        }
        self.notify.notify_one();
    }

    /// 排空队列中的全部消息（Prompt + Info + Defer）
    ///
    /// RCRA 循环的 Receive 阶段调用，一次性消费全部类型。
    pub fn drain_all(&self) -> Vec<QueuedMessage> {
        let mut inner = self.inner.lock();
        let drained: Vec<_> = std::mem::take(&mut *inner).into();
        drop(inner);
        self.notify.notify_one();
        drained
    }

    /// 是否有能唤醒循环的消息（Prompt 或 Defer）
    pub fn has_wake_up(&self) -> bool {
        self.inner.lock().iter().any(|m| m.kind.wakes_up())
    }

    /// 队列中是否存在用户 Prompt（SpeculationGuard 区分"用户新输入"与
    /// Info/Defer 系统注入——只有用户 Prompt 才重置推测深挖计数）
    pub fn has_pending_prompt(&self) -> bool {
        self.inner
            .lock()
            .iter()
            .any(|m| m.kind == MessageKind::Prompt)
    }

    /// 队列中是否存在指定来源的 pending Defer（wake-able 延迟结果）。
    ///
    /// AsyncContinuation 用：`session/cancel` 时确认 SubAgentComplete Defer 是否
    /// 已入队（race 兜底——bg 完成通知可能已在 cancel 前置位前被 scheduler 跳过），
    /// continuation scheduler 在真正 dispatch 前确认 Defer 尚未被消费（跳过空跑）。
    /// 仅匹配 `MessageKind::Defer`：Prompt/Info 均不计入。
    pub fn has_pending_defer(&self, source: &MessageSource) -> bool {
        self.inner
            .lock()
            .iter()
            .any(|m| m.kind == MessageKind::Defer && &m.source == source)
    }

    /// 队列是否为空
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// 队列长度
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// 清空队列（rewind 操作时调用）
    pub fn clear(&self) {
        self.inner.lock().clear();
    }
}

// ─── SessionInbox / InboxHandle ──────────────────────────────────────────────

/// Wraps the existing v2 MessageQueue with an async await-wake mechanism.
///
/// During ReAct loop, `stages/receive.rs` calls `drain_all`
/// to consume pending messages — no wake needed (loop is already spinning).
///
/// During IDLE (between ReAct loops), the ACP executor calls [`await_wake`](Self::await_wake)
/// which blocks until a new Prompt/Defer is enqueued, then the loop resumes.
pub struct SessionInbox {
    queue: Arc<MessageQueue>,
    /// Dedicated notify for await_wake — separate from queue's internal notify
    /// to avoid spurious wakeups when Info messages are pushed.
    wake: Arc<tokio::sync::Notify>,
}

impl SessionInbox {
    /// Create a new SessionInbox wrapping the given queue.
    ///
    /// The queue is typically the session-level shared instance passed through
    /// `Session::new_with_cancel_and_queue`.
    pub fn new(queue: Arc<MessageQueue>) -> Self {
        Self {
            queue,
            wake: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Block until the inbox has at least one wake-able message (Prompt or Defer).
    ///
    /// Called by ACP executor's `run_session_loop` when the previous iteration ends
    /// with `should_continue = false` (no more messages to process).
    ///
    /// ## Non-destructive
    ///
    /// This method does NOT drain any messages. The actual consumption happens in
    /// `stages/receive.rs` via `drain_all`; `drain_for_receive` and `drain_for_end`
    /// remain available for external flush callers.
    ///
    /// ## Spurious wakeup guard
    ///
    /// After waking, we re-check `has_wake_up()`. If only Info messages arrived
    /// (which don't wake the loop), we go back to waiting. This prevents the executor
    /// from spinning on Info-only notifications.
    pub async fn await_wake(&self) {
        // Fast path: if already pending, return immediately
        if self.queue.has_wake_up() {
            return;
        }
        loop {
            self.wake.notified().await;
            // Guard against spurious wakeups: only wake on Prompt/Defer
            if self.queue.has_wake_up() {
                return;
            }
        }
    }

    /// Get a cloneable handle for producers.
    ///
    /// Producers (cron owner, channel owner, async router for bg_results, etc.)
    /// use this handle to push messages and wake the idle executor.
    pub fn handle(&self) -> InboxHandle {
        InboxHandle {
            queue: Arc::clone(&self.queue),
            wake: Arc::clone(&self.wake),
        }
    }

    /// Access the underlying MessageQueue (read-only reference).
    ///
    /// Used by stages that need to drain (e.g., `StageContext` construction).
    pub fn queue(&self) -> &MessageQueue {
        &self.queue
    }
}

impl std::fmt::Debug for SessionInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionInbox")
            .field("queue_len", &self.queue.len())
            .finish()
    }
}

/// Cloneable handle for pushing messages into the SessionInbox.
///
/// Producers (cron_owner, channel_owner, async_router for bg_results) hold this
/// handle to push messages and wake the idle executor. The handle is `Send + Sync`
/// and cheaply cloneable — safe to store in long-lived components.
///
/// TUI should NOT have access to this handle.
#[derive(Clone)]
pub struct InboxHandle {
    queue: Arc<MessageQueue>,
    wake: Arc<tokio::sync::Notify>,
}

impl InboxHandle {
    /// Push a Prompt message (user input or external request) and wake the executor.
    ///
    /// Prompt messages are consumed by `drain_all` during the Receive stage
    /// and wake the loop.
    pub fn push_prompt(&self, source: MessageSource, message: BaseMessage) {
        self.queue.push(QueuedMessage::prompt(source, message));
        self.wake.notify_one();
    }

    /// Push a Defer message (SubAgent complete, Cron trigger, bg result) and wake.
    ///
    /// In RCRA, Defer messages are consumed by `drain_all` during the Receive stage.
    /// They are also detectable via `drain_for_end` for external callers.
    pub fn push_defer(&self, source: MessageSource, message: BaseMessage) {
        self.queue.push(QueuedMessage::defer(source, message));
        self.wake.notify_one();
    }

    /// Push an Info message (system reminder, hook injection) — does NOT wake.
    ///
    /// Info messages are consumed by `drain_all` (in the loop) or `drain_for_receive`
    /// (external flush paths), but never wake the loop.
    /// They must be carried out by a Prompt message arriving later.
    pub fn push_info(&self, source: MessageSource, message: BaseMessage) {
        // Intentionally no wake.notify_one() — Info does not wake the loop
        self.queue.push(QueuedMessage::info(source, message));
    }

    /// Push an arbitrary QueuedMessage and conditionally wake.
    ///
    /// Wakes only if the message kind is Prompt or Defer (i.e., `kind.wakes_up()`).
    pub fn push(&self, msg: QueuedMessage) {
        let should_wake = msg.kind.wakes_up();
        self.queue.push(msg);
        if should_wake {
            self.wake.notify_one();
        }
    }

    /// Batch push messages; wakes once if any message is wake-able.
    pub fn push_batch(&self, msgs: Vec<QueuedMessage>) {
        if msgs.is_empty() {
            return;
        }
        let should_wake = msgs.iter().any(|m| m.kind.wakes_up());
        self.queue.push_batch(msgs);
        if should_wake {
            self.wake.notify_one();
        }
    }
}

impl std::fmt::Debug for InboxHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InboxHandle")
            .field("queue_len", &self.queue.len())
            .finish()
    }
}

// ─── CronOwner ───────────────────────────────────────────────────────────────

/// Agent-owned cron evaluation bridge。
///
/// Spawns a tokio task that receives trigger prompts from the channel and
/// pushes each prompt into the inbox as a Defer + `CronTrigger` source。
///
/// 循环依赖规避：本模块不 import `CronScheduler` / `CronTrigger`
/// （peri-middlewares 类型），只接收 `UnboundedReceiver<String>`——
/// 从 `CronTrigger.prompt` 到本通道的桥接在装配点（peri-acp host）完成。
pub struct CronOwner {
    /// Handle to the spawned trigger-forwarding task.
    /// `None` before [`start`](Self::start) is called.
    handle_task: Option<tokio::task::JoinHandle<()>>,
}

impl CronOwner {
    /// Create a new (not yet started) CronOwner.
    pub fn new() -> Self {
        Self { handle_task: None }
    }

    /// Spawn the trigger-forwarding loop.
    ///
    /// Receives prompt strings from `trigger_rx` and pushes each one into
    /// the inbox as `QueuedMessage::defer(MessageSource::CronTrigger, ...)`.
    ///
    /// The loop terminates when either:
    /// - `shutdown` is cancelled (session tear-down), or
    /// - `trigger_rx` is closed (scheduler dropped).
    ///
    /// # Parameters
    ///
    /// - `trigger_rx`: Unbounded receiver of prompt strings. Each received
    ///   string is the prompt from a fired `CronTrigger`.
    /// - `inbox`: Cloneable handle to the session inbox.
    /// - `shutdown`: Cancellation token tied to the session lifetime (Arc-shared clone).
    pub fn start(
        &mut self,
        mut trigger_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
        inbox: InboxHandle,
        shutdown: Arc<tokio_util::sync::CancellationToken>,
    ) {
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => {
                        tracing::debug!("cron_owner: shutdown signal received, stopping");
                        break;
                    }
                    prompt = trigger_rx.recv() => {
                        match prompt {
                            Some(prompt) => {
                                let message = BaseMessage::human(
                                    crate::messages::MessageContent::text(format!(
                                        "<goal-message>Cron triggered: {}</goal-message>",
                                        prompt
                                    )),
                                );
                                inbox.push(QueuedMessage::defer(
                                    MessageSource::CronTrigger,
                                    message,
                                ));
                                tracing::debug!(prompt = %prompt, "cron_owner: trigger pushed to inbox");
                            }
                            None => {
                                // trigger_rx closed (scheduler dropped)
                                tracing::debug!("cron_owner: trigger_rx closed, stopping");
                                break;
                            }
                        }
                    }
                }
            }
        });
        self.handle_task = Some(handle);
    }

    /// Abort the background task if running.
    ///
    /// Called during session tear-down to ensure clean shutdown even if the
    /// cancellation token has not yet fired.
    pub fn shutdown(&mut self) {
        if let Some(handle) = self.handle_task.take() {
            handle.abort();
        }
    }
}

impl Default for CronOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CronOwner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl std::fmt::Debug for CronOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CronOwner")
            .field("running", &self.handle_task.is_some())
            .finish()
    }
}

// ─── AgentRuntime（注册表条目 + cancel 判定） ────────────────────────────────

/// 运行时 agent 实例（子 agent 取消判定与终止执行的载体）。
///
/// cancel 最终执行权归 Agent 层（§2/§9）：本类型是跨层注册表条目契约
/// （ACP `AcpSession.active_agents` 持有），判定函数为纯函数、无层依赖。
pub struct AgentRuntime {
    pub thread_id: ThreadId,
    pub cancel_token: tokio_util::sync::CancellationToken,
    pub cancel_policy: CancelPolicy,
    pub status: crate::thread::AgentStatus,
}

impl AgentRuntime {
    pub fn new(thread_id: ThreadId, cancel_policy: CancelPolicy) -> Self {
        Self {
            thread_id,
            cancel_token: tokio_util::sync::CancellationToken::new(),
            cancel_policy,
            status: crate::thread::AgentStatus::Active,
        }
    }
}

/// cancel 判定（Cascade/Independent）与终止执行：取消所有 Cascade policy 的
/// 同步子 agent（跟随父 agent 取消）。Independent（bg）子 agent 不受影响，
/// 仅跟随 session 根取消。
pub fn cancel_cascade_agents<'a>(runtimes: impl IntoIterator<Item = &'a AgentRuntime>) {
    for runtime in runtimes {
        if runtime.cancel_policy == CancelPolicy::Cascade {
            runtime.cancel_token.cancel();
        }
    }
}

/// 取消所有 agent（session 结束 / close_session 时）。
pub fn cancel_all_agents<'a>(runtimes: impl IntoIterator<Item = &'a AgentRuntime>) {
    for runtime in runtimes {
        runtime.cancel_token.cancel();
    }
}

/// 便捷入口：按 `thread_id -> AgentRuntime` 注册表执行 cascade 判定。
pub fn cancel_cascade_in<'a>(
    runtimes: impl IntoIterator<Item = &'a HashMap<ThreadId, AgentRuntime>>,
) {
    for map in runtimes {
        cancel_cascade_agents(map.values());
    }
}

/// 便捷入口：按 `thread_id -> AgentRuntime` 注册表取消全部。
pub fn cancel_all_in<'a>(runtimes: impl IntoIterator<Item = &'a HashMap<ThreadId, AgentRuntime>>) {
    for map in runtimes {
        cancel_all_agents(map.values());
    }
}

// ─── SessionAccessPort（L5：executor 对 ACP SessionManager 的访问端口）──────

/// L5：`run_session_loop` 会话编排对 ACP `SessionManager` 的依赖端口。
///
/// 依赖反转（§0）：executor 迁入 peri-agent 后不再引用 ACP `SessionManager`
/// 类型，改为经本端口访问会话级状态（v2 MessageQueue / inbox / task manager /
/// permission-mode 记账 / goal / 子 agent 注册表 / cron bridge）。
/// ACP 侧 `SessionManager` 实现本端口；print mode / 测试等无 session 场景
/// 为 `None`（调用方保持原 None 语义，仅读路径可用时生效）。
pub trait SessionAccessPort: Send + Sync {
    /// 会话级共享 v2 MessageQueue（`AcpSession.v2_message_queue`）。
    /// 返回 clone（内部 Arc 共享，语义同 `SessionManager::v2_queue_for`）。
    fn v2_message_queue(&self, session_id: &str) -> Option<MessageQueue>;

    /// 会话级 SessionInbox（await-wake wrapper；lazy-init 语义由实现方保证）。
    fn session_inbox(&self, session_id: &str) -> Option<Arc<SessionInbox>>;

    /// 会话级 idle-suspended 标志（共享 Arc，executor 在 await_wake 挂起期间
    /// 置 true、醒来/取消时复位）。
    ///
    /// 宿主 `dispatch_prompt_turn` 读取此标志决定"注入 vs 排队"：turn 挂起时
    /// 用户新 prompt 直接注入 inbox（Prompt + wake）让挂起的 loop 立即醒来，
    /// 而不是在 per-session prompt lock 上阻塞至当前 turn 完成（bg 任务可能
    /// 长达数分钟，阻塞会让用户输入"石沉大海"）。
    fn idle_suspended_flag(&self, session_id: &str) -> Option<Arc<AtomicBool>>;

    /// 会话级后台任务管理器（`AcpSession.task_manager`）。
    fn task_manager(&self, session_id: &str) -> Option<Arc<dyn crate::tasks::TaskManager>>;

    /// 最近一次已通知模型的 PermissionMode（D2 记账值，跨 turn 持久）。
    fn last_notified_permission_mode(&self, session_id: &str) -> Option<Arc<AtomicU8>>;

    /// 会话级 GoalController（`AcpSession.goal_state`）。
    fn goal_controller(&self, session_id: &str) -> Option<Arc<dyn crate::goal::GoalController>>;

    /// 构造子 agent runtime 注册闭包（`AcpSession.active_agents` insert）。
    /// 返回 None 表示无注册能力（print mode / session 不存在）。
    fn register_runtime(&self, session_id: &str) -> Option<crate::frozen::RegisterRuntimeFn>;

    /// 构造子 agent runtime 注销闭包（`AcpSession.active_agents` remove）。
    fn deregister_runtime(&self, session_id: &str) -> Option<crate::frozen::DeregisterRuntimeFn>;

    /// cancel cascade 子 agent（Cascade 判定归 Agent 层契约，本端口仅定位）。
    fn cancel_cascade_children(&self, session_id: &str);

    /// 确保 session 级 cron bridge 已启动（lazy-init，幂等；见
    /// `SessionManager::cron_bridge_for`）。
    fn cron_bridge_for(&self, session_id: &str) -> bool;
}
