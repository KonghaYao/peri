//! MCP subscriptions 端口契约（2026-07-28 `subscriptions/listen`）。
//!
//! 由 `peri-middlewares` 的 `McpClientPool` 实现：连接协商 2026-07-28 协议后
//! 建立订阅长流，收到 `notifications/resources/updated` 时按通知 `_meta`
//! 声明向会话 inbox 投递 Defer 或 Info；缺省资源更新仍为
//! Defer，可唤醒 idle executor 回复外部消息。
//!
//! 装配方向（与 cron 端口同构）：
//! `SessionManager`（peri-acp）在 session 创建时调用 `register_inbox`，
//! `close_session` 时调用 `unregister_inbox`；通知转发与唤醒由实现方完成。

use std::{any::Any, sync::Arc};

use crate::session::InboxHandle;

/// Peri extension on an MCP notification's `_meta`: queue scheduling intent.
pub const MCP_MESSAGE_KIND_META_KEY: &str = "peri/messageKind";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpNotificationMessageKind {
    Defer,
    Info,
}

impl McpNotificationMessageKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Defer => "defer",
            Self::Info => "info",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "defer" => Some(Self::Defer),
            "info" => Some(Self::Info),
            _ => None,
        }
    }
}

impl From<McpNotificationMessageKind> for crate::session::MessageKind {
    fn from(value: McpNotificationMessageKind) -> Self {
        match value {
            McpNotificationMessageKind::Defer => Self::Defer,
            McpNotificationMessageKind::Info => Self::Info,
        }
    }
}

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
