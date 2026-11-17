//! ReAct v2 — 四阶段循环（RCRA）
//!
//! 每阶段有明确的类型契约（StageInput → StageOutput），可脱离完整 Agent 单独测试。
//! 阶段间依赖通过输入结构体声明，不读全局状态。
//!
//! 控制流：`Receive → Compact → Reason → Act → (回 Receive)`
//! Receive 是循环入口，也是退出判断点：队列空 + 无 idle 等待时退出。

pub mod act;
mod archive_capability;
mod async_context;
pub mod compact;
mod compact_progress;
mod context_builder;
mod invocation_context;
pub mod middleware_runner;
mod model_view;
mod null_react_llm;
mod react_loop;
use null_react_llm as null_llm;
mod queue_to_transcript;
pub mod reason;
pub mod receive;
pub use receive::{ReceiveInput, ReceiveOutput};
pub mod tool_dispatch;
pub use invocation_context::{
    ExecutionEntryOrigin, InvocationContext, ModelTurnFacts, PostHookSkipSink,
    SessionInvocationSource,
};
pub use null_llm::NullReactLLM;

pub use tool_dispatch::invocation_dispatcher;

#[cfg(test)]
use react_loop::MiddlewareChain;

pub use async_context::AsyncContext;

pub use react_loop::SharedToolMap;

pub use react_loop::LoopResult;

mod context;
mod micro_facts;
pub use context::{
    CompactContext, RuntimeServices, SessionHandle, StageContext, StageContextBuilder,
};
pub use react_loop::CompactInput;

pub use react_loop::CompactOutput;

pub use react_loop::ReasonInput;

pub use react_loop::ReasonOutput;

pub use react_loop::ActInput;

pub use react_loop::ActOutput;

// ─── 工具函数 ────────────────────────────────────────────────────────────────

pub use queue_to_transcript::append_messages_to_transcript;

#[cfg(test)]
use react_loop::enqueue_stream_interruption_continuation;

#[cfg(test)]
use react_loop::enqueue_truncation_continuation;

pub use react_loop::run_react_loop;

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "execution_policy_test.rs"]
mod execution_policy_tests;

#[cfg(test)]
#[path = "stages_test.rs"]
mod tests;

#[cfg(test)]
#[path = "truncation_test.rs"]
mod truncation_tests;

#[cfg(test)]
#[path = "budget_recovery_integration_test.rs"]
mod budget_recovery_integration_tests;

#[cfg(test)]
#[path = "default_iteration_budget_test.rs"]
mod default_iteration_budget_tests;

#[cfg(test)]
#[path = "terminal_wake_test.rs"]
mod terminal_wake_tests;

#[cfg(test)]
#[path = "bounded_wait_exit_test.rs"]
mod bounded_wait_exit_tests;

#[cfg(test)]
#[path = "model_view_test.rs"]
mod model_view_tests;

#[cfg(test)]
#[path = "micro_exposure_test.rs"]
mod micro_exposure_tests;

#[cfg(test)]
#[path = "micro_turn_exposure_test.rs"]
mod micro_turn_exposure_tests;

#[cfg(test)]
#[path = "resident_budget_test.rs"]
mod resident_budget_tests;
