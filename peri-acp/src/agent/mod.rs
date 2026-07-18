//! Agent construction and lifecycle.
//!
//! Builds `AgentComponents` (middleware chain + LLM + system prompt) consumed by
//! v2 `StageContext`. Shared by TUI and ACP paths via [`build_agent`].
//!
//! Migrated from peri-tui/src/app/agent.rs:build_bare_agent().

pub mod builder;
pub mod workflow_agent;
pub use builder::*;
