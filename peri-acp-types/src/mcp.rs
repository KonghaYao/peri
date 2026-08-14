//! MCP subscriptions 端口契约（2026-07-28 `subscriptions/listen`）。
//!
//! 由 `peri-middlewares` 的 `McpClientPool` 实现：连接协商 2026-07-28 协议后
//! 建立订阅长流，收到通知（`notifications/resources/updated` / `list_changed`）
//! 时向已注册的会话 inbox 推送 Defer 消息并唤醒 idle executor（agent 因此
//! 可以主动回复外部聊天消息，无需用户在 TUI 里发 prompt）。
//!
//! 装配方向（与 cron 端口同构）：
//! `SessionManager`（peri-acp）在 session 创建时调用 `register_inbox`，
//! `close_session` 时调用 `unregister_inbox`；通知转发与唤醒由实现方完成。

use std::{any::Any, sync::Arc};

use crate::session::InboxHandle;

/// MCP subscriptions 通知 → 会话 inbox 的桥接端口
pub trait McpSubscriptionPort: Send + Sync {
    /// 注册一个会话的 inbox（session 创建 / 首次启动 bridge 时调用，幂等）。
    fn register_inbox(&self, session_id: &str, handle: InboxHandle);

    /// 注销会话的 inbox（close_session 时调用）。
    fn unregister_inbox(&self, session_id: &str);

    /// 还原具体实现（downcast 还原点，供装配面与宿主使用）。
    fn as_any(&self) -> &dyn Any;
}

impl dyn McpSubscriptionPort {
    /// 将 `Arc<dyn McpSubscriptionPort>` 还原为具体实现 `Arc<T>`
    /// （类型不符返回原 `Arc`；注意经 `as_any()` 取 TypeId，
    /// 见 `CronSchedulerPort::downcast_arc` 的踩坑注释）。
    pub fn downcast_arc<T: McpSubscriptionPort + 'static>(
        self: Arc<Self>,
    ) -> Result<Arc<T>, Arc<Self>> {
        let ptr = Arc::into_raw(self);
        unsafe {
            if (*ptr).as_any().type_id() == std::any::TypeId::of::<T>() {
                Ok(Arc::from_raw(ptr as *const T))
            } else {
                Err(Arc::from_raw(ptr))
            }
        }
    }
}

#[cfg(test)]
#[path = "mcp_test.rs"]
mod tests;
