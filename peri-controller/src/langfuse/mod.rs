//! Langfuse tracing integration.
//!
//! Provides session-level and turn-level tracing via Langfuse API.
//! Trace → Span → Generation hierarchy captures full agent execution.

pub mod bridge;
pub mod config;
pub mod drop_telemetry;
pub mod fake_session;
pub mod session;
pub mod session_like;
pub mod tracer;

pub use config::LangfuseConfig;
pub use session::LangfuseSession;
pub use session_like::LangfuseSessionLike;
// TODO: Phase 5 引入 fake session 测试后将移除此 allow
#[allow(unused_imports)]
pub(crate) use fake_session::FakeLangfuseSession;
pub use tracer::LangfuseTracer;
