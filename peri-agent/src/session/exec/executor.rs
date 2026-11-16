//! Shared prompt execution logic（L5：自 `peri-acp/src/host/exec/executor.rs`
//! 物理迁入，ACP 侧 `crate::session::executor` 薄壳 re-export 保兼容）。
//!
//! Provides [`run_session_loop`] which encapsulates the common agent execution
//! pipeline used by both TUI (via `TransportEventSink`) and stdio (via
//! `StdioEventSink`) paths.
//!
//! Compact 由 v2 `stages/compact.rs`（`run_react_loop` 在每轮开头调
//! `compact_v2::run_compact`）统一处理，不再需要外层 loop + resubmit，
//! 也不再经过 CompactMiddleware。
//!
//! # 文件结构（EXECUTOR-SPLIT 选项 B + L5 迁出）
//!
//! 本文件是 orchestrator，仅保留：
//! - 共享类型：`PromptStopReason` / `PromptResult` / `FrozenSessionData`
//!   / `SessionContext` / `TurnInput` / `TurnConfig` / `LangfuseHooks`
//! - 入口：`run_session_loop`（编排）+ `build_and_execute_agent`（cfg 组装与 v2 dispatch）
//! - Prediction facade：`execute_prediction` / `extract_prediction_text`
//!
//! 子流程已随 L5 迁入本 crate `session::exec::executor_helpers`：
//! - [`intercept_immediate_command`]：slash 命令拦截
//! - [`spawn_event_pump`]：后台事件泵 + Langfuse tracer（注入闭包）
//! - [`build_and_execute_agent_v2`]：v2 stages 装配与 ReAct 循环驱动（9 个 phase）
//! - [`collect_result`] / [`close_channel`] / [`wait_for_pump`]：结果收集
//!
//! 本模块经下方 use 块把 helper 提升到本模块命名空间，使 `executor_test.rs`
//! 的 `super::{intercept_immediate_command, InterceptRequest}` 路径继续可解析。
//!
//! # 依赖反转（§0）
//!
//! 本模块只依赖 peri-acp-types / peri-model / crate 内部：
//! - `provider` / `peri_config` / `AgentPool` / `SessionManager` / `Controller`
//!   五个 ACP 特有字段端口化为投影值 + 注入闭包 + [`SessionAccessPort`](peri_acp_types::session::SessionAccessPort) /
//!   [`EventPublisher`](peri_acp_types::event::EventPublisher) / [`EventSubscriber`](peri_acp_types::event::EventSubscriber) 端口（ACP 宿主装配面构造）；
//! - 事件发射/订阅经契约端口（[`EventPublisher`](peri_acp_types::event::EventPublisher) / [`EventSubscriber`](peri_acp_types::event::EventSubscriber) 适配层
//!   在 ACP 宿主侧），命令拦截 / stage 装配 / Langfuse / cancel cascade
//!   全部经注入面接入；
//! - Langfuse 遥测经 [`LangfuseHooks`]（on_turn_start / on_turn_end /
//!   bridge_factory 闭包，ACP 宿主从 `LangfuseSession` 构造）；
//! - stage 装配桥（[`StageBuildFn`]）与 EventBus forwarder 启动器
//!   （[`ForwarderLauncherFn`]）由 ACP 宿主构造后经 [`TurnInput`] 注入。
//!
//! ## Cancel 语义保持
//!
//! - `intercept_immediate_command` 内的 `tokio::select!` 分支顺序原样保留
//!   （`cmd.execute` 与 `cancel.cancelled()` 仍按原 biased 顺序，二者均触发 push_done）
//! - `build_and_execute_agent_v2` 末尾的 cancel cascade 仍在循环失败后触发，
//!   `LoopResult::Error` 分支先发 `AgentExecutionFailed` 事件再判断 stop_reason
//! - `collect_result` 严格 "close → wait_for_pump(10s timeout) → drain recall"；
//!   泵未送达终态时本轮按失败结算（done 缺失不能报告完成）

use std::sync::Arc;

use tokio::sync::oneshot as exec_oneshot;

use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::messages::ContentBlock;
use peri_acp_types::messages::MessageContent;

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::session::ExecutionFailure;
use peri_acp_types::tasks::BgTaskKind;

use crate::agent::react::AgentInput;
use crate::session::async_router::AsyncRouter;

mod input_recalls;

// 子流程 helper 同 crate 迁移（`session::exec::executor_helpers`）：
// intercept_immediate_command / InterceptRequest / spawn_event_pump /
// SpawnPumpRequest / PumpHandle / collect_result / CollectRequest /
// close_channel / wait_for_pump / build_and_execute_agent_v2 /
// V2ExecuteRequest / StageBuildFn / ExecOutcome ——
// executor_test.rs 通过 `super::` 访问的符号路径保持不变。
pub use crate::session::exec::executor_helpers::{
    build_and_execute_agent_v2, close_channel, collect_result, intercept_immediate_command,
    spawn_event_pump, wait_for_pump, CollectRequest, CommandLookupFn, ExecOutcome,
    ForwarderLauncherFn, InterceptOutcome, InterceptRequest, PumpHandle, SpawnPumpRequest,
    StageBuildFn, V2ExecuteRequest,
};

mod agent_build;
mod context;
mod prediction;

pub use context::{
    AutoClassifierFactory, FrozenFallbackBuilder, FrozenSessionData, LangfuseBridgeFactory,
    LangfuseHooks, LangfuseTurnEndHook, SessionContext, SubagentLlmFactory, TurnInput,
};
pub use prediction::{
    execute_prediction, extract_prediction_text, parse_prediction_actions, PredictionError,
};

/// High-level reason why prompt execution stopped, used to derive ACP `StopReason`.
///
/// L5 契约化：事实源 `peri-acp-types::command::PromptStopReason`。
pub use peri_acp_types::command::PromptStopReason;

pub use session_executor::ContinuationRequest;

/// Result of prompt execution.
///
/// L5 契约化：事实源 `peri-acp-types::session::PromptResult`。
pub use peri_acp_types::session::PromptResult;

pub use session_executor::is_keepgoing;

use session_executor::TurnConfig;

pub use session_executor::run_session_loop;

#[cfg(test)]
#[path = "executor_test.rs"]
mod tests;

#[cfg(test)]
#[path = "executor_prediction_test.rs"]
mod prediction_tests;

#[path = "executor/session_executor.rs"]
mod session_executor;
