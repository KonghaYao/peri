//! 子 Agent 创建统一入口（3.0 L3 迁移）。
//!
//! L3 归位：subagent 创建逻辑（建 thread / 建 session / 运行 + 收尾）自
//! `peri-middlewares/src/subagent/`（spawner / execute_fork / execute_bg /
//! build_agent 四条路径 + ACP `/bg` 命令）收敛至 [`spawn_subagent`]。
//! Middleware 只声明工具与发起意图（组装 [`SubagentSpawnConfig`]），
//! 不持有创建实现。
//!
//! 依赖方向：Agent 层不反向依赖 middlewares。子链装配经
//! [`SubagentChainAssembler`] trait 依赖反转（中间件层提供实现，
//! 链序 AgentsMd→Skills→[SkillPreload]→Todo 由实现方保持，ARC-MIDDLEWARE-001）；
//! 生命周期 hook 触发经 [`SubagentLifecycleStart`]/[`SubagentLifecycleStop`]
//! 闭包注入（middlewares 构造闭包，内部触发其 RegisteredHook）。
//!
//! 验收语义：
//! - subagent 必有持久化 thread（parent_thread_id 父子链；transcript 绑定
//!   `with_persistence`，thread_id = agent_id）；
//! - frozen data 从父 session copy（parent 为 Some 时 claude_md / skill_summary /
//!   date 取自 `parent.store().frozen`，不重新读取磁盘）；
//! - agent_status 收尾语义与迁移前一致：done / cancelled / error。

mod background;
mod directives;
mod factory;
mod lifecycle;
mod run_sync;
mod types;
mod util;
mod v2_bridge;

pub use directives::{build_bg_fork_directive, build_fork_directive, build_prediction_directive};
pub use factory::SessionFactory;
pub(crate) use lifecycle::{
    on_subagent_stop_handler, BgCleanupGuard, BgStopEmitV2, DeregisterGuard,
};
pub use types::{
    ForkDirectiveKind, SubagentCancelPolicy, SubagentChainAssembler, SubagentChainContext,
    SubagentHost, SubagentLifecycleStart, SubagentLifecycleStop, SubagentResumeConfig,
    SubagentRunMode, SubagentSpawnConfig, SubagentSpawned,
};
pub use util::{count_tool_calls_from_session, extract_last_ai_text, format_subagent_result};
pub use v2_bridge::{
    agent_id_from_child_thread, build_v2_subagent_context, DefaultSubagentV2ContextBuilder,
    SubagentV2ContextBuilder, V2SubagentContext,
};
pub(crate) use v2_bridge::{
    build_subagent_start_v2, build_subagent_stop_v2, emit_subagent_start_v2, emit_subagent_stop_v2,
};

#[cfg(test)]
use crate::agent::async_tasks::{
    BackgroundTask, BackgroundTaskStatus, BgCancelHandle, BgTaskKind, TaskManager,
};
#[cfg(test)]
use crate::agent::react::ReactLLM;
#[cfg(test)]
use crate::messages::BaseMessage;
#[cfg(test)]
use crate::middleware::chain::MiddlewareChain;
#[cfg(test)]
use crate::session::{FrozenContext, Session};
#[cfg(test)]
use crate::thread::{ThreadMeta, ThreadStore};
#[cfg(test)]
use peri_acp_types::identity::AgentId;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

#[cfg(test)]
#[path = "subagent_test.rs"]
mod tests;
