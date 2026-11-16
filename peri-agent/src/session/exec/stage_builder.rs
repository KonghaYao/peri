//! Shared Agent builder（L5：自 peri-acp/src/host/exec/stage_builder.rs 迁入；
//! 原 `agent::builder` 全路径引用改 crate::，ACP 特有构造经注入参数接入）。
//!
//! 提供 `build_agent()` 构建函数：构造装配上下文，经 Agent 层 session 工厂
//! 构建中间件链，并产出 `AgentComponents`（供 v2 builder 消费）。
//! 链装配实现已随 L2 迁出（装配上下文 `factory::AssemblyContext` 同层，
//! 装配器经 `factory::MiddlewareChainAssembler` trait 注入——ACP 装配点
//! 传 `ProductionChainAssembler`，本模块不触碰 middlewares 实现）。
//!
//! 依赖反转（§0）：本模块只依赖 peri-acp-types / peri-model / crate 内部；
//! LLM 构造（LlmProvider / AgentPool / RetryObserver 烘焙）、system prompt
//! 渲染、Langfuse bridge、compact hooks 与 tool resolver 全部经
//! [`StageBuildInput`] 注入面接入。

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fmt::{self, Debug, Formatter};
use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::agents::AgentOverrides;
use peri_acp_types::command_registry::CommandRegistry;
use peri_acp_types::compact::CompactConfig;
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::event::AgentEventHandler;
use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::event_v2::EventBus;
use peri_acp_types::event_v2::EventBusConfig;
use peri_acp_types::event_v2::EventHandles;
use peri_acp_types::frozen::ChildHandlerFactory;
use peri_acp_types::frozen::ThreadPersistence;
use peri_acp_types::goal::GoalController;
use peri_acp_types::hooks::RegisteredHook;
use peri_acp_types::identity::AgentId;
use peri_acp_types::interaction::UserInteractionBroker;
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::plugin::LoadedPlugin;
use peri_acp_types::ports::McpPoolPort;
use peri_acp_types::ports::SessionMcpCatalogPort;
use peri_acp_types::ports::ToolSearchPort;
use peri_acp_types::session::MessageQueue;
use peri_acp_types::session::SessionInbox;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::skills::SkillRoot;
use peri_acp_types::tools::TodoItem;

use crate::agent::async_tasks::TaskManager;
use crate::agent::react::ReactLLM;
use crate::agent::stages::SharedToolMap;
use crate::agent::stages::StageContext;
use crate::agent::token::ContextBudget;
use crate::agent::LangfuseBridgeLike;
use crate::middleware::chain::MiddlewareChain;
use crate::session::exec::executor::FrozenSessionData;
use crate::session::exec::executor::SubagentLlmFactory;
use crate::session::factory::{
    AssemblyContext, ChainAssembly, MiddlewareChainAssembler, OnBgCompleteFn,
    SubAgentMiddlewarePort, SystemPromptBuilder,
};
use crate::session::retry_events::RetryEventForwarder;
use crate::session::Session;
use crate::tools::{BaseTool, ToolInvocationResolver};

mod agent;
mod stage_dependencies;
use stage_dependencies as dependencies;
mod session_setup;
mod subagent_setup;
mod tools;

pub(crate) use agent::build_agent;

pub use stage_assembly::StageBuildInput;

pub use stage_assembly::BgEventTx;

pub use stage_assembly::CachedLlmInstances;

pub(crate) use stage_assembly::AcpAgentOutput;

pub use stage_assembly::AgentComponents;

pub use stage_assembly::V2AgentOutput;

pub use stage_assembly::StageBuildError;

pub use stage_assembly::build_stage_context;

#[cfg(test)]
#[path = "stage_builder/builder_v2_test.rs"]
mod builder_v2_tests;

#[path = "stage_builder/stage_assembly.rs"]
mod stage_assembly;
