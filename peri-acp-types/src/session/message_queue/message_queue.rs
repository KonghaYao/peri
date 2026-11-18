//! Persistent queue admission, delivery, and wake state.

use super::super::mq::{
    MqCancelOutcome, MqEnqueueDisposition, MqEnqueueOutcome, MqEntry, MqFilter, MqHeader,
    MqReceiveDisposition, MqReceiveItem, MqReceiveOutcome, MqReceivePurpose, MqReceiveSelection,
    MqReceiveStatus, MqSendItemOutcome, MqSendOutcome, MqSendStatus, MqWatermark,
    SessionResourcesMqStore, ThreadMqStore,
};
use super::super::{ExecutionBinding, MessageDisposition};
use super::{MessageKind, MessageSource, QueuedMessage};
use crate::messages::{BaseMessage, MessageId};
use crate::session_resources::SessionResources;
use crate::store::PersistedPayload;
use crate::thread::ThreadId;
use anyhow::Result;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 单次 Receive 的上限；与旧 `drain_batch` 的批次上限一致。
const RECEIVE_BATCH_LIMIT: usize = 64;

/// 显式发送等待移交时的查询间隔；只影响确认延迟，不改变任何事实。
const SEND_HANDOFF_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// [`MessageQueue::new`] 的内存 thread 标识；不参与生产路由。
const IN_MEMORY_THREAD_ID: &str = "in-memory";

// ─── MessageQueue ────────────────────────────────────────────────────────────

/// 关闭结算开始后拒绝入队；携带 thread 便于调用方诊断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueClosed {
    pub thread_id: String,
}

impl std::fmt::Display for QueueClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "message queue for thread {} is closed to new enqueues",
            self.thread_id
        )
    }
}

impl std::error::Error for QueueClosed {}

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
    pub(super) inner: Arc<Mailbox>,
}

pub(super) struct Mailbox {
    pub(super) store: Arc<dyn ThreadMqStore>,
    thread_id: ThreadId,
    wake: tokio::sync::Notify,
    wake_version: tokio::sync::watch::Sender<u64>,
    pub(super) send_selection: Mutex<Vec<MessageId>>,
    /// 已受理但尚未提交的入队数量（同步 hook 桥与直接入队共用）。
    pending_enqueues: AtomicUsize,
    /// 入队闸门：关闭结算开始后拒绝新入队；clone 共享。
    ///
    /// 只存在于本门面，不写入存储：同一 thread 的新 [`bind`](MessageQueue::bind)
    /// （新 owner / resume）是新的开放门面。
    admission_closed: AtomicBool,
    pending_drained: tokio::sync::Notify,
    /// 仅测试：记录被 `suppress_sync` 处置的消息，供旧断言读取。
    #[cfg(feature = "test-support")]
    pub(super) suppressed: Mutex<Vec<QueuedMessage>>,
}

/// 一次分离入队的在途凭证；Drop 即宣告该写入结束（见 [`MessageQueue::begin_detached_enqueue`]）。
pub struct DetachedEnqueue {
    inner: Arc<Mailbox>,
}

impl Drop for DetachedEnqueue {
    fn drop(&mut self) {
        if self.inner.pending_enqueues.fetch_sub(1, Ordering::SeqCst) == 1 {
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
            Arc::new(super::super::mq_memory::InMemoryMqStore::new()),
            IN_MEMORY_THREAD_ID.to_owned(),
        )
    }

    /// 生产入口：把门面绑定到真实资源与 thread。
    pub fn bind(resources: Arc<dyn SessionResources>, thread_id: ThreadId) -> Self {
        Self::from_store(Arc::new(SessionResourcesMqStore::new(resources)), thread_id)
    }

    pub(super) fn from_store(store: Arc<dyn ThreadMqStore>, thread_id: ThreadId) -> Self {
        Self {
            inner: Arc::new(Mailbox {
                store,
                thread_id,
                wake: tokio::sync::Notify::new(),
                wake_version: tokio::sync::watch::channel(0).0,
                send_selection: Mutex::new(Vec::new()),
                pending_enqueues: AtomicUsize::new(0),
                admission_closed: AtomicBool::new(false),
                pending_drained: tokio::sync::Notify::new(),
                #[cfg(feature = "test-support")]
                suppressed: Mutex::new(Vec::new()),
            }),
        }
    }

    /// 本门面绑定的 thread。
    #[must_use]
    pub fn thread_id(&self) -> &ThreadId {
        &self.inner.thread_id
    }

    /// 登记一次「已受理、尚未提交」的分离入队。
    ///
    /// 同步 middleware hook 无法就地 await 持久提交，只能把写入投递到 runtime；
    /// 调用方在下一次 [`Self::await_pending_enqueues`] 之前不得据此断定收件箱为空。
    /// 返回的 guard 在写入结束（成功或失败）时 Drop，才把入队计为已完成。
    pub fn begin_detached_enqueue(&self) -> DetachedEnqueue {
        self.inner.pending_enqueues.fetch_add(1, Ordering::SeqCst);
        DetachedEnqueue {
            inner: Arc::clone(&self.inner),
        }
    }

    /// 关闭入队准入；幂等。
    ///
    /// 关闭结算在读取 MQ 快照前调用，随后 [`Self::await_pending_enqueues`] 等到
    /// 已通过闸门的写入结束，结算看到的行集合才稳定。结算自身的存储写入
    /// （`settle_retained_mq`）直接经资源门面，不经过本闸门。
    pub fn close_admission(&self) {
        self.inner.admission_closed.store(true, Ordering::SeqCst);
    }

    /// 入队闸门是否已关闭（clone 共享）。
    pub fn is_admission_closed(&self) -> bool {
        self.inner.admission_closed.load(Ordering::SeqCst)
    }

    /// 为一次外部入队登记在途凭证，并检查关闭闸门。
    ///
    /// 先登记计数再读闸门：close 先关闸、再等计数归零，因此不会漏掉
    /// 「已通过检查但尚未提交」的写入。被拒绝的调用不写存储，返回
    /// [`QueueClosed`]。
    pub(super) fn admit_enqueue(&self, message: &QueuedMessage) -> Result<DetachedEnqueue> {
        let pending = self.begin_detached_enqueue();
        if self.is_admission_closed() {
            tracing::warn!(
                thread_id = %self.inner.thread_id,
                kind = ?message.kind,
                source = ?message.source,
                requirement = ?message.policy.requirement,
                "message queue admission is closed; enqueue rejected"
            );
            return Err(anyhow::Error::new(QueueClosed {
                thread_id: self.inner.thread_id.to_string(),
            }));
        }
        Ok(pending)
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
            if self.inner.pending_enqueues.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }

    // ── 事实操作 ──

    /// 入队一条消息并返回接纳结论；只有可唤醒策略（非 Passive）才发布唤醒。
    ///
    /// 调用方需要稳定 id 时用返回的 `outcome.entry.id`：未固定的 `SystemReminder`
    /// `delivery_id` 在本次入队时生成，重试必须复用它。关闭结算开始后返回
    /// [`QueueClosed`]，不写存储。
    pub async fn enqueue(&self, message: QueuedMessage) -> Result<MqEnqueueOutcome> {
        let notifies = message.policy.notifies_execution();
        let _pending = self.admit_enqueue(&message)?;
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
            // 这里只写入发送选择：启动路径随后才跑 Receive，不能在此等待移交。
            self.accept_for_send(vec![id]).await?;
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
    ///
    /// 关闭结算开始后拒绝后续条目，已提交前缀保持持久事实。
    pub async fn enqueue_batch(
        &self,
        messages: Vec<QueuedMessage>,
    ) -> Result<Vec<MqEnqueueOutcome>> {
        let mut outcomes = Vec::with_capacity(messages.len());
        let mut wake = false;
        for message in messages {
            let notifies = message.policy.notifies_execution();
            let _pending = self.admit_enqueue(&message)?;
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

    /// 失败 attempt 的内部清理：只删除本 attempt 写入的 `SystemInjected` 行。
    ///
    /// 用户取消仍只走 [`Self::cancel_user_input`]（只接受 `UserInput`）；本入口只用于
    /// 执行侧在 admission 失败后撤回尚未被 Receive 选中的指引，不写历史。
    pub async fn discard_system_injected_for_attempt(
        &self,
        entries: &[MqEntry],
        execution: &ExecutionBinding,
    ) -> Result<MqReceiveOutcome> {
        if entries.is_empty() {
            return Ok(MqReceiveOutcome { items: Vec::new() });
        }
        if entries
            .iter()
            .any(|entry| entry.source != MessageSource::SystemInjected)
        {
            anyhow::bail!("attempt cleanup only accepts system-injected MQ rows");
        }
        let selection = MqReceiveSelection {
            items: entries
                .iter()
                .cloned()
                .map(|entry| MqReceiveItem {
                    entry,
                    disposition: MqReceiveDisposition::Suppress,
                })
                .collect(),
            purpose: MqReceivePurpose::NonModelSettlement,
            execution: execution.clone(),
        };
        Ok(self
            .inner
            .store
            .mq_receive(&self.inner.thread_id, selection)
            .await?)
    }

    /// 提交运行期发送选择：只接受仍在 MQ 的用户输入，不写历史（`§5.3`）。
    ///
    /// 只报告当下事实：被接纳的消息仍是待发事实（`Accepted`），直到 Receive 移交成功。
    /// 未命中的 id 按历史的持久事实区分「已在历史」与「不存在」，无法查询时报告 `Unknown`。
    /// 需要确认移交时调用 [`Self::send_inputs_within`]（有界等待，不得在阻塞运行时里用）。
    pub async fn send_inputs(&self, ids: Vec<MessageId>) -> Result<MqSendOutcome> {
        self.accept_for_send(ids).await
    }

    /// [`Self::send_inputs`] 的有界等待版本：尽可能确认移交，超时仍报 `Accepted`。
    ///
    /// 等待必须有界（`handoff_timeout` 由调用方给出）；必须从活跃的异步上下文调用，
    /// 调用方若在 `block_on` 里等待它，就会阻塞唯一能推进 Receive 的线程。
    pub async fn send_inputs_within(
        &self,
        ids: Vec<MessageId>,
        handoff_timeout: std::time::Duration,
    ) -> Result<MqSendOutcome> {
        let mut outcome = self.accept_for_send(ids).await?;
        // 没有 Tokio 运行时就没有并发的 Receive：不进入周期性查询（也无法 sleep），
        // 如实报告「已接纳未移交」。
        let handoff_timeout = if tokio::runtime::Handle::try_current().is_err() {
            std::time::Duration::ZERO
        } else {
            handoff_timeout
        };
        if handoff_timeout.is_zero() {
            return Ok(outcome);
        }
        let deadline = std::time::Instant::now() + handoff_timeout;
        // 只等待仍记作 Accepted 的项：其余结论已经由存储事实给出。
        loop {
            let pending = outcome
                .items
                .iter()
                .filter(|item| item.status == MqSendStatus::Accepted)
                .map(|item| item.id)
                .collect::<Vec<_>>();
            if pending.is_empty() {
                return Ok(outcome);
            }
            let entries = match self.query(MqFilter::default()).await {
                Ok(entries) => entries,
                Err(error) => {
                    // 发送选择已经提交：查询失败只能停止确认，不能把整次调用变成
                    // 「未接纳」，否则调用方会误以为重试是安全的。
                    tracing::warn!(
                        %error,
                        "hand-off confirmation query failed; keeping accepted status"
                    );
                    return Ok(outcome);
                }
            };
            for item in &mut outcome.items {
                if item.status != MqSendStatus::Accepted || entries.iter().any(|e| e.id == item.id)
                {
                    continue;
                }
                item.status = match self
                    .inner
                    .store
                    .mq_history_contains(&self.inner.thread_id, item.id)
                    .await
                {
                    Ok(true) => MqSendStatus::HandedOff,
                    Ok(false) => MqSendStatus::NotFound,
                    Err(error) => {
                        tracing::warn!(%error, "hand-off confirmation failed; reporting unknown");
                        MqSendStatus::Unknown
                    }
                };
                if item.status != MqSendStatus::HandedOff {
                    item.entry = None;
                }
            }
            if std::time::Instant::now() >= deadline {
                return Ok(outcome);
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            tokio::time::sleep(remaining.min(SEND_HANDOFF_POLL_INTERVAL)).await;
        }
    }

    /// 只写入发送选择并报告当下事实，不做移交等待（`enqueue_sent` 的同步语义）。
    pub(super) async fn accept_for_send(&self, ids: Vec<MessageId>) -> Result<MqSendOutcome> {
        let entries = self
            .query(MqFilter {
                user_input_only: true,
                ..Default::default()
            })
            .await?;
        let mut items = Vec::with_capacity(ids.len());
        let mut accepted_ids = Vec::new();
        for id in ids {
            match entries.iter().find(|entry| entry.id == id) {
                Some(entry) => {
                    if !accepted_ids.contains(&id) {
                        accepted_ids.push(id);
                    }
                    items.push(MqSendItemOutcome {
                        id,
                        status: MqSendStatus::Accepted,
                        entry: Some(entry.clone()),
                    });
                }
                None => {
                    // 先查持久历史事实，再一次性写入发送选择：锁不能跨 await。
                    let status = match self
                        .inner
                        .store
                        .mq_history_contains(&self.inner.thread_id, id)
                        .await
                    {
                        Ok(true) => MqSendStatus::AlreadyInHistory,
                        Ok(false) => MqSendStatus::NotFound,
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                "history lookup failed for a missing send id; reporting unknown"
                            );
                            MqSendStatus::Unknown
                        }
                    };
                    items.push(MqSendItemOutcome {
                        id,
                        status,
                        entry: None,
                    });
                }
            }
        }
        if !accepted_ids.is_empty() {
            let mut selection = self.inner.send_selection.lock();
            for id in accepted_ids {
                if !selection.contains(&id) {
                    selection.push(id);
                }
            }
            drop(selection);
            self.notify_wake();
        }
        Ok(MqSendOutcome { items })
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
    pub(super) async fn select_for_receive(
        &self,
        permit: &ReceivePermit,
    ) -> Result<Vec<MqReceiveItem>> {
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
    /// 失败时选择保持原样（未确认移交前不能假装已发送）。提交成功之后的唤醒检查是
    /// best-effort：它只影响「是否立刻再取一次」，不能让已提交的移交变成调用方眼中的失败。
    pub(super) async fn receive(
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
        // 移交已经提交：此后的唤醒只是「还有活」的提示，查询失败不能把成功结果
        // 变成错误——调用方会据此丢弃已提交的移交（历史已写、MQ 已删）。
        match self.has_processable_work().await {
            Ok(true) => self.notify_wake(),
            Ok(false) => {}
            Err(error) => tracing::warn!(
                thread_id = %self.inner.thread_id,
                %error,
                "post-receive wake check failed; committed handoff is kept"
            ),
        }
        Ok(outcome)
    }

    /// 取一次 Receive 许可：仅 Agent Loop 与同 thread 生命周期 owner 调用。
    ///
    /// 许可不可 Clone，`receive` 会消费它，防止重放同一次准入。
    pub(super) fn begin_receive(
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
        let headers = self
            .inner
            .store
            .mq_headers(&self.inner.thread_id, MqFilter::default())
            .await?;
        let selection = self.inner.send_selection.lock().clone();
        Ok(headers.iter().any(|header| {
            is_header_eligible(header, &selection) && header.policy.ensures_processing()
        }))
    }

    pub async fn has_required_for_run(&self, execution: &ExecutionBinding) -> Result<bool> {
        let headers = self
            .inner
            .store
            .mq_headers(&self.inner.thread_id, MqFilter::default())
            .await?;
        let selection = self.inner.send_selection.lock().clone();
        Ok(headers.iter().any(|header| {
            is_header_eligible(header, &selection)
                && header.policy.disposition(execution) == MessageDisposition::Process
        }))
    }

    pub async fn has_required(&self) -> Result<bool> {
        Ok(self
            .inner
            .store
            .mq_headers(&self.inner.thread_id, MqFilter::default())
            .await?
            .iter()
            .any(|header| header.policy.requirement == super::super::MessageRequirement::Required))
    }

    /// 按来源查询 required 消息，仅用于诊断；不授予来源调度权。
    pub async fn has_pending_defer(&self, source: &MessageSource) -> Result<bool> {
        Ok(self
            .inner
            .store
            .mq_headers(
                &self.inner.thread_id,
                MqFilter {
                    sources: vec![source.clone()],
                    ..Default::default()
                },
            )
            .await?
            .iter()
            .any(|header| header.policy.ensures_processing()))
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
        Ok(!self.inner.store.mq_exists(&self.inner.thread_id).await?)
    }

    /// 队列长度。
    pub async fn len(&self) -> Result<usize> {
        Ok(self.inner.store.mq_count(&self.inner.thread_id).await?)
    }

    /// 当前 MQ 的最大 `(enqueued_at, id)`；空队列返回 `None`。
    pub async fn admission_watermark(&self) -> Result<Option<MqWatermark>> {
        Ok(self.inner.store.mq_watermark(&self.inner.thread_id).await?)
    }

    /// 是否在水位之后有需要模型处理的入队项；`None` 表示检查全部。
    pub async fn has_ensure_processing_after(
        &self,
        watermark: Option<&MqWatermark>,
    ) -> Result<bool> {
        let filter = MqFilter {
            after: watermark.cloned(),
            ..Default::default()
        };
        let headers = self
            .inner
            .store
            .mq_headers(&self.inner.thread_id, filter)
            .await?;
        let selection = self.inner.send_selection.lock().clone();
        Ok(headers.iter().any(|header| {
            is_header_eligible(header, &selection) && header.policy.ensures_processing()
        }))
    }

    /// 清空本 thread 的全部 MQ 行：只给 rewind 与 thread 生命周期调用。
    ///
    /// 常规消费必须走 [`super::QueueOwner::receive`]；本方法会丢弃尚未处理的待发事实。
    pub(super) async fn clear(&self) -> Result<()> {
        self.inner.store.mq_clear(&self.inner.thread_id).await?;
        self.inner.send_selection.lock().clear();
        Ok(())
    }

    // ── 唤醒 ──

    pub fn subscribe_wake(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.wake_version.subscribe()
    }

    /// 请求激活 listener 重新读取一次 MQ：不改变任何队列事实，只补一次唤醒。
    ///
    /// 查询失败时调用方无法判断「有没有工作」；把重查责任交还 listener，避免一次
    /// 瞬时错误消耗掉唯一一次已消费的唤醒。没有 listener 时是空操作。
    pub fn request_recheck(&self) {
        self.notify_wake();
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

    pub(super) fn notify_wake(&self) {
        self.inner.wake.notify_waiters();
        self.inner
            .wake_version
            .send_modify(|version| *version = version.saturating_add(1));
    }

    /// 移交后是否仍有本轮可继续消费的工作；调用方据此补一次唤醒。
    pub(super) async fn has_processable_work(&self) -> Result<bool> {
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
    source_is_eligible(&entry.source, &entry.id, selection)
}

/// 控制面 header 与完整行共用同一条 eligibility 规则。
fn is_header_eligible(header: &MqHeader, selection: &[MessageId]) -> bool {
    source_is_eligible(&header.source, &header.id, selection)
}

fn source_is_eligible(source: &MessageSource, id: &MessageId, selection: &[MessageId]) -> bool {
    *source != MessageSource::UserInput || selection.contains(id)
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
/// [`crate::session::QueueOwner::begin_receive`] 取得，[`crate::session::QueueOwner::receive`] 消费它。
#[derive(Debug)]
pub struct ReceivePermit {
    execution: ExecutionBinding,
    purpose: MqReceivePurpose,
}

impl ReceivePermit {
    pub(super) fn into_selection(self, items: Vec<MqReceiveItem>) -> MqReceiveSelection {
        MqReceiveSelection {
            items,
            purpose: self.purpose,
            execution: self.execution,
        }
    }
}
