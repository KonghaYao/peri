//! [`run_session_loop`](crate::session::exec::executor::run_session_loop) 的 helper 子流程（L5：自 `peri-acp/src/host/exec/executor_helpers.rs`
//! 物理迁入，ACP 侧保留 re-export 桥）。
//!
//! 本文件承载以下四个被 orchestrator 串起来的子流程：
//!
//! - [`intercept_immediate_command`]：slash 命令拦截（已注册命令直接返回，不构建 agent）
//! - [`spawn_event_pump`]：后台事件泵 + Langfuse tracer（经注入闭包）
//! - [`build_and_execute_agent_v2`]：v2 stages 装配与 ReAct 循环驱动（9 个 phase）
//! - [`collect_result`]：close channel + 等待 pump drain + recall 提取
//!
//! 共享类型（原 ACP `executor.rs` 定义）随本文件迁入：[`ExecOutcome`]。
//!
//! # 依赖反转（§0）
//!
//! 本模块只依赖 peri-acp-types / peri-model / crate 内部：
//! - 事件发射经 [`EventPublisher`](peri_acp_types::event::EventPublisher) 端口（ACP/Controller 适配层实现），
//!   事件消费经 [`EventSubscriber`](peri_acp_types::event::EventSubscriber) 端口（包装 Controller 订阅）
//! - 命令拦截经注入的 `command_lookup` 闭包（ACP 协议面注册表）+ 注入的
//!   `compact_config_loader` 闭包（`load_compact_config` 语义留在 ACP）
//! - stage 装配经注入的 `StageBuildFn`（ACP 侧从 `SessionContext` 投影
//!   `StageBuildInput` 并补齐注入面）；Langfuse tracer 由 ACP 闭包捕获，
//!   本模块不触碰观测实现
//! - cancel cascade 经注入的 `cancel_cascade` 闭包（ACP 侧 `SessionManager`）
//!
//! # Cancel 语义保持
//!
//! - `intercept_immediate_command` 显式优先处理 cancel；compact 已确认提交时
//!   恢复 durable history，提交结果不确定时返回 Internal 并要求冷恢复。
//!   命令返回路径均发送 `push_done`。
//! - `build_and_execute_agent_v2` 末尾的 cancel cascade 仍在循环失败后触发，
//!   且与 failure / `TurnEnded` 共用一次 post-flush cancel 采样的
//!   单一终态分类；顺序保持 failure 事件 → `TurnEnded` → cascade
//! - `collect_result` 严格 "close → wait_for_pump(10s timeout) → drain recall"，
//!   顺序不变（pump 必须先 close sender 才能退出 recv 循环）；泵未送达终态时
//!   本轮按失败结算，避免客户端缺 `done` 停在 loading

use peri_acp_types::command::PromptStopReason;
use peri_acp_types::session::ExecutionFailure;
use std::fmt::{self, Debug, Formatter};

use crate::agent::state::AgentState;

mod collect;
mod event_pump;
mod intercept;
mod loop_terminal;
mod v2_execute;

pub use collect::{close_channel, collect_result, wait_for_pump, CollectRequest};
pub use event_pump::{spawn_event_pump, LangfuseEndFn, PumpHandle, SpawnPumpRequest};
pub use intercept::{
    emit_command_feedback, intercept_immediate_command, CommandLookupFn, InjectPayload,
    InterceptOutcome, InterceptRequest,
};
pub use v2_execute::{
    build_and_execute_agent_v2, ForwarderLauncherFn, StageBuildFn, StageBuildRequest,
    V2ExecuteRequest,
};

use exec_outcome::done_stop_reason;

pub use exec_outcome::ExecOutcome;

pub(crate) use exec_outcome::pending_task_count;

#[cfg(test)]
#[path = "executor_helpers/event_pump_test.rs"]
mod event_pump_tests;

#[cfg(test)]
#[path = "executor_helpers/collect_test.rs"]
mod collect_tests;

#[cfg(test)]
#[path = "executor_helpers_test.rs"]
mod tests;

#[path = "executor_helpers/exec_outcome.rs"]
mod exec_outcome;
