//! ACP session 注册表、外部句柄与生命周期边界。
//!
//! `SessionManager` 的 clone 共享唯一 `SessionManagerInner`；本模块保留
//! session 发布、关闭、取消与注册表访问，私有子模块分别实现初始装配、
//! capability 协商、frozen 渲染、executor 访问及 Cron/MCP 接线。
//!
//! `AcpSession` 持有会话的 active agent 句柄、收件箱、后台任务管理器与
//! bridge/projection lease；取消策略和终止执行仍委托给契约层
//! `cancel_cascade_agents` / `cancel_all_agents`。Cron bridge 跨 turn 存活，
//! session 关闭时随 owner 释放。

mod access;
pub mod agent_pool;
mod bridges;
mod caps;
pub mod command;
mod construction;
pub mod session_cron_bridge;
pub use session_cron_bridge as cron_bridge;
pub mod event_sink;
pub mod executor;
mod frozen;
pub(crate) mod frozen_snapshot;
pub mod goal_state;
pub mod retry_events;
pub mod state_builders;

// AsyncRouter（L5：物理迁入 peri-agent，仅依赖契约层；本处 re-export 桥保兼容）。
pub use peri_agent::session::async_router::AsyncRouter;

pub(crate) use frozen::{
    build_collected_sections, build_collected_sections_for_plan, derived_prompt_template,
    policy_from_settings, prepare_derived_environment,
};
pub use retry_events::RetryEventForwarder;

#[cfg(test)]
#[path = "retry_events_test.rs"]
mod retry_events_tests;

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use chrono::Utc;
use dashmap::DashMap;
use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::command::command_route::RouteEntry;
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::thread::{ThreadId, WorkState};
use tokio_util::sync::CancellationToken;

use peri_acp_types::PeriCaps;

use crate::provider::config::PeriConfig;
use crate::provider::LlmProvider;
use peri_acp_types::session::AgentRuntime;

pub use session_manager::TaskManagerFactory;

mod session_activation;
use session_activation as activation;

pub(crate) use activation::SessionActivation;

pub use session_manager::AcpSession;

pub use session_manager::SessionManager;

pub(crate) use session_manager::RecoveryDelivery;

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[path = "session_manager.rs"]
mod session_manager;
