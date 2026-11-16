//! Synchronous adapters for existing queue tests.

use super::super::mq::{
    MqCancelOutcome, MqEnqueueOutcome, MqFilter, MqReceiveDisposition, MqReceiveItem,
    MqReceivePurpose, MqWatermark,
};
use super::super::ExecutionBinding;
use super::{
    MessageKind, MessageQueue, MessageSource, ProducerQueue, QueuedMessage, ReceivePermit,
};
use crate::messages::{BaseMessage, MessageId};
use crate::system_reminder::TrustedSystemReminder;

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
            super::super::ExecutionBinding {
                turn_id: super::super::TurnId::new(),
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

    /// 仅测试：下一次 enqueue 在写入前暂停；返回（进入通知，放行信号）。
    pub fn gate_next_enqueue(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        self.inner
            .store
            .install_enqueue_gate((entered_tx, release_rx));
        (entered_rx, release_tx)
    }

    /// 仅测试：下一次 MQ 查询在读取前暂停；返回（进入通知，放行信号）。
    pub fn gate_next_query(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        self.inner
            .store
            .install_query_gate((entered_tx, release_rx));
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

    pub fn has_ensure_processing_after_sync(&self, watermark: Option<&MqWatermark>) -> bool {
        ready_only(self.has_ensure_processing_after(watermark)).expect("test MQ watermark policy")
    }
}

#[cfg(feature = "test-support")]
impl ProducerQueue<'_> {
    pub fn push_sync(&self, message: QueuedMessage) {
        self.queue.push_sync(message);
    }

    pub fn push_batch_sync(&self, messages: Vec<QueuedMessage>) {
        self.queue.push_batch_sync(messages);
    }
}

#[cfg(feature = "test-support")]
impl super::super::InboxHandle {
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
