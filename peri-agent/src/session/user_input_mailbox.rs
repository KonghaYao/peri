//! 用户待发区与执行准入的会话级 owner，普通待办在 idle 时逐条交接给 MQ。
//!
//! MQ 是待发事实的权威；运行期记录只缓存投影所需的正文/原稿与调度状态。
//! `snapshot` 是只读投影，`refresh_from_store` 才按 MQ 查询结果对账
//! （`docs/design/thread-mq.md` §3.2/§4.2）。

mod mailbox;

mod commands;
mod delivery;
mod runtime;
mod snapshot;
mod state;
mod window;

pub use mailbox::UserInputRunTicket;

pub use mailbox::UserInputAttemptOutcome;

pub use mailbox::UserInputQueueError;

use mailbox::InterruptReason;

use mailbox::ActiveRun;

use mailbox::InputRecord;

use mailbox::CommandReceipt;

use mailbox::MailboxState;

pub use mailbox::UserInputMailbox;

use mailbox::SnapshotSeed;

#[cfg(test)]
#[path = "user_input_mailbox_test.rs"]
mod tests;
