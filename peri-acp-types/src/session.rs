//! Session 层契约类型（自 peri-agent 迁入；`peri-agent::session::{queue,turn,runtime}`
//! 与 `peri-agent::agent::session::{inbox,cron_owner}` 保留 re-export 保兼容）。
//!
//! 归位说明（§0 兜底：接口契约归 peri-acp-types）：MQ 消息管理、turn 身份、
//! inbox 唤醒、cron 触发桥、Agent 运行时注册表条目是跨层接口契约——
//! Agent 层持有实现与执行权，ACP / middlewares 只依赖本层契约类型。

mod cron_owner;
mod execution;
mod inbox;
mod queue;
mod runtime;

use crate::{command_registry::CommandRegistry, mcp_skills::McpSkillRegistry};
use serde::{Deserialize, Serialize};
use std::{sync::atomic::AtomicBool, sync::Arc};

pub use cron_owner::CronOwner;
pub use execution::{
    sanitize_public_error, ExecutionFailure, ExecutionFailureKind, PromptResult,
    TurnTelemetryOutcome, EXECUTION_FAILURE_FALLBACK_MESSAGE,
};
pub use inbox::{InboxHandle, SessionInbox};
pub use queue::{MessageKind, MessageQueue, MessageSource, QueuedMessage, QueuedPayload};
pub use runtime::{
    cancel_all_agents, cancel_all_in, cancel_cascade_agents, cancel_cascade_in, AgentRuntime,
};

// ─── TurnId ──────────────────────────────────────────────────────────────────

/// Turn 唯一标识符 — UUID v7（时间有序）
///
/// 作为一次 turn 内所有事件的统一纽带。从 LlmCallStart 到 TurnCompleted 全程一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnId(uuid::Uuid);

impl TurnId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }

    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }
}

impl Default for TurnId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TurnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ─── SessionAccessPort（L5：executor 对 ACP SessionManager 的访问端口）──────

/// L5：`run_session_loop` 会话编排对 ACP `SessionManager` 的依赖端口。
///
/// 依赖反转（§0）：executor 迁入 peri-agent 后不再引用 ACP `SessionManager`
/// 类型，改为经本端口访问会话级状态（v2 MessageQueue / inbox / task manager /
/// goal / 子 agent 注册表 / cron bridge）。
/// ACP 侧 `SessionManager` 实现本端口；print mode / 测试等无 session 场景
/// 为 `None`（调用方保持原 None 语义，仅读路径可用时生效）。
pub trait SessionAccessPort: Send + Sync {
    /// 会话级共享 v2 MessageQueue（`AcpSession.v2_message_queue`）。
    /// 返回 clone（内部 Arc 共享，语义同 `SessionManager::v2_queue_for`）。
    fn v2_message_queue(&self, session_id: &str) -> Option<MessageQueue>;

    /// 会话级 SessionInbox（await-wake wrapper；lazy-init 语义由实现方保证）。
    fn session_inbox(&self, session_id: &str) -> Option<Arc<SessionInbox>>;

    /// 会话级 idle-suspended 标志（共享 Arc，executor 在 await_wake 挂起期间
    /// 置 true、醒来/取消时复位）。
    ///
    /// 宿主 `dispatch_prompt_turn` 读取此标志决定"注入 vs 排队"：turn 挂起时
    /// 用户新 prompt 直接注入 inbox（Prompt + wake）让挂起的 loop 立即醒来，
    /// 而不是在 per-session prompt lock 上阻塞至当前 turn 完成（bg 任务可能
    /// 长达数分钟，阻塞会让用户输入"石沉大海"）。
    fn idle_suspended_flag(&self, session_id: &str) -> Option<Arc<AtomicBool>>;

    /// 会话级后台任务管理器（`AcpSession.task_manager`）。
    fn task_manager(&self, session_id: &str) -> Option<Arc<dyn crate::tasks::TaskManager>>;

    /// 会话级 GoalController（`AcpSession.goal_state`）。
    fn goal_controller(&self, session_id: &str) -> Option<Arc<dyn crate::goal::GoalController>>;

    /// 构造子 agent runtime 注册闭包（`AcpSession.active_agents` insert）。
    /// 返回 None 表示无注册能力（print mode / session 不存在）。
    fn register_runtime(&self, session_id: &str) -> Option<crate::frozen::RegisterRuntimeFn>;

    /// 构造子 agent runtime 注销闭包（`AcpSession.active_agents` remove）。
    fn deregister_runtime(&self, session_id: &str) -> Option<crate::frozen::DeregisterRuntimeFn>;

    /// cancel cascade 子 agent（Cascade 判定归 Agent 层契约，本端口仅定位）。
    fn cancel_cascade_children(&self, session_id: &str);

    /// 确保 session 级 cron bridge 已启动（lazy-init，幂等；见
    /// `SessionManager::cron_bridge_for`）。
    fn cron_bridge_for(&self, session_id: &str) -> bool;

    /// 确保 session 级 MCP 订阅 inbox 已注册（lazy-init，幂等；见
    /// `SessionManager::mcp_subscription_for`）。
    ///
    /// 默认实现返回 false（print mode / 未装配端口时安全 no-op）。
    fn mcp_subscription_for(&self, _session_id: &str) -> bool {
        false
    }

    /// Bind the existing session inbox as the checked Dynamic MCP notification target.
    fn dynamic_mcp_notifications_for(&self, _session_id: &str) -> bool {
        false
    }

    /// 会话级 MCP skill 远端注册表（AcpSession 持有；发现任务写入，
    /// Skills 侧读取合并）。
    ///
    /// 默认实现返回 None（print mode / 未装配端口时安全 no-op）。
    fn mcp_skill_registry(&self, _session_id: &str) -> Option<Arc<McpSkillRegistry>> {
        None
    }

    /// 会话级命令注册表（AcpSession 持有；命令面动态注入——MCP/插件发现
    /// 结果经注册表写入，投影经 snapshot 下发）。
    ///
    /// 默认实现返回 None（print mode / 未装配端口时安全 no-op）。
    fn command_registry(&self, _session_id: &str) -> Option<Arc<CommandRegistry>> {
        None
    }
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
