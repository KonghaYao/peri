//! MessageQueue v2 — 会话级临时收件箱（事实源 `peri-acp-types::session`，
//! 本模块 re-export 保兼容）。
//!
//! 消息分三类（Prompt/Defer/Info）控制循环唤醒与消费行为；RCRA 循环的
//! Receive 阶段通过 `drain_all` 一次性消费，循环退出后 `has_wake_up` 检测激活。

pub use peri_acp_types::session::{MessageKind, MessageQueue, MessageSource, QueuedMessage};
