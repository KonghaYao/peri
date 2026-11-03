//! Queue payload、独立消息策略与有序 Receive 接纳。
//!
//! [`MessageQueue`] 是持久 MQ 的门面：所有事实操作经 [`ThreadMqStore`] 落到存储层，
//! 内存只保留唤醒信号、运行期发送选择与通知版本。破坏性内存 API
//! （`push` / `drain_all` / `drain_batch` / `suppress` / `clear_batch` / `withdraw`）
//! 已移除——待发事实的唯一来源是 MQ 行（`docs/design/thread-mq.md` §4.1）。

use super::mq::{
    MqCancelOutcome, MqEnqueueDisposition, MqEnqueueOutcome, MqEntry, MqFilter, MqInput,
    MqReceiveDisposition, MqReceiveItem, MqReceiveOutcome, MqReceivePurpose, MqReceiveSelection,
    MqReceiveStatus, MqSendOutcome, MqWatermark, SessionResourcesMqStore, ThreadMqStore,
};
use super::{ExecutionBinding, MessageDisposition, MessagePolicy};
use crate::messages::{BaseMessage, MessageId};
use crate::session_resources::SessionResources;
use crate::store::PersistedPayload;
use crate::system_reminder::TrustedSystemReminder;
use crate::thread::ThreadId;
use anyhow::Result;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 单次 Receive 的上限；与旧 `drain_batch` 的批次上限一致。
const RECEIVE_BATCH_LIMIT: usize = 64;

/// [`MessageQueue::new`] 的内存 thread 标识；不参与生产路由。
const IN_MEMORY_THREAD_ID: &str = "in-memory";

// ─── MessageKind ─────────────────────────────────────────────────────────────

/// 消息展示标签；运行调度以 MessagePolicy 为准。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// 外部主动请求 — Receive 消费，循环结束后到达同样激活
    Prompt,
    /// 延迟到达的结果 — Receive 消费，循环结束后到达同样激活
    Defer,
    /// 通知性数据 — Receive 消费，永不唤醒循环
    Info,
}

impl MessageKind {
    /// 此展示标签的默认策略；显式策略可覆盖，不能用于运行调度。
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
    /// Todo steering（requireCompletion 续跑提醒）
    TodoSteering,
    /// Cron 定时触发
    CronTrigger,
    /// Stop hook feedback
    StopHookFeedback,
    /// Channel 消息（微信/Slack 等）
    ChannelMessage,
    /// MCP subscription or lifecycle notification.
    #[serde(alias = "dynamic_mcp_notification")]
    McpNotification,
    /// Hook 系统注入
    SystemInjected,
    /// Hook 的显式停止意图（continue:false）：经 Receive 唯一出口停止当前 run，
    /// 不按可唤醒消息处理，也不发起额外模型请求。
    HookStopIntent,
    /// 工具失败警告
    ToolFailureWarning,
    /// 工作流完成
    WorkflowComplete,
    /// 进入会话时的恢复提醒（进程中断后从已提交历史继续）。
    ///
    /// 只有 Host 恢复协调以可信 SystemReminder 构造；旧行没有该来源，反序列化不受影响。
    SessionRecovery,
}

// ─── QueuedMessage ───────────────────────────────────────────────────────────

/// Queue payload. Scheduling (`MessageKind`) is deliberately orthogonal to content semantics.
#[derive(Debug, Clone)]
pub enum QueuedPayload {
    Message(BaseMessage),
    SystemReminder(TrustedSystemReminder),
}

/// 一条待投递的消息（v2 富类型）
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    /// 消息 Kind（决定默认策略）
    pub kind: MessageKind,
    /// 消息来源
    pub source: MessageSource,
    /// 实际消息内容
    pub payload: QueuedPayload,
    /// Stable canonical ID for an owner-confirmed terminal reminder.
    pub delivery_id: Option<MessageId>,
    pub policy: MessagePolicy,
    /// 旧内存队列的接纳序号；MQ 用 `(enqueued_at, id)` 定序，本字段不再参与排序。
    pub admission_sequence: Option<u64>,
    /// 用户输入的原始编辑器文本；其他来源为 `None`。
    pub original_draft: Option<String>,
    /// 存储生成的入队时间；只有从 MQ 读回的条目有值。
    pub enqueued_at: Option<String>,
}

impl QueuedMessage {
    pub fn new(kind: MessageKind, source: MessageSource, message: BaseMessage) -> Self {
        Self::with_payload(kind, source, QueuedPayload::Message(message))
    }

    pub fn with_payload(kind: MessageKind, source: MessageSource, payload: QueuedPayload) -> Self {
        let mut policy = match kind {
            MessageKind::Prompt | MessageKind::Defer => MessagePolicy::ensure_processing(),
            MessageKind::Info => MessagePolicy::passive(),
        };
        if let QueuedPayload::SystemReminder(reminder) = &payload {
            policy.model_visible = reminder
                .as_reminder()
                .audiences
                .0
                .contains(&crate::system_reminder::ReminderAudience::Model);
        }
        Self {
            kind,
            source,
            payload,
            delivery_id: None,
            policy,
            admission_sequence: None,
            original_draft: None,
            enqueued_at: None,
        }
    }

    pub fn with_policy(mut self, mut policy: MessagePolicy) -> Self {
        if let QueuedPayload::SystemReminder(reminder) = &self.payload {
            policy.model_visible &= reminder
                .as_reminder()
                .audiences
                .0
                .contains(&crate::system_reminder::ReminderAudience::Model);
        }
        self.policy = policy;
        self
    }

    /// 附带用户输入的原始编辑器文本（只有待发送区的用户输入需要）。
    pub fn with_original_draft(mut self, draft: impl Into<String>) -> Self {
        self.original_draft = Some(draft.into());
        self
    }

    /// 附带入队时间；仅用于从 MQ 行构造视图，入队请求不接受调用方指定时间。
    pub fn with_enqueued_at(mut self, enqueued_at: impl Into<String>) -> Self {
        self.enqueued_at = Some(enqueued_at.into());
        self
    }

    pub fn system_reminder_with_delivery_id(
        kind: MessageKind,
        source: MessageSource,
        reminder: TrustedSystemReminder,
        delivery_id: MessageId,
    ) -> Self {
        let mut message = Self::system_reminder(kind, source, reminder);
        message.delivery_id = Some(delivery_id);
        message
    }

    pub fn system_reminder(
        kind: MessageKind,
        source: MessageSource,
        reminder: TrustedSystemReminder,
    ) -> Self {
        Self::with_payload(kind, source, QueuedPayload::SystemReminder(reminder))
    }

    pub fn message(&self) -> Option<&BaseMessage> {
        match &self.payload {
            QueuedPayload::Message(message) => Some(message),
            QueuedPayload::SystemReminder(_) => None,
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

    /// 稳定投递身份：`Message` 用载荷 id，`SystemReminder` 用已固定的 `delivery_id`。
    ///
    /// 未固定的提醒每次调用都会得到新 id——需要稳定身份时用 [`MessageQueue::enqueue`]
    /// 返回的 `outcome.entry.id`（入队时只固定一次）。
    pub fn id(&self) -> MessageId {
        match &self.payload {
            QueuedPayload::Message(message) => message.id(),
            QueuedPayload::SystemReminder(_) => self.delivery_id.unwrap_or_default(),
        }
    }

    /// 转为入队请求；`SystemReminder` 的 `delivery_id` 在这里固定一次，行 id 与载荷 id 一致。
    pub fn into_mq_input(mut self) -> MqInput {
        let payload = match self.payload {
            QueuedPayload::Message(message) => PersistedPayload::Message(message),
            QueuedPayload::SystemReminder(reminder) => {
                let id = self.delivery_id.unwrap_or_default();
                self.delivery_id = Some(id);
                PersistedPayload::SystemReminder { id, reminder }
            }
        };
        MqInput {
            id: payload.id(),
            kind: self.kind,
            source: self.source,
            payload,
            policy: self.policy,
            original_draft: self.original_draft,
        }
    }

    /// MQ 行 → 队列视图：策略以存储行为准，不按 kind 重算。
    pub fn from_mq_entry(entry: MqEntry) -> Self {
        let MqEntry {
            id: _,
            enqueued_at,
            kind,
            source,
            payload,
            policy,
            original_draft,
        } = entry;
        let (payload, delivery_id) = match payload {
            PersistedPayload::Message(message) => (QueuedPayload::Message(message), None),
            PersistedPayload::SystemReminder { id, reminder } => {
                (QueuedPayload::SystemReminder(reminder), Some(id))
            }
        };
        Self {
            kind,
            source,
            payload,
            delivery_id,
            policy,
            admission_sequence: None,
            original_draft,
            enqueued_at: Some(enqueued_at),
        }
    }
}

// ─── MessageQueue ────────────────────────────────────────────────────────────

/// 会话级持久收件箱门面（绑定一个 thread）。
///
/// 待发事实（有哪些消息、顺序、策略、移交与取消）全部由 [`ThreadMqStore`] 回答；
/// 本类型在内存里只保留唤醒信号、运行期发送选择与通知版本。唤醒只提示重查，
/// 不能当作消息存在性或消费资格的依据。
///
/// 生产必须用 [`bind`](Self::bind)；[`new`](Self::new) 使用内存后端，仅供测试与无持久化
/// 的嵌入场景。
#[derive(Clone)]
pub struct MessageQueue {
    inner: Arc<Mailbox>,
}

struct Mailbox {
    store: Arc<dyn ThreadMqStore>,
    thread_id: ThreadId,
    wake: tokio::sync::Notify,
    wake_version: tokio::sync::watch::Sender<u64>,
    send_selection: Mutex<Vec<MessageId>>,
    /// 已受理但尚未提交的分离入队数量（同步 hook 桥，见 [`MessageQueue::begin_detached_enqueue`]）。
    pending_enqueues: AtomicUsize,
    pending_drained: tokio::sync::Notify,
    /// 仅测试：记录被 `suppress_sync` 处置的消息，供旧断言读取。
    #[cfg(feature = "test-support")]
    suppressed: Mutex<Vec<QueuedMessage>>,
}

/// 一次分离入队的在途凭证；Drop 即宣告该写入结束（见 [`MessageQueue::begin_detached_enqueue`]）。
pub struct DetachedEnqueue {
    inner: Arc<Mailbox>,
}

impl Drop for DetachedEnqueue {
    fn drop(&mut self) {
        if self.inner.pending_enqueues.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.inner.pending_drained.notify_waiters();
        }
    }
}

impl Default for MessageQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MessageQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageQueue")
            .field("thread_id", &self.inner.thread_id)
            .field("wake_version", &*self.inner.wake_version.borrow())
            .field("send_selection", &self.inner.send_selection.lock().len())
            .finish()
    }
}

impl MessageQueue {
    /// 仅测试/嵌入：内存 MQ 后端 + 固定 thread（见 [`bind`](Self::bind)）。
    pub fn new() -> Self {
        Self::from_store(
            Arc::new(super::mq_memory::InMemoryMqStore::new()),
            IN_MEMORY_THREAD_ID.to_owned(),
        )
    }

    /// 生产入口：把门面绑定到真实资源与 thread。
    pub fn bind(resources: Arc<dyn SessionResources>, thread_id: ThreadId) -> Self {
        Self::from_store(Arc::new(SessionResourcesMqStore::new(resources)), thread_id)
    }

    fn from_store(store: Arc<dyn ThreadMqStore>, thread_id: ThreadId) -> Self {
        Self {
            inner: Arc::new(Mailbox {
                store,
                thread_id,
                wake: tokio::sync::Notify::new(),
                wake_version: tokio::sync::watch::channel(0).0,
                send_selection: Mutex::new(Vec::new()),
                pending_enqueues: AtomicUsize::new(0),
                pending_drained: tokio::sync::Notify::new(),
                #[cfg(feature = "test-support")]
                suppressed: Mutex::new(Vec::new()),
            }),
        }
    }

    /// 本门面绑定的 thread。
    pub fn thread_id(&self) -> &ThreadId {
        &self.inner.thread_id
    }

    /// 登记一次「已受理、尚未提交」的分离入队。
    ///
    /// 同步 middleware hook 无法就地 await 持久提交，只能把写入投递到 runtime；
    /// 调用方在下一次 [`Self::await_pending_enqueues`] 之前不得据此断定收件箱为空。
    /// 返回的 guard 在写入结束（成功或失败）时 Drop，才把入队计为已完成。
    pub fn begin_detached_enqueue(&self) -> DetachedEnqueue {
        self.inner.pending_enqueues.fetch_add(1, Ordering::AcqRel);
        DetachedEnqueue {
            inner: Arc::clone(&self.inner),
        }
    }

    /// 等待所有已受理的分离入队提交完成。
    ///
    /// Receive 在选择前缀前必须调用：否则本轮选择会漏掉刚由 hook 投递的消息，
    /// 循环据此判定空闲并退出。
    pub async fn await_pending_enqueues(&self) {
        loop {
            let notified = self.inner.pending_drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.inner.pending_enqueues.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }

    // ── 事实操作 ──

    /// 入队一条消息并返回接纳结论；只有可唤醒策略（非 Passive）才发布唤醒。
    ///
    /// 调用方需要稳定 id 时用返回的 `outcome.entry.id`：未固定的 `SystemReminder`
    /// `delivery_id` 在本次入队时生成，重试必须复用它。
    pub async fn enqueue(&self, message: QueuedMessage) -> Result<MqEnqueueOutcome> {
        let notifies = message.policy.notifies_execution();
        let outcome = self
            .inner
            .store
            .mq_enqueue(&self.inner.thread_id, message.into_mq_input())
            .await?;
        if notifies && outcome.disposition != MqEnqueueDisposition::AlreadyHandedOff {
            self.notify_wake();
        }
        Ok(outcome)
    }

    /// 入队并立即放行：非交互入口没有待发送区，用户输入必须在同一步进入发送选择。
    ///
    /// 用户待发输入走 mailbox `enqueue` + `dispatch`，不能使用本方法绕过准入。
    pub async fn enqueue_sent(&self, message: QueuedMessage) -> Result<MqEnqueueOutcome> {
        let id = message.id();
        let outcome = self.enqueue(message).await?;
        if outcome.disposition != MqEnqueueDisposition::AlreadyHandedOff {
            self.send_inputs(vec![id]).await?;
        }
        Ok(outcome)
    }

    /// 同步能力面的入队：仅当存储声明无需 I/O 时才就地完成，否则返回 `None`。
    ///
    /// 同步 hook 无法 await；内存存储可以立即提交，调用方据此保持"返回即已存在"
    /// 的可见性，真实存储仍由调用方投递到 runtime。
    #[doc(hidden)]
    pub fn enqueue_now(&self, message: QueuedMessage) -> Option<Result<MqEnqueueOutcome>> {
        self.inner
            .store
            .supports_immediate_enqueue()
            .then(|| futures::executor::block_on(self.enqueue(message)))
    }

    /// 批量入队（逐条提交）；任一失败立即返回，调用方按稳定 ID 重查已提交的部分。
    pub async fn enqueue_batch(
        &self,
        messages: Vec<QueuedMessage>,
    ) -> Result<Vec<MqEnqueueOutcome>> {
        let mut outcomes = Vec::with_capacity(messages.len());
        let mut wake = false;
        for message in messages {
            let notifies = message.policy.notifies_execution();
            let outcome = self
                .inner
                .store
                .mq_enqueue(&self.inner.thread_id, message.into_mq_input())
                .await?;
            if notifies && outcome.disposition != MqEnqueueDisposition::AlreadyHandedOff {
                wake = true;
            }
            outcomes.push(outcome);
        }
        if wake {
            self.notify_wake();
        }
        Ok(outcomes)
    }

    /// 按投影用途读取本 thread 的 MQ 行；只读，不消费。
    pub async fn query(&self, filter: MqFilter) -> Result<Vec<MqEntry>> {
        Ok(self
            .inner
            .store
            .mq_query(&self.inner.thread_id, filter)
            .await?)
    }

    /// 全部 MQ 行转成队列视图（非破坏性冻结引用）。
    pub async fn snapshot(&self) -> Result<Vec<QueuedMessage>> {
        Ok(self
            .query(MqFilter::default())
            .await?
            .into_iter()
            .map(QueuedMessage::from_mq_entry)
            .collect())
    }

    /// 取消一条仍在 MQ 的用户输入，返回整行原稿。
    pub async fn cancel_user_input(&self, id: &MessageId) -> Result<MqCancelOutcome> {
        Ok(self
            .inner
            .store
            .mq_cancel(&self.inner.thread_id, *id)
            .await?)
    }

    /// 提交运行期发送选择：只接受仍在 MQ 的用户输入，不写历史（`§5.3`）。
    ///
    /// 未命中的 id（已移交或不存在）如实进入 `missing`；被接受的消息仍是待发事实，
    /// 直到下一次 Receive 移交成功。
    pub async fn send_inputs(&self, ids: Vec<MessageId>) -> Result<MqSendOutcome> {
        let entries = self
            .query(MqFilter {
                user_input_only: true,
                ..Default::default()
            })
            .await?;
        let mut accepted = Vec::new();
        let mut missing = Vec::new();
        {
            let mut selection = self.inner.send_selection.lock();
            for id in ids {
                match entries.iter().find(|entry| entry.id == id) {
                    Some(entry) => {
                        if !selection.contains(&id) {
                            selection.push(id);
                        }
                        if !accepted.iter().any(|item: &MqEntry| item.id == id) {
                            accepted.push(entry.clone());
                        }
                    }
                    None => missing.push(id),
                }
            }
        }
        if !accepted.is_empty() {
            self.notify_wake();
        }
        Ok(MqSendOutcome { accepted, missing })
    }

    /// 取走本次运行期发送选择（仅内存，不影响 MQ 行）。
    pub fn take_send_selection(&self) -> Vec<MessageId> {
        std::mem::take(&mut *self.inner.send_selection.lock())
    }

    /// 撤回指定的运行期发送选择；MQ 行保持不变。
    ///
    /// Stop/失败收尾恢复 `Queued` 时使用：选择已被取消，旧执行不能借此消费。
    pub fn withdraw_send_inputs(&self, ids: &[MessageId]) {
        self.inner
            .send_selection
            .lock()
            .retain(|id| !ids.contains(id));
    }

    /// 生成本次 Receive 的选择：非用户输入全收，用户输入只收 `send_selection` 选中的。
    ///
    /// 顺序固定为 `(enqueued_at, id)`；`Suppressed` 策略与空 Prompt 唤醒信号
    /// 标记为只删不写历史。最多 [`RECEIVE_BATCH_LIMIT`] 条，且不消费发送选择。
    pub async fn select_for_receive(&self, permit: &ReceivePermit) -> Result<Vec<MqReceiveItem>> {
        let entries = self.query(MqFilter::default()).await?;
        let selection = self.inner.send_selection.lock().clone();
        let mut items = Vec::new();
        for entry in entries {
            if !is_eligible_for_receive(&entry, &selection) {
                continue;
            }
            let disposition = match entry.policy.disposition(&permit.execution) {
                MessageDisposition::Suppressed => MqReceiveDisposition::Suppress,
                // 空 Prompt（keepgoing）只是唤醒信号：行必须消费，但不能制造空用户气泡。
                _ if is_wake_only(&entry) => MqReceiveDisposition::Suppress,
                MessageDisposition::Process | MessageDisposition::ProjectOnly => {
                    MqReceiveDisposition::Commit
                }
            };
            items.push(MqReceiveItem { entry, disposition });
            if items.len() >= RECEIVE_BATCH_LIMIT {
                break;
            }
        }
        Ok(items)
    }

    /// 原子移交：成功后结清发送选择中已结算的 id，仍有可处理项时补一次唤醒。
    ///
    /// 失败时选择保持原样（未确认移交前不能假装已发送）。
    pub async fn receive(
        &self,
        permit: ReceivePermit,
        items: Vec<MqReceiveItem>,
    ) -> Result<MqReceiveOutcome> {
        if items.is_empty() {
            return Ok(MqReceiveOutcome { items: Vec::new() });
        }
        let outcome = self
            .inner
            .store
            .mq_receive(&self.inner.thread_id, permit.into_selection(items))
            .await?;
        #[cfg(feature = "test-support")]
        for item in &outcome.items {
            // Conflict 与 Suppressed 一样是「行已删除、未写历史」的终态处置，
            // 这里保留同一份可观察诊断，供旧断言读取。
            if matches!(
                item.status,
                MqReceiveStatus::Suppressed | MqReceiveStatus::Conflict
            ) {
                if let Some(entry) = &item.entry {
                    self.inner
                        .suppressed
                        .lock()
                        .push(QueuedMessage::from_mq_entry(entry.clone()));
                }
            }
        }
        let settled = outcome
            .items
            .iter()
            .filter(|item| item.status != MqReceiveStatus::Missing)
            .map(|item| item.id)
            .collect::<Vec<_>>();
        if !settled.is_empty() {
            self.inner
                .send_selection
                .lock()
                .retain(|id| !settled.contains(id));
        }
        if self.has_processable_work().await? {
            self.notify_wake();
        }
        Ok(outcome)
    }

    /// 取一次 Receive 许可：仅 Agent Loop 与同 thread 生命周期 owner 调用。
    ///
    /// 许可不可 Clone，`receive` 会消费它，防止重放同一次准入。
    #[doc(hidden)]
    pub fn begin_receive(
        &self,
        purpose: MqReceivePurpose,
        execution: ExecutionBinding,
    ) -> ReceivePermit {
        ReceivePermit { execution, purpose }
    }

    // ── 策略查询（按 MQ 事实判定） ──
    //
    // 这些判定与 [`select_for_receive`](Self::select_for_receive) 共用
    // [`is_eligible_for_receive`]：未发送的用户输入留在待发送区，不能让循环
    // 认为「有可消费工作」而空转。

    pub async fn has_ensure_processing(&self) -> Result<bool> {
        let entries = self.query(MqFilter::default()).await?;
        let selection = self.inner.send_selection.lock().clone();
        Ok(entries.iter().any(|entry| {
            is_eligible_for_receive(entry, &selection) && entry.policy.ensures_processing()
        }))
    }

    pub async fn has_required_for_run(&self, execution: &ExecutionBinding) -> Result<bool> {
        let entries = self.query(MqFilter::default()).await?;
        let selection = self.inner.send_selection.lock().clone();
        Ok(entries.iter().any(|entry| {
            is_eligible_for_receive(entry, &selection)
                && entry.policy.disposition(execution) == MessageDisposition::Process
        }))
    }

    pub async fn has_required(&self) -> Result<bool> {
        Ok(self
            .query(MqFilter::default())
            .await?
            .iter()
            .any(|entry| entry.policy.requirement == super::MessageRequirement::Required))
    }

    /// 按来源查询 required 消息，仅用于诊断；不授予来源调度权。
    pub async fn has_pending_defer(&self, source: &MessageSource) -> Result<bool> {
        Ok(self
            .query(MqFilter::default())
            .await?
            .iter()
            .any(|entry| entry.policy.ensures_processing() && &entry.source == source))
    }

    /// 是否仍需建立执行处理 required MQ 消息。
    pub async fn needs_mq_continuation(&self) -> Result<bool> {
        self.has_ensure_processing().await
    }

    pub async fn has_wake_up(&self) -> Result<bool> {
        self.has_ensure_processing().await
    }

    /// 队列是否为空。
    pub async fn is_empty(&self) -> Result<bool> {
        Ok(self.query(MqFilter::default()).await?.is_empty())
    }

    /// 队列长度。
    pub async fn len(&self) -> Result<usize> {
        Ok(self.query(MqFilter::default()).await?.len())
    }

    /// 当前 MQ 的最大 `(enqueued_at, id)`；空队列返回 `None`。
    pub async fn admission_watermark(&self) -> Result<Option<MqWatermark>> {
        Ok(self
            .query(MqFilter::default())
            .await?
            .last()
            .map(MqEntry::watermark))
    }

    /// 是否在水位之后有需要模型处理的入队项；`None` 表示检查全部。
    pub async fn has_ensure_processing_after(
        &self,
        watermark: &Option<MqWatermark>,
    ) -> Result<bool> {
        let filter = MqFilter {
            after: watermark.clone(),
            ..Default::default()
        };
        let entries = self.query(filter).await?;
        let selection = self.inner.send_selection.lock().clone();
        Ok(entries.iter().any(|entry| {
            is_eligible_for_receive(entry, &selection) && entry.policy.ensures_processing()
        }))
    }

    /// 清空本 thread 的全部 MQ 行：只给 rewind 与 thread 生命周期调用。
    ///
    /// 常规消费必须走 [`receive`](Self::receive)；本方法会丢弃尚未处理的待发事实。
    pub async fn clear(&self) -> Result<()> {
        self.inner.store.mq_clear(&self.inner.thread_id).await?;
        self.inner.send_selection.lock().clear();
        Ok(())
    }

    // ── 唤醒 ──

    pub fn subscribe_wake(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.wake_version.subscribe()
    }

    /// 非破坏性等待可执行消息：先注册通知再检查 MQ，避免丢唤醒。
    ///
    /// 查询失败时直接返回（不声称有工作），由调用方在重查时暴露错误——既不能把失败
    /// 当成「没有消息」永久挂起，也不能伪造唤醒依据。
    pub async fn await_wake(&self) {
        loop {
            let notified = self.inner.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.has_wake_up().await {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        thread_id = %self.inner.thread_id,
                        %error,
                        "MQ wake check failed; returning for caller re-check"
                    );
                    return;
                }
            }
            notified.await;
        }
    }

    /// 只等待绑定到给定执行的 required 消息。
    pub async fn await_wake_for_run(&self, execution: &ExecutionBinding) {
        loop {
            let notified = self.inner.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.has_required_for_run(execution).await {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        thread_id = %self.inner.thread_id,
                        %error,
                        "MQ run wake check failed; returning for caller re-check"
                    );
                    return;
                }
            }
            notified.await;
        }
    }

    fn notify_wake(&self) {
        self.inner.wake.notify_waiters();
        self.inner
            .wake_version
            .send_modify(|version| *version = version.saturating_add(1));
    }

    /// 移交后是否仍有本轮可继续消费的工作；调用方据此补一次唤醒。
    async fn has_processable_work(&self) -> Result<bool> {
        self.has_ensure_processing().await
    }
}

// ─── ReceivePermit ───────────────────────────────────────────────────────────

/// 行是否可能被本轮 Receive 取用。
///
/// 未发送的用户输入留在待发送区：即使策略要求处理，也要 `send_inputs` 放行后
/// 才参与本轮消费。所有「是否还有本轮工作」的判定都必须与 Receive 的选择一致，
/// 否则会出现「判定有工作但取不出」的空转。
fn is_eligible_for_receive(entry: &MqEntry, selection: &[MessageId]) -> bool {
    entry.source != MessageSource::UserInput || selection.contains(&entry.id)
}

/// 空内容 Prompt 是 keepgoing 等控制信号的唤醒载体，消费后不得写入历史。
fn is_wake_only(entry: &MqEntry) -> bool {
    entry.kind == MessageKind::Prompt
        && matches!(
            &entry.payload,
            PersistedPayload::Message(BaseMessage::Human { content, .. }) if content.is_empty()
        )
}

/// 一次 Receive 的运行期许可。
///
/// 字段私有、不实现 `Clone`：只有 Agent Loop 或同 thread 生命周期 owner 能经
/// [`MessageQueue::begin_receive`] 取得，[`MessageQueue::receive`] 消费它。
#[derive(Debug)]
pub struct ReceivePermit {
    execution: ExecutionBinding,
    purpose: MqReceivePurpose,
}

impl ReceivePermit {
    fn into_selection(self, items: Vec<MqReceiveItem>) -> MqReceiveSelection {
        MqReceiveSelection {
            items,
            purpose: self.purpose,
            execution: self.execution,
        }
    }
}

// ─── 测试同步适配（仅 feature = "test-support"） ─────────────────────────────
//
// 旧单元测试以同步断言描述队列行为。这里用同一个持久门面，不复制事实源、不引入
// 第二份内存权威；生产构建不包含这些方法。
//
// 同步桥在 tokio 运行时内只在首次 poll 即可完成时返回：阻塞 runtime 线程会让同
// runtime 上持有写入屏障的任务永远得不到调度，从挂起变成死锁；这类调用必须改用
// 异步 API。确需同步等待的回调（如 mock server）应换独立 runtime 线程执行。

#[cfg(feature = "test-support")]
fn ready_only<T>(future: impl std::future::Future<Output = T>) -> T {
    if tokio::runtime::Handle::try_current().is_err() {
        return futures::executor::block_on(future);
    }
    use futures::FutureExt;
    let mut future = Box::pin(future);
    match future.as_mut().now_or_never() {
        Some(value) => value,
        None => panic!(
            "test MQ sync API cannot await inside a tokio runtime; use the async API instead"
        ),
    }
}

#[cfg(feature = "test-support")]
impl MessageQueue {
    fn test_permit(&self) -> ReceivePermit {
        self.begin_receive(
            MqReceivePurpose::NonModelSettlement,
            super::ExecutionBinding {
                turn_id: super::TurnId::new(),
                attempt_id: crate::identity::AttemptId::new(),
            },
        )
    }

    /// 仅测试：下一次原子移交在写入前暂停；返回（进入通知，放行信号）。
    pub fn gate_next_receive(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        self.inner
            .store
            .install_receive_gate((entered_tx, release_rx));
        (entered_rx, release_tx)
    }

    pub fn push_sync(&self, message: QueuedMessage) {
        ready_only(self.push_async(message));
    }

    pub fn push_batch_sync(&self, messages: Vec<QueuedMessage>) {
        ready_only(self.push_batch_async(messages));
    }

    /// 仅测试：`push_sync` 的异步等价，可在 tokio 运行时内安全 await。
    pub async fn push_async(&self, message: QueuedMessage) {
        let id = message.id();
        let is_user_input = message.source == MessageSource::UserInput;
        self.enqueue(message).await.expect("test MQ enqueue");
        if is_user_input {
            // 旧测试的 `push` 语义是“立即可被 Receive 消费”；显式写入选择，
            // 与 `send_inputs` 的筛选保持一致。
            let mut selection = self.inner.send_selection.lock();
            if !selection.contains(&id) {
                selection.push(id);
            }
        }
    }

    pub async fn push_batch_async(&self, messages: Vec<QueuedMessage>) {
        for message in messages {
            self.push_async(message).await;
        }
    }

    pub fn enqueue_batch_sync(&self, messages: Vec<QueuedMessage>) -> Vec<MqEnqueueOutcome> {
        messages
            .into_iter()
            .map(|message| ready_only(self.enqueue(message)).expect("test MQ enqueue"))
            .collect()
    }

    pub fn drain_all_sync(&self) -> Vec<QueuedMessage> {
        ready_only(self.drain_all_async())
    }

    pub fn drain_batch_sync(&self, limit: usize) -> Vec<QueuedMessage> {
        ready_only(self.drain_batch_async(limit))
    }

    pub async fn drain_all_async(&self) -> Vec<QueuedMessage> {
        self.drain_batch_async(usize::MAX).await
    }

    /// 仅测试：`drain_batch_sync` 的异步等价，可在 tokio 运行时内安全 await。
    pub async fn drain_batch_async(&self, limit: usize) -> Vec<QueuedMessage> {
        let selection = self.inner.send_selection.lock().clone();
        let entries = self
            .query(MqFilter::default())
            .await
            .expect("test MQ query");
        let items = entries
            .into_iter()
            .filter(|entry| {
                entry.source != MessageSource::UserInput || selection.contains(&entry.id)
            })
            .take(limit)
            .map(|entry| MqReceiveItem {
                entry,
                disposition: MqReceiveDisposition::Commit,
            })
            .collect();
        match self.receive(self.test_permit(), items).await {
            Ok(outcome) => outcome
                .items
                .into_iter()
                .filter_map(|item| item.entry.map(QueuedMessage::from_mq_entry))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn suppress_sync(&self, message: QueuedMessage) {
        let id = message.id();
        let entries = ready_only(self.query(MqFilter::default())).expect("test MQ query");
        let Some(entry) = entries.into_iter().find(|entry| entry.id == id) else {
            return;
        };
        let _ = ready_only(self.receive(
            self.test_permit(),
            vec![MqReceiveItem {
                entry,
                disposition: MqReceiveDisposition::Suppress,
            }],
        ));
    }

    pub fn suppressed_messages(&self) -> Vec<QueuedMessage> {
        self.inner.suppressed.lock().clone()
    }

    pub fn withdraw_user_inputs_sync(&self, ids: &[MessageId]) -> Vec<QueuedMessage> {
        ids.iter()
            .filter_map(|id| match ready_only(self.cancel_user_input(id)) {
                Ok(MqCancelOutcome::Cancelled(entry)) => Some(QueuedMessage::from_mq_entry(entry)),
                _ => None,
            })
            .collect()
    }

    pub fn snapshot_sync(&self) -> Vec<QueuedMessage> {
        ready_only(self.snapshot()).expect("test MQ snapshot")
    }

    pub fn clear_sync(&self) {
        ready_only(self.clear()).expect("test MQ clear");
    }

    pub fn len_sync(&self) -> usize {
        let selection = self.inner.send_selection.lock().clone();
        let entries = ready_only(self.query(MqFilter::default())).expect("test MQ len");
        entries
            .iter()
            .filter(|entry| {
                entry.source != MessageSource::UserInput || selection.contains(&entry.id)
            })
            .count()
    }

    pub fn is_empty_sync(&self) -> bool {
        self.len_sync() == 0
    }

    pub fn has_wake_up_sync(&self) -> bool {
        ready_only(self.has_wake_up()).expect("test MQ wake")
    }

    pub fn has_ensure_processing_sync(&self) -> bool {
        ready_only(self.has_ensure_processing()).expect("test MQ policy")
    }

    pub fn has_required_sync(&self) -> bool {
        ready_only(self.has_required()).expect("test MQ requirement")
    }

    pub fn has_required_for_run_sync(&self, execution: &ExecutionBinding) -> bool {
        ready_only(self.has_required_for_run(execution)).expect("test MQ run requirement")
    }

    pub fn has_pending_defer_sync(&self, source: &MessageSource) -> bool {
        ready_only(self.has_pending_defer(source)).expect("test MQ defer")
    }

    pub fn needs_mq_continuation_sync(&self) -> bool {
        ready_only(self.needs_mq_continuation()).expect("test MQ continuation")
    }

    pub fn admission_watermark_sync(&self) -> Option<MqWatermark> {
        ready_only(self.admission_watermark()).expect("test MQ watermark")
    }

    pub fn has_ensure_processing_after_sync(&self, watermark: &Option<MqWatermark>) -> bool {
        ready_only(self.has_ensure_processing_after(watermark)).expect("test MQ watermark policy")
    }
}

#[cfg(feature = "test-support")]
impl super::InboxHandle {
    pub fn push_sync(&self, message: QueuedMessage) {
        self.queue().push_sync(message);
    }

    pub fn push_prompt_sync(&self, source: MessageSource, message: BaseMessage) {
        self.push_sync(QueuedMessage::prompt(source, message));
    }

    pub fn push_defer_sync(&self, source: MessageSource, message: BaseMessage) {
        self.push_sync(QueuedMessage::defer(source, message));
    }

    pub fn push_info_sync(&self, source: MessageSource, message: BaseMessage) {
        self.push_sync(QueuedMessage::info(source, message));
    }

    pub fn push_batch_sync(&self, messages: Vec<QueuedMessage>) {
        self.queue().push_batch_sync(messages);
    }

    pub fn push_system_reminder_sync(
        &self,
        kind: MessageKind,
        source: MessageSource,
        reminder: TrustedSystemReminder,
    ) {
        self.push_sync(QueuedMessage::system_reminder(kind, source, reminder));
    }

    pub fn push_system_reminder_with_delivery_id_sync(
        &self,
        kind: MessageKind,
        source: MessageSource,
        reminder: TrustedSystemReminder,
        delivery_id: MessageId,
    ) {
        self.push_sync(QueuedMessage::system_reminder_with_delivery_id(
            kind,
            source,
            reminder,
            delivery_id,
        ));
    }
}

#[cfg(test)]
#[path = "queue_mq_test.rs"]
mod mq_tests;

#[cfg(test)]
#[path = "queue_source_test.rs"]
mod source_tests;

#[cfg(test)]
#[path = "queue_wake_test.rs"]
mod wake_tests;
