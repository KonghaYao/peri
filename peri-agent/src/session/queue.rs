//! MessageQueue v2 — 会话级临时收件箱
//!
//! 独立于 SessionStore，会话内持续可变。**不持久化**——Session 重建时从空开始。
//!
//! ## 消息分三类（控制循环唤醒和消费行为）
//!
//! | Kind | 来源 | Receive 行为 | End 行为 | 唤醒新 turn |
//! |------|------|-------------|---------|------------|
//! | `Prompt` | 外部用户输入、外部主动请求 | 消费（写入 Transcript） | 可唤醒 | ✅ |
//! | `Defer` | SubAgent 完成、Cron 触发、延迟结果 | 跳过（保留） | 消费 + 唤醒 | ✅ |
//! | `Info` | SystemReminder、Hook 注入 | 消费（写入 Transcript） | 不唤醒 | ❌ |
//!
//! ReAct 循环退出后，若队列新到达 Prompt 或 Defer，重新激活新 turn；
//! 仅有 Info 不激活（Info 必须被 Prompt 带出或单独消费）。

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::messages::BaseMessage;

// ─── MessageKind ─────────────────────────────────────────────────────────────

/// 消息 Kind — 控制循环唤醒和消费行为
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// 外部主动请求 — Receive 消费，End 可唤醒，循环结束后到达同样激活
    Prompt,
    /// 延迟到达的结果 — Receive 跳过，End 可唤醒，循环结束后到达同样激活
    Defer,
    /// 通知性数据 — 仅 Receive 消费，永不唤醒循环
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
}

// ─── QueuedMessage ───────────────────────────────────────────────────────────

/// 一条待投递的消息（v2 富类型）
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    /// 消息 Kind（决定消费行为）
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

    /// 快速构造 Defer 消息（SubAgent 完成、Cron 触发）
    pub fn defer(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Defer, source, message)
    }

    /// 快速构造 Info 消息（SystemReminder）
    pub fn info(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Info, source, message)
    }
}

// ─── MessageQueue ────────────────────────────────────────────────────────────

/// 会话级临时收件箱（v2）
///
/// 内部用 `Arc<Mutex<VecDeque>>` 保证线程安全。`Notify` 用于异步等待新消息。
/// 与 v1 的区别：
/// - 消息带 Kind（Prompt/Defer/Info），控制循环唤醒
/// - 接受 `BaseMessage` 而非 `String`，富类型
/// - 提供 `drain_for_receive` / `drain_for_end` 两套排空 API
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

    /// Receive 阶段：取出并消费所有 Prompt + Info，Defer 保留在队列中
    ///
    /// 返回的 Vec 按 push 顺序排列。Defer 消息留在队列里，等待 End 阶段或下个 turn。
    pub fn drain_for_receive(&self) -> Vec<QueuedMessage> {
        let mut inner = self.inner.lock();
        let mut consumed = Vec::new();
        let mut deferred = VecDeque::new();

        while let Some(msg) = inner.pop_front() {
            match msg.kind {
                MessageKind::Defer => deferred.push_back(msg),
                MessageKind::Prompt | MessageKind::Info => consumed.push(msg),
            }
        }

        // Defer 放回队列尾部（保持原相对顺序）
        *inner = deferred;
        consumed
    }

    /// End 阶段：检查队列是否有 Prompt 或 Defer
    ///
    /// - 有 → 取出全部 Prompt + Defer，返回 `Some(messages)` 激活新 turn
    /// - 无（队列空或仅有 Info）→ 返回 `None`，循环退出
    ///
    /// 注意：Info 永远不会被 End 阶段单独消费——必须被 Prompt 带出。
    /// 此处仅检查唤醒条件，不消费 Info。
    pub fn drain_for_end(&self) -> Option<Vec<QueuedMessage>> {
        let mut inner = self.inner.lock();
        let has_wake = inner.iter().any(|m| m.kind.wakes_up());

        if !has_wake {
            return None;
        }

        // 取出 Prompt + Defer，保留 Info
        let mut consumed = Vec::new();
        let mut retained = VecDeque::new();

        while let Some(msg) = inner.pop_front() {
            match msg.kind {
                MessageKind::Info => retained.push_back(msg),
                MessageKind::Prompt | MessageKind::Defer => consumed.push(msg),
            }
        }

        *inner = retained;
        Some(consumed)
    }

    /// 异步等待新消息到达（用于循环退出后阻塞）
    ///
    /// 与 `drain_for_end` 配合：先 drain_for_end，None 则 wait_for_message。
    pub async fn wait_for_message(&self) {
        self.notify.notified().await;
    }

    /// 是否有能唤醒循环的消息（Prompt 或 Defer）
    pub fn has_wake_up(&self) -> bool {
        self.inner.lock().iter().any(|m| m.kind.wakes_up())
    }

    /// 队列是否为空
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// 队列长度
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// 获取 Notify 的克隆（用于自定义等待逻辑）
    pub fn notifier(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.notify)
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
