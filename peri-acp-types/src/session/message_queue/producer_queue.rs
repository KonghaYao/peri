//! Producer and lifecycle owner capability views.

use super::super::mq::{
    MqCancelOutcome, MqEnqueueOutcome, MqEntry, MqFilter, MqReceiveItem, MqReceiveOutcome,
    MqReceivePurpose, MqSendOutcome,
};
use super::super::ExecutionBinding;
use super::{DetachedEnqueue, MessageQueue, QueuedMessage, ReceivePermit};
use crate::messages::MessageId;
use crate::thread::ThreadId;
use anyhow::Result;
use std::time::Duration;

// ─── 能力视图：生产者 / 生命周期 owner ────────────────────────────────────────
//
// 同一份 MQ 事实有两个能力面：生产者只能入队、查询与写发送选择；Receive 与清空
// 会改变消费事实，只出现在生命周期 owner 视图上。两者是不同名义类型，生产者拿到的
// （`InboxHandle::queue`）没有通往 owner 视图的方法，误用在编译期就失败。

/// 生产者视图：入队、只读查询与运行期发送选择，不含消费与清空。
#[derive(Clone, Copy, Debug)]
pub struct ProducerQueue<'queue> {
    pub(super) queue: &'queue MessageQueue,
}

/// 生命周期 owner 视图：Receive 许可、原子移交与 thread 级清空。
///
/// 由 [`MessageQueue::owner`] 取得；调用方必须是 Agent Loop、rewind 或 thread 关闭
/// 等生命周期 owner。
#[derive(Clone, Copy, Debug)]
pub struct QueueOwner<'queue> {
    pub(super) queue: &'queue MessageQueue,
}

impl MessageQueue {
    /// 生产者能力面（窄接口）。
    pub fn producer(&self) -> ProducerQueue<'_> {
        ProducerQueue { queue: self }
    }

    /// 生命周期 owner 能力面（含消费与清空）。
    pub fn owner(&self) -> QueueOwner<'_> {
        QueueOwner { queue: self }
    }
}

impl ProducerQueue<'_> {
    /// 同一 mailbox 的底层门面；只在 crate 内用于构造可 `'static` 的窄句柄。
    pub(crate) fn queue_ref(&self) -> &MessageQueue {
        self.queue
    }

    pub fn thread_id(&self) -> &ThreadId {
        self.queue.thread_id()
    }

    pub fn begin_detached_enqueue(&self) -> DetachedEnqueue {
        self.queue.begin_detached_enqueue()
    }

    pub async fn await_pending_enqueues(&self) {
        self.queue.await_pending_enqueues().await;
    }

    pub async fn enqueue(&self, message: QueuedMessage) -> Result<MqEnqueueOutcome> {
        self.queue.enqueue(message).await
    }

    pub async fn enqueue_sent(&self, message: QueuedMessage) -> Result<MqEnqueueOutcome> {
        self.queue.enqueue_sent(message).await
    }

    pub fn enqueue_now(&self, message: QueuedMessage) -> Option<Result<MqEnqueueOutcome>> {
        self.queue.enqueue_now(message)
    }

    pub async fn enqueue_batch(
        &self,
        messages: Vec<QueuedMessage>,
    ) -> Result<Vec<MqEnqueueOutcome>> {
        self.queue.enqueue_batch(messages).await
    }

    pub async fn query(&self, filter: MqFilter) -> Result<Vec<MqEntry>> {
        self.queue.query(filter).await
    }

    pub async fn snapshot(&self) -> Result<Vec<QueuedMessage>> {
        self.queue.snapshot().await
    }

    pub async fn cancel_user_input(&self, id: &MessageId) -> Result<MqCancelOutcome> {
        self.queue.cancel_user_input(id).await
    }

    pub async fn send_inputs(&self, ids: Vec<MessageId>) -> Result<MqSendOutcome> {
        self.queue.send_inputs(ids).await
    }

    pub async fn send_inputs_within(
        &self,
        ids: Vec<MessageId>,
        timeout: Duration,
    ) -> Result<MqSendOutcome> {
        self.queue.send_inputs_within(ids, timeout).await
    }

    pub fn take_send_selection(&self) -> Vec<MessageId> {
        self.queue.take_send_selection()
    }

    pub fn withdraw_send_inputs(&self, ids: &[MessageId]) {
        self.queue.withdraw_send_inputs(ids);
    }

    pub async fn await_wake(&self) {
        self.queue.await_wake().await;
    }

    pub fn subscribe_wake(&self) -> tokio::sync::watch::Receiver<u64> {
        self.queue.subscribe_wake()
    }

    pub async fn has_ensure_processing(&self) -> Result<bool> {
        self.queue.has_ensure_processing().await
    }

    pub async fn has_required(&self) -> Result<bool> {
        self.queue.has_required().await
    }

    pub async fn has_wake_up(&self) -> Result<bool> {
        self.queue.has_wake_up().await
    }

    pub async fn is_empty(&self) -> Result<bool> {
        self.queue.is_empty().await
    }

    pub async fn len(&self) -> Result<usize> {
        self.queue.len().await
    }
}

impl QueueOwner<'_> {
    /// 取一次 Receive 许可；许可不可 Clone，移交时被消费。
    pub fn begin_receive(
        &self,
        purpose: MqReceivePurpose,
        execution: ExecutionBinding,
    ) -> ReceivePermit {
        self.queue.begin_receive(purpose, execution)
    }

    pub async fn select_for_receive(&self, permit: &ReceivePermit) -> Result<Vec<MqReceiveItem>> {
        self.queue.select_for_receive(permit).await
    }

    pub async fn receive(
        &self,
        permit: ReceivePermit,
        items: Vec<MqReceiveItem>,
    ) -> Result<MqReceiveOutcome> {
        self.queue.receive(permit, items).await
    }

    /// 清空本 thread 的全部 MQ 行：只给 rewind 与 thread 生命周期调用。
    pub async fn clear(&self) -> Result<()> {
        self.queue.clear().await
    }
}
