//! peri-acp — ACP Agent Service Layer
//!
//! Provides session management, agent construction, middleware chain assembly,
//! transport abstraction (mpsc/stdio), event mapping, HITL/AskUser broker.
//! Serves both TUI (via in-memory transport) and IDE (via stdio transport)
//! frontends.
//!
//! Langfuse 观测已随 3.0 重构迁出至 `peri-controller`（事件流旁路消费者），
//! 本层仅在事件协议化前分支调用 bridge（见 `event::forwarder`）。

// 3.0 批 3（tui-deps）过渡 re-export：宿主装配点（TUI cli_print）经本面取
// Langfuse 会话句柄 trait，避免直依赖 peri-controller。L4（bridge 归位）后
// 随归位评估收敛。
pub use peri_controller::langfuse::LangfuseSessionLike;
// 过渡 re-export：telemetry 当前驻留 peri-agent；L5 从 peri-acp 移除
// peri-agent 依赖时须先给 telemetry 独立归宿（届时处理）。
pub use peri_agent::telemetry;

pub mod agent;
pub mod broker;
pub mod dispatch;
pub mod event;
pub mod host;
pub mod prompt;
pub mod provider;
pub mod session;
pub mod transport;
