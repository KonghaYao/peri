//! Langfuse 单轮追踪器（per-turn）。
#![allow(dead_code)]
//!
//! 本模块采用 Layered + Module-per-Feature 模式拆分：
//!
//! - `mod.rs`：定义唯一的 `LangfuseTracer` 状态、构造器和普通诊断事件。
//! - `turn.rs`：turn 生命周期编排与最终 flush；所有观测先同步入队。
//! - `turn_fallback.rs`：遗留 stage/generation 的收尾投影。
//! - `turn_error.rs`：稳定错误分类和未采样错误 turn 的观测投影。
//! - `event_builder.rs`：基础设施层，统一时间戳、UUID、入队与丢弃计数。
//! - `usage.rs`：TokenUsage → langfuse_usage_details 转换 + 重试 metadata 组装。
//! - `sampling.rs`：采样决策器。
//! - `stages.rs`：ReAct 5 阶段 Span 管理。
//! - `middleware.rs`：中间件链追踪器。
//! - `generation.rs`：LLM Generation 生命周期追踪器（只保存已冻结输入来源）。
//! - `input_snapshot.rs`：控制器侧输入来源适配器与冻结状态（借用 `Arc`，不做 IO）。
//! - `tool_batch.rs`：工具调用批次管理器。
//! - `registry.rs`：SubAgent 身份注册表(agent_id 查表归属,替代旧 LIFO 栈)。
//! - `compact.rs`：Compact 操作 Span 追踪器。
//! - `llm_events.rs`：Facade 的 LLM Generation 事件处理方法(`on_llm_*`)。
//! - `tool_events.rs`：Facade 的工具调用事件处理方法 + 工具批次上报(`emit_tools_flush`)。
//! - `span_events.rs`：Facade 的 Stage/Workflow/Compact/Middleware Span 事件处理方法。
//! - `subagent_events.rs`：Facade 的 SubAgent registry 入口与 AGENT obs 上报。
//!
//! 所有事件通过 session trait 的 try_add() 同步入队，保证事件顺序与调用顺序一致，
//! 确保 Langfuse 层级关系正确（父 span 先于子 span 入队）。

use std::fmt::{self, Debug, Formatter};
pub(crate) mod compact;
mod event_builder;
mod generation;
mod input_snapshot;
mod llm_events;
pub(crate) mod middleware;
pub(crate) mod registry;
mod sampling_decider;
use sampling_decider as sampling;
mod span_events;
pub mod stages;
mod subagent_events;
mod tool_batch;
mod tool_events;
mod turn;
mod turn_error;
mod turn_fallback;
mod usage;

use crate::langfuse::config::LangfuseConfig;
use crate::langfuse::session_like::LangfuseSessionLike;
use crate::langfuse::tracer::stages::StageHandle;
pub(crate) use event_builder::{new_uuid, now_rfc3339, try_add_or_warn_via_session, VERSION};
use langfuse_client::types::{EventBody, ObservationLevel};
use langfuse_client::IngestionEvent;
use peri_agent::agent::events_v2::TurnErrorReason;

pub use langfuse_tracer::LangfuseTracer;

#[cfg(test)]
#[path = "tracer_test.rs"]
mod tests;

#[cfg(test)]
mod production_observation_test;

#[path = "langfuse_tracer.rs"]
mod langfuse_tracer;
