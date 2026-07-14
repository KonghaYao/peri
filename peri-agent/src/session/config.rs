//! SessionConfig — 会话级可变配置
//!
//! 与 SessionStore 的不可变 frozen 数据相反，SessionConfig 在会话生命周期内可变。
//! 通过 `Arc<SessionConfig>` 在 Session、Agent、TurnContext 之间共享。
//! 外部写入（用户切换权限模式、cancel），内部读取（循环检查）。

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

// ─── PermissionMode ──────────────────────────────────────────────────────────

/// 权限模式 — 控制工具执行的审批策略
///
/// 用户可通过 Shift+Tab 在运行时切换。会话内可变，存于 SessionConfig。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// 逐个审批（默认）
    #[default]
    Default,
    /// 自动批准编辑类工具
    AcceptEdit,
    /// LLM 自动分类审批
    Auto,
    /// 全部跳过审批（YOLO）
    Bypass,
}

impl PermissionMode {
    /// 是否启用 HITL 审批
    pub fn hitl_enabled(self) -> bool {
        !matches!(self, Self::Bypass)
    }

    /// 是否需要审批指定工具（基于权限模式 + 工具特征）
    pub fn requires_approval(self, is_edit_tool: bool, in_default_list: bool) -> bool {
        match self {
            Self::Bypass => false,
            Self::AcceptEdit => in_default_list && !is_edit_tool, // 编辑类自动批，其他默认列表工具仍需审批
            Self::Auto => false, // LLM 自动分类（实际审批逻辑在 HITL 中间件中实现）
            Self::Default => in_default_list || is_edit_tool,
        }
    }
}

/// Extended thinking 配置（Anthropic）/ reasoning_effort（OpenAI）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingConfig {
    /// 思考预算（token 数）
    pub budget_tokens: u32,
    /// 努力程度（low/medium/high）
    pub effort: String,
}

// ─── SessionConfig ───────────────────────────────────────────────────────────

/// 会话级可变配置
///
/// 内部用 RwLock 包装可变字段，外部通过 Arc 共享。Cancel token 独立持有，
/// 因为它需要原子的 child_token 创建能力（不能在 RwLock 后面）。
#[derive(Debug)]
pub struct SessionConfig {
    /// 权限模式（用户可实时切换）
    permission_mode: RwLock<PermissionMode>,
    /// Cancel token（会话级，派生 turn 级子 token）
    pub cancel_token: Arc<CancellationToken>,
    /// 单 turn 超时（None 表示无超时）
    turn_timeout: RwLock<Option<Duration>>,
    /// Thinking 配置（None 表示禁用 extended thinking）
    thinking: RwLock<Option<ThinkingConfig>>,
    /// 最大 ReAct 迭代数（防死循环）
    max_iterations: RwLock<usize>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            permission_mode: RwLock::new(PermissionMode::Default),
            cancel_token: Arc::new(CancellationToken::new()),
            turn_timeout: RwLock::new(None),
            thinking: RwLock::new(None),
            max_iterations: RwLock::new(500),
        }
    }
}

impl SessionConfig {
    /// 创建新 SessionConfig
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前权限模式
    pub fn permission_mode(&self) -> PermissionMode {
        *self.permission_mode.read()
    }

    /// 设置权限模式（用户切换）
    pub fn set_permission_mode(&self, mode: PermissionMode) {
        *self.permission_mode.write() = mode;
    }

    /// 当前 turn 超时
    pub fn turn_timeout(&self) -> Option<Duration> {
        *self.turn_timeout.read()
    }

    /// 设置 turn 超时
    pub fn set_turn_timeout(&self, timeout: Option<Duration>) {
        *self.turn_timeout.write() = timeout;
    }

    /// 当前 Thinking 配置
    pub fn thinking(&self) -> Option<ThinkingConfig> {
        self.thinking.read().clone()
    }

    /// 设置 Thinking 配置
    pub fn set_thinking(&self, thinking: Option<ThinkingConfig>) {
        *self.thinking.write() = thinking;
    }

    /// 最大 ReAct 迭代数
    pub fn max_iterations(&self) -> usize {
        *self.max_iterations.read()
    }

    /// 设置最大迭代数
    pub fn set_max_iterations(&self, n: usize) {
        *self.max_iterations.write() = n;
    }

    /// 是否已取消
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    /// 触发 cancel
    pub fn cancel(&self) {
        self.cancel_token.cancel();
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
