//! Queue payload、独立消息策略与有序 Receive 接纳。

use super::{ExecutionBinding, MessageDisposition, MessagePolicy};
use crate::{
    messages::{BaseMessage, MessageId},
    system_reminder::TrustedSystemReminder,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{collections::VecDeque, sync::Arc};

// ─── MessageKind ─────────────────────────────────────────────────────────────

/// 消息展示标签；运行调度以 MessagePolicy 为准。
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
    /// 此展示标签的默认策略；显式策略可覆盖，不能用于运行调度。
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
    /// 后台 Shell 完成
    ShellComplete,
    /// Goal steering（中途纠正）
    GoalSteering,
    /// Todo steering（requireCompletion 续跑提醒）
    TodoSteering,
    /// Cron 定时触发
    CronTrigger,
    /// Stop hook feedback
    StopHookFeedback,
    /// Channel 消息（微信/Slack 等）
    ChannelMessage,
    /// Dynamic MCP lifecycle notification.
    DynamicMcpNotification,
    /// Hook 系统注入
    SystemInjected,
    /// Hook 的显式停止意图（continue:false）：经 Receive 唯一出口停止当前 run，
    /// 不按可唤醒消息处理，也不发起额外模型请求。
    HookStopIntent,
    /// 工具失败警告
    ToolFailureWarning,
    /// 工作流完成
    WorkflowComplete,
}

// ─── QueuedMessage ───────────────────────────────────────────────────────────

/// Queue payload. Scheduling (`MessageKind`) is deliberately orthogonal to content semantics.
#[derive(Debug, Clone)]
pub enum QueuedPayload {
    Message(BaseMessage),
    SystemReminder(TrustedSystemReminder),
}

/// 一条待投递的消息（v2 富类型）
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    /// 消息 Kind（决定唤醒行为）
    pub kind: MessageKind,
    /// 消息来源
    pub source: MessageSource,
    /// 实际消息内容
    pub payload: QueuedPayload,
    /// Stable canonical ID for an owner-confirmed terminal reminder.
    pub delivery_id: Option<MessageId>,
    pub policy: MessagePolicy,
    pub admission_sequence: Option<u64>,
}

impl QueuedMessage {
    pub fn new(kind: MessageKind, source: MessageSource, message: BaseMessage) -> Self {
        Self::with_payload(kind, source, QueuedPayload::Message(message))
    }

    pub fn with_payload(kind: MessageKind, source: MessageSource, payload: QueuedPayload) -> Self {
        let mut policy = match kind {
            MessageKind::Prompt | MessageKind::Defer => MessagePolicy::ensure_processing(),
            MessageKind::Info => MessagePolicy::passive(),
        };
        if let QueuedPayload::SystemReminder(reminder) = &payload {
            policy.model_visible = reminder
                .as_reminder()
                .audiences
                .0
                .contains(&crate::system_reminder::ReminderAudience::Model);
        }
        Self {
            kind,
            source,
            payload,
            delivery_id: None,
            policy,
            admission_sequence: None,
        }
    }

    pub fn with_policy(mut self, mut policy: MessagePolicy) -> Self {
        if let QueuedPayload::SystemReminder(reminder) = &self.payload {
            policy.model_visible &= reminder
                .as_reminder()
                .audiences
                .0
                .contains(&crate::system_reminder::ReminderAudience::Model);
        }
        self.policy = policy;
        self
    }

    pub fn system_reminder_with_delivery_id(
        kind: MessageKind,
        source: MessageSource,
        reminder: TrustedSystemReminder,
        delivery_id: MessageId,
    ) -> Self {
        let mut message = Self::system_reminder(kind, source, reminder);
        message.delivery_id = Some(delivery_id);
        message
    }

    pub fn system_reminder(
        kind: MessageKind,
        source: MessageSource,
        reminder: TrustedSystemReminder,
    ) -> Self {
        Self::with_payload(kind, source, QueuedPayload::SystemReminder(reminder))
    }

    pub fn message(&self) -> Option<&BaseMessage> {
        match &self.payload {
            QueuedPayload::Message(message) => Some(message),
            QueuedPayload::SystemReminder(_) => None,
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
/// 消息与唤醒状态由同一个共享 owner 持有，所有发布入口使用同一信号。
///
/// RCRA 循环中 Receive 阶段通过 [`Self::drain_all`] 一次性消费全部三类消息；
/// 循环退出后通过 [`Self::has_wake_up`] 检测是否需重新激活。
#[derive(Debug, Clone)]
pub struct MessageQueue {
    state: Arc<MailboxState>,
}

#[derive(Debug)]
struct MailboxState {
    messages: Mutex<VecDeque<QueuedMessage>>,
    wake: tokio::sync::Notify,
    wake_version: tokio::sync::watch::Sender<u64>,
    next_sequence: AtomicU64,
    suppressed: Mutex<Vec<QueuedMessage>>,
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
            state: Arc::new(MailboxState {
                messages: Mutex::new(VecDeque::new()),
                wake: tokio::sync::Notify::new(),
                wake_version: tokio::sync::watch::channel(0).0,
                next_sequence: AtomicU64::new(1),
                suppressed: Mutex::new(Vec::new()),
            }),
        }
    }

    /// 发布消息；Prompt/Defer 唤醒等待者，Info 只保留到下次消费。
    pub fn push(&self, mut msg: QueuedMessage) {
        let should_wake = msg.policy.notifies_execution();
        {
            let mut inner = self.state.messages.lock();
            if msg.admission_sequence.is_none() {
                msg.admission_sequence =
                    Some(self.state.next_sequence.fetch_add(1, Ordering::Relaxed));
            }
            inner.push_back(msg);
        }
        if should_wake {
            self.state.wake.notify_waiters();
            self.state
                .wake_version
                .send_modify(|version| *version = version.saturating_add(1));
        }
    }

    /// 批量推入消息；空列表为 no-op
    pub fn push_batch(&self, mut msgs: Vec<QueuedMessage>) {
        if msgs.is_empty() {
            return;
        }
        let should_wake = msgs
            .iter()
            .any(|message| message.policy.notifies_execution());
        {
            let mut inner = self.state.messages.lock();
            for message in &mut msgs {
                if message.admission_sequence.is_none() {
                    message.admission_sequence =
                        Some(self.state.next_sequence.fetch_add(1, Ordering::Relaxed));
                }
            }
            inner.extend(msgs);
            inner
                .make_contiguous()
                .sort_by_key(|message| message.admission_sequence);
        }
        if should_wake {
            self.state.wake.notify_waiters();
            self.state
                .wake_version
                .send_modify(|version| *version = version.saturating_add(1));
        }
    }

    pub fn subscribe_wake(&self) -> tokio::sync::watch::Receiver<u64> {
        self.state.wake_version.subscribe()
    }

    /// 非破坏性等待可执行消息，注册通知后再检查状态以避免丢唤醒。
    pub async fn await_wake(&self) {
        loop {
            let notified = self.state.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.has_wake_up() {
                return;
            }
            notified.await;
        }
    }

    pub async fn await_wake_for_run(&self, execution: &ExecutionBinding) {
        loop {
            let notified = self.state.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.has_required_for_run(execution) {
                return;
            }
            notified.await;
        }
    }

    /// 排空队列中的全部消息（Prompt + Info + Defer）
    ///
    /// RCRA 循环的 Receive 阶段调用，一次性消费全部类型。
    pub fn drain_all(&self) -> Vec<QueuedMessage> {
        let mut inner = self.state.messages.lock();
        std::mem::take(&mut *inner).into()
    }

    pub fn drain_batch(&self, limit: usize) -> Vec<QueuedMessage> {
        let mut inner = self.state.messages.lock();
        let count = inner.len().min(limit);
        inner.drain(..count).collect()
    }

    pub fn suppress(&self, message: QueuedMessage) {
        self.state.suppressed.lock().push(message);
    }

    pub fn suppressed_messages(&self) -> Vec<QueuedMessage> {
        self.state.suppressed.lock().clone()
    }

    pub fn has_required_for_run(&self, execution: &ExecutionBinding) -> bool {
        self.state
            .messages
            .lock()
            .iter()
            .any(|message| message.policy.disposition(execution) == MessageDisposition::Process)
    }

    pub fn has_ensure_processing(&self) -> bool {
        self.state
            .messages
            .lock()
            .iter()
            .any(|message| message.policy.ensures_processing())
    }

    pub fn has_required(&self) -> bool {
        self.state
            .messages
            .lock()
            .iter()
            .any(|message| message.policy.requirement == super::MessageRequirement::Required)
    }

    /// 与 Receive 领取共享同一锁，仅撤出尚未被领取的指定用户输入。
    pub fn withdraw_user_inputs(&self, ids: &[crate::messages::MessageId]) -> Vec<QueuedMessage> {
        let mut inner = self.state.messages.lock();
        let mut withdrawn = Vec::new();
        let mut kept = VecDeque::with_capacity(inner.len());
        for message in inner.drain(..) {
            if message.source == MessageSource::UserInput
                && matches!(message.message(), Some(BaseMessage::Human { id, .. }) if ids.contains(id))
            {
                withdrawn.push(message);
            } else {
                kept.push_back(message);
            }
        }
        *inner = kept;
        withdrawn
    }

    /// 是否有要求新执行处理的消息。
    pub fn has_wake_up(&self) -> bool {
        self.has_ensure_processing()
    }

    /// 按来源查询 required 消息，仅用于诊断；不授予来源调度权。
    pub fn has_pending_defer(&self, source: &MessageSource) -> bool {
        self.state
            .messages
            .lock()
            .iter()
            .any(|message| message.policy.ensures_processing() && &message.source == source)
    }

    /// 是否仍需建立执行处理 required MQ 消息。
    pub fn needs_mq_continuation(&self) -> bool {
        self.has_ensure_processing()
    }

    /// 队列是否为空
    pub fn is_empty(&self) -> bool {
        self.state.messages.lock().is_empty()
    }

    /// 队列长度
    pub fn len(&self) -> usize {
        self.state.messages.lock().len()
    }

    /// 清空队列（rewind 操作时调用）
    pub fn clear(&self) {
        self.state.messages.lock().clear();
    }
}

#[cfg(test)]
#[path = "queue_wake_test.rs"]
mod wake_tests;
