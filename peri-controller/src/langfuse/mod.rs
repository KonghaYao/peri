//! Langfuse tracing integration.
//!
//! Provides session-level and turn-level tracing via Langfuse API.
//! Trace → Span → Generation hierarchy captures full agent execution.
//!
//! `metric_sink.rs` 同时承载 `peri-agent::metrics` 的出口：指标事件
//! 以 Langfuse event 观测上报（不占用 trace/span/generation 类型）。

pub mod bridge;
pub mod config;
pub mod drop_telemetry;
pub mod fake_session;
mod metric_sink;
pub mod session;
pub mod session_like;
pub mod tracer;
mod turn_traces;

pub use config::LangfuseConfig;
pub use metric_sink::LangfuseMetricsSink;
pub use session::{LangfuseSession, LangfuseShutdownOwner, LangfuseShutdownReport};
pub use session_like::LangfuseSessionLike;
pub use turn_traces::TurnTraceRegistry;
// TODO: Phase 5 引入 fake session 测试后将移除此 allow
#[allow(unused_imports)]
pub(crate) use fake_session::FakeLangfuseSession;
pub use tracer::LangfuseTracer;
