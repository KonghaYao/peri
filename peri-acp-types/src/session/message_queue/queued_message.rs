//! Queued payload and source contracts.

use super::super::mq::{MqEntry, MqInput};
use super::super::{MessagePolicy, PluginSelection};
use crate::messages::{BaseMessage, MessageId};
use crate::store::PersistedPayload;
use crate::system_reminder::TrustedSystemReminder;
use serde::{Deserialize, Serialize};

// ─── MessageKind ─────────────────────────────────────────────────────────────

/// 消息展示标签；运行调度以 MessagePolicy 为准。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// 外部主动请求 — Receive 消费，循环结束后到达同样激活
    Prompt,
    /// 延迟到达的结果 — Receive 消费，循环结束后到达同样激活
    Defer,
    /// 通知性数据 — Receive 消费，永不唤醒循环
    Info,
}

impl MessageKind {
    /// 此展示标签的默认策略；显式策略可覆盖，不能用于运行调度。
    #[must_use]
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
    /// MCP subscription or lifecycle notification.
    #[serde(alias = "dynamic_mcp_notification")]
    McpNotification,
    /// Hook 系统注入
    SystemInjected,
    /// Hook 的显式停止意图（continue:false）：经 Receive 唯一出口停止当前 run，
    /// 不按可唤醒消息处理，也不发起额外模型请求。
    HookStopIntent,
    /// 工具失败警告
    ToolFailureWarning,
    /// 工作流完成
    WorkflowComplete,
    /// 进入会话时的恢复提醒（进程中断后从已提交历史继续）。
    ///
    /// 只有 Host 恢复协调以可信 SystemReminder 构造；旧行没有该来源，反序列化不受影响。
    SessionRecovery,
}

impl MessageSource {
    /// 四条后台终态来源；Receive 据此识别 canonical 后台完成，不按 task kind 判定。
    #[must_use]
    pub const fn is_background_completion(&self) -> bool {
        matches!(
            self,
            Self::SubAgentComplete
                | Self::ShellComplete
                | Self::WorkflowComplete
                | Self::McpNotification
        )
    }
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
    /// 消息 Kind（决定默认策略）
    pub kind: MessageKind,
    /// 消息来源
    pub source: MessageSource,
    /// 实际消息内容
    pub payload: QueuedPayload,
    /// Stable canonical ID for an owner-confirmed terminal reminder.
    pub delivery_id: Option<MessageId>,
    pub policy: MessagePolicy,
    /// 旧内存队列的接纳序号；MQ 用 `(enqueued_at, id)` 定序，本字段不再参与排序。
    pub admission_sequence: Option<u64>,
    /// 用户输入的原始编辑器文本；其他来源为 `None`。
    pub original_draft: Option<String>,
    /// 用户输入随行冻结的插件选择；其他来源为空。
    pub plugin_selections: Vec<PluginSelection>,
    /// 存储生成的入队时间；只有从 MQ 读回的条目有值。
    pub enqueued_at: Option<String>,
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
            original_draft: None,
            plugin_selections: Vec::new(),
            enqueued_at: None,
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

    /// 附带用户输入的原始编辑器文本（只有待发送区的用户输入需要）。
    pub fn with_original_draft(mut self, draft: impl Into<String>) -> Self {
        self.original_draft = Some(draft.into());
        self
    }

    /// 附带与本次用户输入冻结的插件选择。
    pub fn with_plugin_selections(mut self, selections: Vec<PluginSelection>) -> Self {
        self.plugin_selections = selections;
        self
    }

    /// 附带入队时间；仅用于从 MQ 行构造视图，入队请求不接受调用方指定时间。
    pub fn with_enqueued_at(mut self, enqueued_at: impl Into<String>) -> Self {
        self.enqueued_at = Some(enqueued_at.into());
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
    #[must_use]
    pub fn prompt(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Prompt, source, message)
    }

    /// 快速构造 Defer 消息（SubAgent/Cron/Channel/Workflow 延迟结果）
    #[must_use]
    pub fn defer(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Defer, source, message)
    }

    /// 快速构造 Info 消息（SystemReminder/Hook 注入，不唤醒循环）
    #[must_use]
    pub fn info(source: MessageSource, message: BaseMessage) -> Self {
        Self::new(MessageKind::Info, source, message)
    }

    /// 稳定投递身份：`Message` 用载荷 id，`SystemReminder` 用已固定的 `delivery_id`。
    ///
    /// 未固定的提醒每次调用都会得到新 id——需要稳定身份时用 [`super::MessageQueue::enqueue`]
    /// 返回的 `outcome.entry.id`（入队时只固定一次）。
    pub fn id(&self) -> MessageId {
        match &self.payload {
            QueuedPayload::Message(message) => message.id(),
            QueuedPayload::SystemReminder(_) => self.delivery_id.unwrap_or_default(),
        }
    }

    /// 转为入队请求；`SystemReminder` 的 `delivery_id` 在这里固定一次，行 id 与载荷 id 一致。
    pub fn into_mq_input(mut self) -> MqInput {
        let payload = match self.payload {
            QueuedPayload::Message(message) => PersistedPayload::Message(message),
            QueuedPayload::SystemReminder(reminder) => {
                let id = self.delivery_id.unwrap_or_default();
                self.delivery_id = Some(id);
                PersistedPayload::SystemReminder { id, reminder }
            }
        };
        MqInput {
            id: payload.id(),
            kind: self.kind,
            source: self.source,
            payload,
            policy: self.policy,
            original_draft: self.original_draft,
            plugin_selections: self.plugin_selections,
        }
    }

    /// MQ 行 → 队列视图：策略以存储行为准，不按 kind 重算。
    pub fn from_mq_entry(entry: MqEntry) -> Self {
        let MqEntry {
            id: _,
            enqueued_at,
            kind,
            source,
            payload,
            policy,
            original_draft,
            plugin_selections,
        } = entry;
        let (payload, delivery_id) = match payload {
            PersistedPayload::Message(message) => (QueuedPayload::Message(message), None),
            PersistedPayload::SystemReminder { id, reminder } => {
                (QueuedPayload::SystemReminder(reminder), Some(id))
            }
        };
        Self {
            kind,
            source,
            payload,
            delivery_id,
            policy,
            admission_sequence: None,
            original_draft,
            plugin_selections,
            enqueued_at: Some(enqueued_at),
        }
    }
}
