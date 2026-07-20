//! peri-acp — ACP Agent Service Layer
//!
//! Provides session management, agent construction, middleware chain assembly,
//! transport abstraction (mpsc/stdio), event mapping, HITL/AskUser broker, and
//! Langfuse tracing. Serves both TUI (via in-memory transport) and IDE (via stdio
//! transport) frontends.

pub mod agent;
pub mod broker;
pub mod dispatch;
pub mod event;
pub mod langfuse;
pub mod prompt;
pub mod provider;
pub mod session;
pub mod transport;
