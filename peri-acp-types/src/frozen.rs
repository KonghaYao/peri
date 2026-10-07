//! 会话冻结数据契约（自 peri-agent 迁入；`peri-agent::session::factory` 保留 re-export）。
//!
//! ARC-FROZEN-001：会话创建时冻结日期、项目指引、skills 摘要与 system prompt；
//! 同一会话及其 SubAgent 复用冻结数据，禁止中途重新读取而改变 prompt 前缀。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::event::AgentEventHandler;

/// 会话创建时冻结的运行环境快照（H3）。
///
/// 首次内容准入时从**选定执行/工具环境**取得一次（本地会话即计算宿主；远端
/// 执行环境必须由其自身提供），随后随 `FrozenContext` 与版本化 snapshot
/// 持久化。主重渲染、子 Agent、fork 与 workflow 只消费该快照，不再在调用时
/// 重新探测（ARC-FROZEN-001：prompt 前缀不得因 `.git` 出现/消失或探测器差异
/// 而漂移）。
///
/// 旧 snapshot 没有该字段时解码为 `None` = unavailable：需要派生新 prompt 时
/// 显式标记限制，**不得**用当前计算宿主的探测值冒充历史/远端环境。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenRuntimeEnv {
    /// 执行环境平台标识（`std::env::consts::OS` 口径）。
    pub platform: String,
    /// 执行环境 OS 版本描述。
    pub os_version: String,
    /// 冻结时刻该执行目录是否位于 Git 仓库内（含向上查找 `.git` 的语义）。
    pub is_git_repo: bool,
}

/// 子 Agent event handler 工厂：child_thread_id → child 专属 handler。
pub type ChildHandlerFactory = Arc<dyn Fn(String) -> Arc<dyn AgentEventHandler> + Send + Sync>;

/// Register callback: (thread_id, cancel_token, cancel_policy_str) → ()
pub type RegisterRuntimeFn =
    Arc<dyn Fn(String, tokio_util::sync::CancellationToken, String) + Send + Sync>;

/// Deregister callback: &str (thread_id) → ()
pub type DeregisterRuntimeFn = Arc<dyn Fn(&str) + Send + Sync>;

/// 会话级冻结数据（session/new 一次性捕获，后续轮次直接复用）。
///
/// 零跨依赖分组：四个字段在链装配与 SubAgent 构造中独立使用，
/// 不与其它字段共享 mutable state。
#[derive(Clone)]
pub struct FrozenData {
    /// Frozen CLAUDE.md content (None = read from disk each turn, legacy).
    pub claude_md: Option<String>,
    /// Frozen CLAUDE.local.md content.
    pub claude_local_md: Option<String>,
    /// Frozen skills summary (None = scan each turn).
    pub skill_summary: Option<String>,
    /// Frozen session date in YYYY-MM-DD (None = compute fresh each turn).
    pub date: Option<String>,
}

/// 子 Agent 线程持久化分组（零跨依赖）。
#[derive(Clone, Default)]
pub struct ThreadPersistence {
    /// 会话资源门面：child 保存/状态/历史写入的唯一入口（None = 不持久化）
    pub session_resources: Option<Arc<dyn crate::session_resources::SessionResources>>,
    /// Parent thread ID for child thread hierarchy (None = top-level agent)
    pub parent_thread_id: Option<String>,
    /// Register callback: called when a child agent starts executing.
    pub register_runtime: Option<RegisterRuntimeFn>,
    /// Deregister callback: called when a child agent finishes.
    pub deregister_runtime: Option<DeregisterRuntimeFn>,
}
