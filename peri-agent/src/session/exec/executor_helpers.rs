//! [`run_session_loop`] 的 helper 子流程（L5：自 `peri-acp/src/host/exec/executor_helpers.rs`
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
//! - 事件发射经 [`EventPublisher`] 端口（ACP/Controller 适配层实现），
//!   事件消费经 [`EventSubscriber`] 端口（包装 Controller 订阅）
//! - 命令拦截经注入的 `command_lookup` 闭包（ACP 协议面注册表）+ 注入的
//!   `compact_config_loader` 闭包（`load_compact_config` 语义留在 ACP）
//! - `/bg` fork 启动器 [`DefaultBgForkSpawner`] 的 LLM 构造 / 父工具集 /
//!   链装配器 / tool resolver 全部经 `new()` 注入（ACP 装配面构造）
//! - stage 装配经注入的 `StageBuildFn`（ACP 侧从 `SessionContext` 投影
//!   `StageBuildInput` 并补齐注入面）；Langfuse tracer 由 ACP 闭包捕获，
//!   本模块不触碰观测实现
//! - cancel cascade 经注入的 `cancel_cascade` 闭包（ACP 侧 `SessionManager`）
//!
//! # Cancel 语义保持
//!
//! - `intercept_immediate_command` 内的 `tokio::select!` 分支顺序原样保留
//!   （`handler.execute` 优先于 `cancel.cancelled()`；二者均会触发 `push_done`）
//! - `build_and_execute_agent_v2` 末尾的 cancel cascade 仍在循环失败后触发，
//!   `LoopResult::Error` 分支先发 `AgentExecutionFailed` 事件再判断 stop_reason，
//!   顺序与原实现一致
//! - `collect_result` 严格 "close → wait_for_pump(10s timeout) → drain recall"，
//!   顺序不变（pump 必须先 close sender 才能退出 recv 循环）

use peri_acp_types::command::PromptStopReason;

use crate::agent::state::AgentState;

mod bg_fork;
mod collect;
mod event_pump;
mod intercept;
mod v2_execute;

pub use bg_fork::{DefaultBgForkSpawner, ForkLlmFactory, ParentToolsFactory};
pub use collect::{close_channel, collect_result, wait_for_pump, CollectRequest};
pub use event_pump::{spawn_event_pump, LangfuseEndFn, PumpHandle, SpawnPumpRequest};
pub use intercept::{
    emit_command_feedback, intercept_immediate_command, CommandLookupFn, InterceptOutcome,
    InterceptRequest,
};
pub use v2_execute::{
    build_and_execute_agent_v2, ForwarderLauncherFn, StageBuildFn, StageBuildRequest,
    V2ExecuteRequest,
};

// ── 共享类型（L5：自 ACP executor.rs 迁入）──────────────────────────────────

/// Agent 执行后的最终输出（state + 停止原因）。
pub struct ExecOutcome {
    pub ok: bool,
    pub stop_reason: PromptStopReason,
    /// A Full Compact committed during this turn and replaced prior visible history.
    pub history_replaced_by_compaction: bool,
    pub agent_state: AgentState,
}

#[cfg(test)]
#[path = "executor_helpers_test.rs"]
mod tests;
