//! Goal 子系统的核心数据模型。
//!
//! `ThreadGoal` 是事实数据，必须跨 session 持久化。
//! `GoalStatus` 是状态机枚举，转换规则见 `can_transition_to`。
//! `GoalAccounting` 是计费状态（token/time 增量累积）。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

#[cfg(test)]
#[path = "model_test.rs"]
mod tests;
