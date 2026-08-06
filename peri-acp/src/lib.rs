//! peri-acp — ACP Agent Service Layer
//!
//! Provides session management, agent construction, middleware chain assembly,
//! transport abstraction (mpsc/stdio), event mapping, HITL/AskUser broker.
//! Serves both TUI (via in-memory transport) and IDE (via stdio transport)
//! frontends.
//!
//! Langfuse 观测已随 3.0 重构迁出至 `peri-controller`（事件流旁路消费者），
//! 本层仅在事件协议化前分支调用 bridge（见 `event::forwarder`）。

pub mod agent;
pub mod broker;
pub mod dispatch;
pub mod event;
pub mod host;
pub mod prompt;
pub mod provider;
pub mod session;
pub mod transport;
