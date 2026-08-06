//! Goal Steering 契约（自 peri-agent 迁入；`peri-agent::goal` 保留 re-export）。
//!
//! 纯数据模型 + store/controller trait，无 ACP/middleware 依赖。
//! 并发状态机（`GoalState`）在 peri-acp `session::goal_state`。
//! BRIDGE 设计：`GoalController` / `GoalStateView` 是 peri-middlewares ↔ peri-acp
//! 的解耦 trait（避免循环依赖），事实源随契约归位到本层。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

// ─── model ─────────────────────────────────────────────────────────────────────

/// Goal 状态机
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// 活跃，continuation 可运行
    Active,
    /// Agent 宣告完成
    Complete,
    /// Agent 宣告阻塞（必须附带 reason）
    Blocked,
}

impl GoalStatus {
    /// 检查状态转换是否合法
    pub fn can_transition_to(&self, target: &GoalStatus) -> bool {
        use GoalStatus::*;
        match (self, target) {
            // 终态不可转换
            (Complete, _) | (Blocked, _) => false,
            // Active → Complete / Blocked
            (Active, Complete | Blocked) => true,
            (Active, Active) => false,
        }
    }

    /// 是否是终态（continuation 应停止）
    pub fn is_terminal(&self) -> bool {
        use GoalStatus::*;
        matches!(self, Complete | Blocked)
    }
}

impl std::fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use GoalStatus::*;
        match self {
            Active => write!(f, "active"),
            Complete => write!(f, "complete"),
            Blocked => write!(f, "blocked"),
        }
    }
}

/// 计费状态（累积增量）
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalAccounting {
    /// 已用 token（含 input + output - cache_read）
    pub tokens_used: u64,
    /// 已用时间（秒）
    pub time_used_seconds: u64,
}

/// Thread-level goal 事实数据（持久化）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadGoal {
    /// 唯一标识（uuid v7）
    pub goal_id: String,
    /// 目标描述
    pub objective: String,
    /// 当前状态
    pub status: GoalStatus,
    /// Token 预算上限（None = 无上限）
    pub token_budget: Option<u64>,
    /// 阻塞原因（仅 Blocked 状态有值）
    pub blocked_reason: Option<String>,
    /// 计费状态
    pub accounting: GoalAccounting,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 最后更新时间
    pub updated_at: DateTime<Utc>,
}

impl ThreadGoal {
    pub fn new(objective: String, token_budget: Option<u64>) -> Self {
        let now = Utc::now();
        Self {
            goal_id: uuid::Uuid::now_v7().to_string(),
            objective,
            status: GoalStatus::Active,
            token_budget,
            blocked_reason: None,
            accounting: GoalAccounting::default(),
            created_at: now,
            updated_at: now,
        }
    }

    /// usage 百分比（0.0-1.0），budget=None 时返回 None
    pub fn usage_pct(&self) -> Option<f32> {
        self.token_budget
            .filter(|&b| b > 0)
            .map(|b| self.accounting.tokens_used as f32 / b as f32)
    }
}

// ─── store ─────────────────────────────────────────────────────────────────────

/// Goal 持久化存储 trait
#[async_trait]
pub trait GoalStore: Send + Sync {
    /// 保存（upsert）goal 到指定 thread
    async fn save(&self, thread_id: &str, goal: ThreadGoal) -> Result<(), GoalStoreError>;

    /// 加载指定 thread 的 goal，无 goal 返回 None
    async fn load(&self, thread_id: &str) -> Result<Option<ThreadGoal>, GoalStoreError>;

    /// 删除指定 thread 的 goal
    async fn delete(&self, thread_id: &str) -> Result<(), GoalStoreError>;
}

/// Store 错误类型
#[derive(Debug, thiserror::Error)]
pub enum GoalStoreError {
    #[error("存储 IO 错误: {0}")]
    Io(String),
    #[error("序列化错误: {0}")]
    Serde(String),
}

/// 纯内存实现（测试 + fallback）
pub struct InMemoryGoalStore {
    inner: Arc<RwLock<HashMap<String, ThreadGoal>>>,
}

impl InMemoryGoalStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryGoalStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GoalStore for InMemoryGoalStore {
    async fn save(&self, thread_id: &str, goal: ThreadGoal) -> Result<(), GoalStoreError> {
        self.inner.write().insert(thread_id.to_string(), goal);
        Ok(())
    }

    async fn load(&self, thread_id: &str) -> Result<Option<ThreadGoal>, GoalStoreError> {
        Ok(self.inner.read().get(thread_id).cloned())
    }

    async fn delete(&self, thread_id: &str) -> Result<(), GoalStoreError> {
        self.inner.write().remove(thread_id);
        Ok(())
    }
}

// ─── controller ────────────────────────────────────────────────────────────────

/// Goal 读写控制器接口（供 Goal 工具和 GoalMiddleware 依赖注入）。
#[async_trait]
pub trait GoalController: Send + Sync {
    /// 创建 goal。如果 goal 已存在返回 Err。
    async fn create_goal(&self, objective: String) -> Result<(), String>;

    /// 声明完成。状态转换非法时返回 Err。
    async fn complete_goal(&self) -> Result<(), String>;

    /// 声明阻塞。reason 必填。状态转换非法时返回 Err。
    async fn block_goal(&self, reason: String) -> Result<(), String>;

    /// 清除当前 goal（释放 singleton 槽位，终态也可清除）。
    async fn clear_goal(&self) -> Result<(), String>;

    /// 只读快照（get action + after_agent 判断用）
    fn snapshot(&self) -> GoalViewSnapshot;
}

/// GoalController 的补充视图（after_agent 只需判断 active）
pub fn is_active(snap: &GoalViewSnapshot) -> bool {
    snap.status == Some(GoalStatus::Active)
}

// ─── view ──────────────────────────────────────────────────────────────────────

/// 只读快照（与 GoalSnapshot 平行，但定义在契约层避免依赖）
#[derive(Debug, Clone, Default)]
pub struct GoalViewSnapshot {
    pub objective: Option<String>,
    pub status: Option<GoalStatus>,
    pub token_budget: Option<u64>,
    pub tokens_used: u64,
    pub objective_just_updated: bool,
}

/// GoalState 的抽象视图（供 middleware 依赖注入）
pub trait GoalStateView: Send + Sync {
    /// 只读快照
    fn snapshot(&self) -> GoalViewSnapshot;

    /// 消费 objective_just_updated 标志
    fn consume_objective_updated(&self) -> bool;
}
