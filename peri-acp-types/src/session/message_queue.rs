//! Queue payload、独立消息策略与有序 Receive 接纳。
//!
//! [`MessageQueue`] 是持久 MQ 的门面：所有事实操作经 [`crate::session::ThreadMqStore`] 落到存储层，
//! 内存只保留唤醒信号、运行期发送选择与通知版本。破坏性内存 API
//! （`push` / `drain_all` / `drain_batch` / `suppress` / `clear_batch` / `withdraw`）
//! 已移除——待发事实的唯一来源是 MQ 行（`docs/design/thread-mq.md` §4.1）。

#[expect(
    clippy::module_inception,
    reason = "The facade preserves API paths; the leaf is named for its primary type as required by the Rust guide."
)]
mod message_queue;
mod producer_queue;
mod queued_message;
#[cfg(feature = "test-support")]
mod test_support;

pub use message_queue::{DetachedEnqueue, MessageQueue, QueueClosed, ReceivePermit};
pub use producer_queue::{ProducerQueue, QueueOwner};
pub use queued_message::{MessageKind, MessageSource, QueuedMessage, QueuedPayload};

#[cfg(test)]
#[path = "queue_mq_test.rs"]
mod mq_tests;

#[cfg(test)]
#[path = "queue_source_test.rs"]
mod source_tests;

#[cfg(test)]
#[path = "queue_wake_test.rs"]
mod wake_tests;
