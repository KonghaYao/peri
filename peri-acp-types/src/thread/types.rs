use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use std::time::SystemTime;
use thiserror::Error;

/// Thread 唯一标识符（UUID v7，按时间排序）
pub type ThreadId = String;

/// agent 取消策略（强类型枚举，杜绝非法字符串）
///
/// Making Illegal States Unrepresentable：原 String 字段允许任意字符串，
/// 现在使用强类型枚举约束取值集合。持久化层（SQLite）以 `as_str()` 输出
/// 的字符串为准，读取时通过 `FromStr` 解析，遇到非法值直接报错（不静默 fallback）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CancelPolicy {
    /// 同步子 agent：跟随父 agent 取消
    #[default]
    Cascade,
    /// Background 子 agent：仅跟随 session 根取消
    Independent,
}

impl CancelPolicy {
    /// 序列化为稳定的小写字符串，用于 SQLite 列值
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cascade => "cascade",
            Self::Independent => "independent",
        }
    }
}

impl fmt::Display for CancelPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CancelPolicy {
    type Err = ThreadMetaParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cascade" => Ok(Self::Cascade),
            "independent" => Ok(Self::Independent),
            other => Err(ThreadMetaParseError::invalid_cancel_policy(other)),
        }
    }
}

/// agent 运行时状态（强类型枚举，杜绝非法字符串）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    #[default]
    Active,
    Done,
    Cancelled,
    Error,
}

impl AgentStatus {
    /// 序列化为稳定的小写字符串，用于 SQLite 列值
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
        }
    }

    /// 是否仍处于活跃状态
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentStatus {
    type Err = ThreadMetaParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "done" => Ok(Self::Done),
            "cancelled" => Ok(Self::Cancelled),
            "error" => Ok(Self::Error),
            other => Err(ThreadMetaParseError::invalid_agent_status(other)),
        }
    }
}

/// durable 工作意图：中断后是否允许进入会话时自动续跑。
///
/// 与实时运行状态（`AgentStatus`）分开：它只记录「下次进入会话是否继续」，不表达
/// TUI loading 或 TaskManager 的当前忙碌状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    /// 不自动续跑。
    #[default]
    Idle,
    /// 允许进入会话后自动续跑。
    Working,
    /// 保留显式停止意图，不自动续跑。
    Stopped,
}

impl WorkState {
    /// 序列化为稳定的蛇形字符串，用于 SQLite 列值
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Stopped => "stopped",
        }
    }

    /// 一轮 prompt 收尾后的 durable 工作意图（见 `docs/design/session-durability.md` §2.1）。
    ///
    /// 用户取消保留显式停止意图；正常完成、明确失败与交还控制都回到不自动续跑。
    /// 持久化结果不确定时返回 `None`：最后提交的值才是权威事实，本次不能覆盖它。
    pub fn after_prompt_turn(
        stop_reason: crate::command::PromptStopReason,
        persistence_inconsistent: bool,
    ) -> Option<Self> {
        if persistence_inconsistent {
            return None;
        }
        Some(match stop_reason {
            crate::command::PromptStopReason::Cancelled => Self::Stopped,
            _ => Self::Idle,
        })
    }

    /// 子 agent 终态对应的 durable 工作意图：被取消保留停止意图，其余收尾不再续跑。
    pub fn after_agent_terminal(status: AgentStatus) -> Self {
        match status {
            AgentStatus::Cancelled => Self::Stopped,
            _ => Self::Idle,
        }
    }
}

impl fmt::Display for WorkState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WorkState {
    type Err = ThreadMetaParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "idle" => Ok(Self::Idle),
            "working" => Ok(Self::Working),
            "stopped" => Ok(Self::Stopped),
            other => Err(ThreadMetaParseError::invalid_work_state(other)),
        }
    }
}

/// 强类型枚举解析失败错误
///
/// 关键约束：禁止 `_ => default` 静默 fallback。任何来自 DB / 外部字符串
/// 的非法值必须以错误形式上抛，由调用方决定处理策略。
#[derive(Debug, Error)]
pub enum ThreadMetaParseError {
    #[error("非法 cancel_policy 值: {0:?}（合法值: cascade / independent）")]
    InvalidCancelPolicy(String),

    #[error("非法 agent_status 值: {0:?}（合法值: active / done / cancelled / error）")]
    InvalidAgentStatus(String),

    #[error("非法 work_state 值: {0:?}（合法值: idle / working / stopped）")]
    InvalidWorkState(String),
}

impl ThreadMetaParseError {
    pub fn invalid_cancel_policy(s: impl Into<String>) -> Self {
        Self::InvalidCancelPolicy(s.into())
    }

    pub fn invalid_agent_status(s: impl Into<String>) -> Self {
        Self::InvalidAgentStatus(s.into())
    }

    pub fn invalid_work_state(s: impl Into<String>) -> Self {
        Self::InvalidWorkState(s.into())
    }
}

/// Thread 列表的轻量投影，不包含消息内容聚合或完整配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadListEntry {
    pub id: ThreadId,
    pub title: Option<String>,
    pub cwd: String,
    pub message_count: usize,
    pub updated_at: DateTime<Utc>,
}

impl From<ThreadMeta> for ThreadListEntry {
    fn from(meta: ThreadMeta) -> Self {
        Self {
            id: meta.id,
            title: meta.title,
            cwd: meta.cwd,
            message_count: meta.message_count,
            updated_at: meta.updated_at,
        }
    }
}

/// Thread 元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadMeta {
    pub id: ThreadId,
    /// 对话标题，可由第一条用户消息自动截取
    pub title: Option<String>,
    /// 创建时的工作目录
    pub cwd: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: usize,
    /// 消息内容总字节数（由 list_threads 查询时计算）
    #[serde(default)]
    pub content_size: u64,
    /// 父 agent thread ID，None = 根 agent
    #[serde(default)]
    pub parent_thread_id: Option<String>,
    /// 快照截止消息 ID
    #[serde(default)]
    pub snapshot_at_message_id: Option<String>,
    /// true = 子 agent，不显示在主列表
    #[serde(default)]
    pub hidden: bool,
    /// 取消策略（强类型，旧 JSON 缺失时默认 Cascade）
    #[serde(default)]
    pub cancel_policy: CancelPolicy,
    /// JSON 完整配置快照
    #[serde(default)]
    pub config: Option<String>,
    /// agent 运行状态（强类型，旧 JSON 缺失时默认 Active）
    #[serde(default)]
    pub agent_status: AgentStatus,
    /// durable 工作意图（旧 JSON 缺失时默认 Idle）
    #[serde(default)]
    pub work_state: WorkState,
    /// 最近一次有效上下文占用的 token 数；`None` 表示未知，不是零占用。
    #[serde(default)]
    pub context_usage_tokens: Option<u64>,
}

impl ThreadMeta {
    /// Set creation/update fields from the supplied wall-clock value.
    /// The UUID v7 identity keeps its own monotonic generation sequence.
    pub fn new_at(cwd: impl Into<String>, at: SystemTime) -> Self {
        let now = at.into();
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            title: None,
            cwd: cwd.into(),
            created_at: now,
            updated_at: now,
            message_count: 0,
            content_size: 0,
            parent_thread_id: None,
            snapshot_at_message_id: None,
            hidden: false,
            cancel_policy: CancelPolicy::default(),
            config: None,
            agent_status: AgentStatus::default(),
            work_state: WorkState::default(),
            context_usage_tokens: None,
        }
    }

    /// 是否为根 agent（无父线程）
    pub fn is_root(&self) -> bool {
        self.parent_thread_id.is_none()
    }

    /// 用于从 DB 行构建 ThreadMeta 时填充新字段的默认值
    pub fn default_for_db_at(at: SystemTime) -> Self {
        let now = at.into();
        Self {
            id: String::new(),
            title: None,
            cwd: String::new(),
            created_at: now,
            updated_at: now,
            message_count: 0,
            content_size: 0,
            parent_thread_id: None,
            snapshot_at_message_id: None,
            hidden: false,
            cancel_policy: CancelPolicy::default(),
            config: None,
            agent_status: AgentStatus::default(),
            work_state: WorkState::default(),
            context_usage_tokens: None,
        }
    }
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
