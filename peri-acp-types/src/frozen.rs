//! 会话冻结数据契约（自 peri-agent 迁入；`peri-agent::session::factory` 保留 re-export）。
//!
//! ARC-FROZEN-001：会话创建时冻结日期、项目指引、skills 摘要与 system prompt；
//! 同一会话及其 SubAgent 复用冻结数据，禁止中途重新读取而改变 prompt 前缀。

use std::sync::Arc;

use crate::event::AgentEventHandler;
use crate::store::ThreadStore;

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
    /// Thread persistence store for child thread creation (None = non-persistent)
    pub store: Option<Arc<dyn ThreadStore>>,
    /// Parent thread ID for child thread hierarchy (None = top-level agent)
    pub parent_thread_id: Option<String>,
    /// Register callback: called when a child agent starts executing.
    pub register_runtime: Option<RegisterRuntimeFn>,
    /// Deregister callback: called when a child agent finishes.
    pub deregister_runtime: Option<DeregisterRuntimeFn>,
}
