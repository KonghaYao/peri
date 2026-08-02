//! MessageQueue v2 — 会话级临时收件箱
//!
//! 独立于 SessionStore，会话内持续可变。**不持久化**——Session 重建时从空开始。
//!
//! ## 消息分三类（控制循环唤醒和消费行为）
//!
//! | Kind | 来源 | RCRA Receive 行为 | 唤醒新 turn |
//! |------|------|-------------------|------------|
//! | `Prompt` | 外部用户输入、外部主动请求 | drain_all 消费 | ✅ |
//! | `Defer` | SubAgent/Cron/Channel/Workflow 完成 | drain_all 消费 | ✅ |
//! | `Info` | SystemReminder、Hook 注入 | drain_all 消费 | ❌ |
//!
//! RCRA 循环的 Receive 阶段通过 [`drain_all`] 一次性消费全部三类消息。
//! 循环退出后，若队列新到达 Prompt 或 Defer，通过 [`has_wake_up`] 检测并重新激活新 turn；
//! 仅有 Info 不激活。

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::messages::BaseMessage;

// ─── MessageKind ─────────────────────────────────────────────────────────────

/// 消息 Kind — 控制循环唤醒行为
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// 外部主动请求 — drain_all 消费，循环结束后到达同样激活
    Prompt,
    /// 延迟到达的结果 — drain_all 消费，循环结束后到达同样激活
    Defer,
    /// 通知性数据 — drain_all 消费，永不唤醒循环
    Info,
}

impl MessageKind {
    /// 是否能唤醒新 turn
    pub fn wakes_up(self) -> bool {
        matches!(self, Self::Prompt | Self::Defer)
    }
}

// ─── MessageSource ───────────────────────────────────────────────────────────

/// 消息来源 — 用于调试和事件追踪
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSource {
    /// 外部用户输入
    UserInput,
    /// SubAgent 完成
    SubAgentComplete,
    /// Goal steering（中途纠正）
    GoalSteering,
    /// Cron 定时触发
    CronTrigger,
    /// Stop hook feedback
    StopHookFeedback,
    /// Channel 消息（微信/Slack 等）
    ChannelMessage,
    /// Hook 系统注入
    SystemInjected,
    /// 工具失败警告
    ToolFailureWarning,
    /// 工作流完成
    WorkflowComplete,
    /// 推测深挖哨兵（SpeculationGuard 注入的提问纪律提醒）
    SpeculationGuard,
}

// ─── QueuedMessage ───────────────────────────────────────────────────────────

/// 一条待投递的消息（v2 富类型）
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    /// 消息 Kind（决定唤醒行为）
    pub kind: MessageKind,
    /// 消息来源
    pub source: MessageSource,
    /// 实际消息内容
    pub message: BaseMessage,
}

impl QueuedMessage {
    pub fn new(kind: MessageKind, source: MessageSource, message: BaseMessage) -> Self {
        Self {
            kind,
            source,
            message,
        }
    }

    /// 快速构造 Prompt 消息（用户输入）
    pub fn prompt(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Prompt, source, message)
    }

    /// 快速构造 Defer 消息（SubAgent/Cron/Channel/Workflow 延迟结果）
    pub fn defer(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Defer, source, message)
    }

    /// 快速构造 Info 消息（SystemReminder/Hook 注入，不唤醒循环）
    pub fn info(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Info, source, message)
    }
}

// ─── MessageQueue ────────────────────────────────────────────────────────────

/// 会话级临时收件箱（v2）
///
/// 内部用 `Arc<Mutex<VecDeque>>` 保证线程安全。`Notify` 用于异步等待新消息。
///
/// RCRA 循环中 Receive 阶段通过 [`drain_all`] 一次性消费全部三类消息；
/// 循环退出后通过 [`has_wake_up`] 检测是否需重新激活。
#[derive(Debug, Clone)]
pub struct MessageQueue {
    inner: Arc<Mutex<VecDeque<QueuedMessage>>>,
    notify: Arc<tokio::sync::Notify>,
}

impl Default for MessageQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageQueue {
    /// 创建空队列
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// 推入一条消息，唤醒等待者
    pub fn push(&self, msg: QueuedMessage) {
        {
            let mut inner = self.inner.lock();
            inner.push_back(msg);
        }
        self.notify.notify_one();
    }

    /// 批量推入消息；空列表为 no-op
    pub fn push_batch(&self, msgs: Vec<QueuedMessage>) {
        if msgs.is_empty() {
            return;
        }
        {
            let mut inner = self.inner.lock();
            inner.extend(msgs);
        }
        self.notify.notify_one();
    }

    /// 排空队列中的全部消息（Prompt + Info + Defer）
    ///
    /// RCRA 循环的 Receive 阶段调用，一次性消费全部类型。
    pub fn drain_all(&self) -> Vec<QueuedMessage> {
        let mut inner = self.inner.lock();
        let drained: Vec<_> = std::mem::take(&mut *inner).into();
        drop(inner);
        self.notify.notify_one();
        drained
    }

    /// 是否有能唤醒循环的消息（Prompt 或 Defer）
    pub fn has_wake_up(&self) -> bool {
        self.inner.lock().iter().any(|m| m.kind.wakes_up())
    }

    /// 队列中是否存在用户 Prompt（SpeculationGuard 区分"用户新输入"与
    /// Info/Defer 系统注入——只有用户 Prompt 才重置推测深挖计数）
    pub fn has_pending_prompt(&self) -> bool {
        self.inner
            .lock()
            .iter()
            .any(|m| m.kind == MessageKind::Prompt)
    }

    /// 队列是否为空
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// 队列长度
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// 清空队列（rewind 操作时调用）
    pub fn clear(&self) {
        self.inner.lock().clear();
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "queue_test.rs"]
mod tests;
