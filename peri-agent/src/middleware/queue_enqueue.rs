//! 统一 v2 MessageQueue 入队：优先经 [`InboxHandle`] 以触发 `await_wake`。

use peri_acp_types::session::QueuedMessage;

use super::state::MiddlewareState;

/// 将消息写入会话级队列；有 inbox 时走 `InboxHandle::push`（Defer/Prompt 会 wake）。
pub fn enqueue_v2_message(state: &dyn MiddlewareState, msg: QueuedMessage) {
    if let Some(inbox) = state.inbox_handle() {
        inbox.push(msg);
    } else {
        state.v2_queue().push(msg);
    }
}
