use std::sync::Arc;

use peri_acp_types::session::{MessageKind, MessageSource, QueuedMessage};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::tasks::TaskTerminalDelivery;
use sha2::{Digest, Sha256};

use crate::session::async_result_mailbox::{AcceptOutcome, AsyncResultMailbox};
use crate::session::MessageQueue;

pub(crate) fn terminal_delivery_id(
    task_id: &str,
    terminal_transition_id: &str,
) -> peri_acp_types::messages::MessageId {
    let mut hasher = Sha256::new();
    hasher.update((task_id.len() as u64).to_be_bytes());
    hasher.update(task_id.as_bytes());
    hasher.update((terminal_transition_id.len() as u64).to_be_bytes());
    hasher.update(terminal_transition_id.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    peri_acp_types::messages::MessageId::from(uuid::Uuid::from_bytes(bytes))
}

pub(crate) struct SessionTerminalDelivery {
    queue: MessageQueue,
    mailbox: Arc<AsyncResultMailbox>,
}

impl SessionTerminalDelivery {
    /// 仅测试/无宿主回退：使用一次性 mailbox，不跨会话共享保护状态。
    pub(crate) fn for_queue(queue: MessageQueue) -> Arc<dyn TaskTerminalDelivery> {
        Self::for_queue_with_mailbox(queue, Arc::new(AsyncResultMailbox::default()))
    }

    /// 生产路径：投递责任与模型确认共用同一个会话级 owner。
    pub(crate) fn for_queue_with_mailbox(
        queue: MessageQueue,
        mailbox: Arc<AsyncResultMailbox>,
    ) -> Arc<dyn TaskTerminalDelivery> {
        Arc::new(Self { queue, mailbox })
    }
}

impl TaskTerminalDelivery for SessionTerminalDelivery {
    fn accept(
        &self,
        delivery_id: peri_acp_types::messages::MessageId,
        reminder: &TrustedSystemReminder,
        source: MessageSource,
    ) -> Result<(), String> {
        let future = self.deliver(delivery_id, reminder, source);
        // 调用方可能是 runtime 线程（shell 监视线程不会），也可能完全没有 runtime
        // （同步测试）。统一在独立线程的新 runtime 上等待真实提交：既不依赖调用
        // 线程模型，也不允许先返回成功再 detached 写入。
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let bridge = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| format!("terminal delivery bridge runtime: {error}"))?;
                    bridge.block_on(future)
                })
                .join()
                .map_err(|_| "terminal delivery bridge thread panicked".to_string())?
        })
    }

    fn deliver<'a>(
        &'a self,
        delivery_id: peri_acp_types::messages::MessageId,
        reminder: &'a TrustedSystemReminder,
        source: MessageSource,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            // 先建立 Queued 责任，再让队列消息可见：消费者不可能看到一条
            // “已入队但 mailbox 尚未登记”的终态通知。锁不跨 await。
            match self.mailbox.accept(delivery_id, reminder, source.clone())? {
                AcceptOutcome::Accepted => self
                    .queue
                    .enqueue(QueuedMessage::system_reminder_with_delivery_id(
                        MessageKind::Defer,
                        source,
                        reminder.clone(),
                        delivery_id,
                    ))
                    .await
                    .map_err(|error| error.to_string())
                    .map(|_| ()),
                AcceptOutcome::Duplicate => Ok(()),
            }
        })
    }
}
