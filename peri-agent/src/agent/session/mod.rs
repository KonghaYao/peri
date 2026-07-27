//! Session-level inbox abstraction — async await-wake for idle state
//!
//! [`SessionInbox`] wraps the existing v2 [`MessageQueue`](crate::session::MessageQueue)
//! with an async wake mechanism. During IDLE (between ReAct loops), the ACP executor
//! calls [`await_wake`](SessionInbox::await_wake) which blocks until a new Prompt/Defer
//! is enqueued, then the loop resumes.
//!
//! During the ReAct loop itself, `stages/receive.rs` calls `drain_all`
//! to consume pending messages — no wake needed (loop is already spinning).
//!
//! Pushers from Agent/ACP layers use [`InboxHandle`] (cloneable). The TUI should NOT have
//! access to this handle — TUI loses its `drain_for_end` responsibility in v2.

pub mod channel_owner;
pub mod cron_owner;
pub mod inbox;

pub use channel_owner::ChannelOwner;
pub use cron_owner::CronOwner;
pub use inbox::InboxHandle;
pub use inbox::SessionInbox;
