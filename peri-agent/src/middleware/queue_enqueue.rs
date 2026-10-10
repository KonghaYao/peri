//! 统一 v2 mailbox 发布，由 queue 同源地管理消息与唤醒。

use peri_acp_types::session::QueuedMessage;

use super::capabilities::QueueState;

/// 发布到会话级 mailbox，保留消息类型的调度语义。
pub fn enqueue_v2_message(state: &dyn QueueState, msg: QueuedMessage) {
    state.enqueue_v2_message(msg);
}
