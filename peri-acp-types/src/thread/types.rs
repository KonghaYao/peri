use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
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
}

impl ThreadMetaParseError {
    pub fn invalid_cancel_policy(s: impl Into<String>) -> Self {
        Self::InvalidCancelPolicy(s.into())
    }

    pub fn invalid_agent_status(s: impl Into<String>) -> Self {
        Self::InvalidAgentStatus(s.into())
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
    /// 物化缓存
    #[serde(default)]
    pub cached_context: Option<String>,
    /// agent 运行状态（强类型，旧 JSON 缺失时默认 Active）
    #[serde(default)]
    pub agent_status: AgentStatus,
}

impl ThreadMeta {
    pub fn new(cwd: impl Into<String>) -> Self {
        let now = Utc::now();
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
            cached_context: None,
            agent_status: AgentStatus::default(),
        }
    }

    /// 是否为根 agent（无父线程）
    pub fn is_root(&self) -> bool {
        self.parent_thread_id.is_none()
    }

    /// 用于从 DB 行构建 ThreadMeta 时填充新字段的默认值
    pub fn default_for_db() -> Self {
        Self {
            id: String::new(),
            title: None,
            cwd: String::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            message_count: 0,
            content_size: 0,
            parent_thread_id: None,
            snapshot_at_message_id: None,
            hidden: false,
            cancel_policy: CancelPolicy::default(),
            config: None,
            cached_context: None,
            agent_status: AgentStatus::default(),
        }
    }
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
